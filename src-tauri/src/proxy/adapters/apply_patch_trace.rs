//! apply_patch conversion decision tracing (data source for the diagnostics traffic viewer's "apply-patch" page).
//!
//! ## Why this exists (vs. forward-trace)
//!
//! forward-trace captures the **raw protocol body** (Codex's original request / converted
//! request sent upstream / upstream response), but can't see the adapter's **intermediate
//! decisions** when it repackages an upstream `apply_patch` tool call into the Codex
//! `custom_tool_call` wire format: what the original function args looked like, the extracted
//! V4A text, what the envelope repair changed, the JSON/V4A truncation detection result, the
//! post-hoc V4A syntax validation verdict, the final completed/incomplete decision.
//! These are exactly the steps that matter most when iterating on the apply_patch module
//! (extract / repair / validate), so this is a dedicated trace outlet shared by
//! [`crate::responses`] / [`crate::gemini_native`].
//!
//! ## Why a sink hook instead of calling trace_store directly
//!
//! `trace_store` lives in `crates/proxy`, while this crate (`adapters`) is a dependency of
//! proxy -- a reverse `use` would create a **circular dependency**. So this module only defines
//! a process-level sink hook: at proxy startup (`build_router`) a closure is registered that
//! takes the diagnostic `Value` built here, fills in `seq`/`captured_at`, and pushes it into
//! `trace_store` (`TraceKind::ApplyPatch`). This follows the same "outer layer fills in seq"
//! approach as the cat-webfetch subprocess's `POST /api/ingest`, just as an in-process closure
//! with no cross-process hop needed.
//!
//! ## Overhead / off by default
//!
//! The gate points at `proxy::diagnostics::forward_trace_enabled` (env `CAS_DIAG_TRACE` or the
//! in-app "diagnostics mode" toggle, off by default). When unregistered / off, [`emit`] costs
//! one `OnceLock` load plus one atomic read, **and builds no `Value` at all** (the `build`
//! closure only runs when enabled) -- the same "zero cost when off" contract as forward-trace.
//! Same positioning as forward-trace: local developer diagnostics, patch bodies (code) are
//! recorded verbatim and not redacted, loopback only, off by default, never enabled for
//! end users in a release.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

/// Per-record cap on args/input text size on disk (guards against one giant patch blowing up a single trace record). Truncated beyond this, with `truncated_bytes` noted.
const MAX_FIELD_BYTES: usize = 256 * 1024;

/// Cap on "sent, awaiting result" apply_patch call_ids (leak guard: if some call never gets a
/// result, the oldest is evicted once over the cap). Steady-state in-flight apply_patch calls
/// are rare, 512 is comfortably enough.
const PENDING_CAP: usize = 512;

/// Process-level trace hook. `gate` decides whether to collect (points at proxy's diagnostics
/// master switch), `sink` delivers the constructed diagnostic `Value` to trace_store (the
/// closure registered by proxy fills in seq before pushing).
struct Hook {
    gate: fn() -> bool,
    sink: Box<dyn Fn(Value) + Send + Sync>,
}

static HOOK: OnceLock<Hook> = OnceLock::new();

/// Registered once at proxy startup (`OnceLock`, a second call is silently ignored -- process-level singleton).
/// - `gate`: returns "is diagnostics collection currently on", pass `proxy::diagnostics::forward_trace_enabled`.
/// - `sink`: receives one constructed apply_patch diagnostic `Value` (no `seq`/`captured_at`
///   yet), which proxy fills in before pushing into `trace_store` (`TraceKind::ApplyPatch`).
pub fn install(gate: fn() -> bool, sink: Box<dyn Fn(Value) + Send + Sync>) {
    let _ = HOOK.set(Hook { gate, sink });
}

/// Whether apply_patch tracing is currently on (unregistered -> false). Callers can check the gate before building expensive fields.
pub fn enabled() -> bool {
    HOOK.get().map(|h| (h.gate)()).unwrap_or(false)
}

/// Input for one apply_patch conversion decision (all references, only serialized into a `Value` when collection is on).
pub struct ApplyPatchTrace<'a> {
    /// Conversion source path: `"chat"` (responses/converter.rs) / `"gemini_native"`.
    pub source: &'a str,
    /// Upstream model name (converter's `self.model` / gemini's `self.model`), since apply_patch behavior varies by model.
    pub model: &'a str,
    /// Codex wire `call_id` (correlates with the tool result being fed back).
    pub call_id: &'a str,
    /// Codex wire item id (`fc_*`).
    pub fc_id: &'a str,
    /// The **raw** function arguments returned by upstream (standard shape is
    /// `{"input":"*** Begin Patch..."}`, but may also be bare V4A / an alias key / a truncated fragment).
    pub args_raw: &'a str,
    /// The V4A text actually sent to Codex, after `extract_apply_patch_input` extraction plus `repair_v4a_envelope` repair.
    pub input: &'a str,
    /// Whether the stream was interrupted (chat: no finish_reason and not `[DONE]`; gemini is non-incremental, always false).
    pub interrupted: bool,
    /// JSON structural truncation detection result (`detect_json_truncation`; not applicable to the gemini path, pass None).
    pub json_truncation: Option<&'a str>,
    /// V4A envelope truncation detection result (`detect_v4a_truncation`; not applicable to the gemini path, pass None).
    pub v4a_truncation: Option<&'a str>,
    /// Post-hoc V4A syntax validation failure (`validate_v4a_syntax`): `(line number, human-readable message)`.
    pub v4a_validation: Option<(usize, &'a str)>,
    /// Final decision: `"completed"` (emit input.delta+done, write to cache) or `"incomplete"`
    /// (emit status=incomplete, skip input.done, don't write to cache, to avoid a destructive half-apply).
    pub decision: &'a str,
    /// Pre-flight auto-repair record (the output of `apply_patch_preflight::repairs_to_value`): the result of the on-disk
    /// comparison for each `Update File` (repaired / clean / skipped). Pass `None` when there were no repairs.
    pub repairs: Option<&'a Value>,
}

/// When collection is on, builds the diagnostic `Value` (phase=`call`) and delivers it via sink; zero-cost return when off.
/// A completed call **registers its call_id in pending**, to be paired up when its result comes
/// back in a later request round via [`emit_result`] (an incomplete call is never executed by
/// Codex and never gets a result, so it isn't registered).
pub fn emit(trace: &ApplyPatchTrace) {
    let Some(hook) = HOOK.get() else { return };
    if !(hook.gate)() {
        return;
    }
    (hook.sink)(build_value(trace));
    if trace.decision == "completed" {
        register_pending(trace.call_id);
    }
}

/// When collection is on, emits a phase=`result` diagnostic for one apply_patch **result being fed back**
/// (the `custom_tool_call_output` Codex stuffs back to the model after applying). `output` is the raw fed-back value (string or content_items array).
///
/// **Dedup + precision**: the request side replays the full history every round (the same
/// call_id's result reappears in every subsequent request round), so this only emits and
/// removes on the call_id's **first** hit against pending (= a completed apply_patch call we
/// previously emitted); duplicate results from history replay, and results for non-apply_patch
/// custom tools, never hit -> skipped. A retry gets a new call_id, paired independently.
/// Zero-cost when sink is off / unregistered (gate checked before the pending lookup).
pub fn emit_result(call_id: &str, output: &Value) {
    let Some(hook) = HOOK.get() else { return };
    if !(hook.gate)() {
        return;
    }
    if !take_pending(call_id) {
        return;
    }
    let text = match output {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    (hook.sink)(build_result_value(call_id, &text));
}

// ─────────────────────────────────────────────────────────────────────────────
// MOC-263 P3: shell write-to-disk trace. A model may use `exec_command` (shell) to run
// sed -i / cat> / echo> / python write / `apply_patch <<EOF` etc. to **edit files directly**,
// bypassing structured apply_patch (so it goes through neither the preflight double-check nor
// the apply_patch trace above). This module recognizes write-to-disk commands when the
// converter handles an exec_command-class tool call and emits a `trace_kind:"shell_edit"`
// diagnostic (through the same sink/gate), so the diagnostics page / jsonl can see "how many
// edits bypassed apply_patch", supporting a deeper phase-2 analysis of shell file-editing
// behavior. **Observation only, does not intercept or modify the command.**
// ─────────────────────────────────────────────────────────────────────────────

/// Tool names treated as shell execution (their args may contain a file-editing command).
pub fn is_shell_exec_tool(name: &str) -> bool {
    matches!(
        name,
        "exec_command" | "shell" | "execute_command" | "local_shell" | "container.exec"
    )
}

/// Strips fd->/dev noise redirects (`2>/dev/null` / `>/dev/null` / `2>&1` / `&>/dev/null`) out
/// of a shell string, so they aren't mistaken for a write-to-disk `>`.
fn strip_dev_redirects(cmd: &str) -> String {
    let mut out = cmd.to_owned();
    for pat in [
        "2>/dev/null",
        "1>/dev/null",
        ">/dev/null",
        "&>/dev/null",
        "2>&1",
        "2> /dev/null",
        "> /dev/null",
    ] {
        out = out.replace(pat, " ");
    }
    out
}

/// Roughly splits a shell string on subcommand boundaries (newline / `;` / `|` / `&&` / `||` /
/// `&`). All separators are ASCII, so slicing by byte offset never cuts a UTF-8 sequence
/// (an ASCII byte can never be part of a multi-byte sequence).
fn split_subcommands(cmd: &str) -> Vec<&str> {
    let b = cmd.as_bytes();
    let mut segs = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'\n' || b[i] == b';' || b[i] == b'|' {
            segs.push(&cmd[start..i]);
            start = i + 1;
            i += 1;
        } else if b[i] == b'&' && b.get(i + 1) == Some(&b'&') {
            segs.push(&cmd[start..i]);
            start = i + 2;
            i += 2;
        } else if b[i] == b'&' {
            segs.push(&cmd[start..i]);
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    segs.push(&cmd[start..]);
    segs
}

/// Whether the command word `w` appears as a standalone token in the subcommand (rough check, tolerates a leading `cd`/assignment already having been split off).
fn has_word(s: &str, w: &str) -> bool {
    s.split(|c: char| c.is_whitespace()).any(|t| t == w)
}

/// Whether an "edit in place" flag is present (`sed -i` / `perl -pi` / `--in-place`).
fn has_inplace_flag(s: &str) -> bool {
    s.split(|c: char| c.is_whitespace()).any(|t| {
        t == "--in-place"
            || t.starts_with("--in-place=")
            || (t.starts_with('-') && !t.starts_with("--") && t.len() > 1 && t[1..].contains('i'))
    })
}

/// Whether the subcommand redirects output into a **real file** (still contains `>` after dev noise has been stripped).
fn redirects_to_file(s: &str) -> bool {
    s.contains('>')
}

/// Whether the first token is a read-only-class command (awk/grep/rg/find/diff/sort/jq/`sed -n`) -- used to exclude generic redirect-write false positives.
fn starts_with_reader(s: &str) -> bool {
    let t = s.trim_start();
    let first = t.split(|c: char| c.is_whitespace()).next().unwrap_or("");
    matches!(
        first,
        "awk" | "grep" | "rg" | "find" | "diff" | "comm" | "sort" | "jq"
    ) || (has_word(s, "sed") && t.contains(" -n"))
}

/// Whether a single redirect target is an archive/compression artifact (`*.tar.gz`/`.tgz`/`.zip`/`.gz`/`.bz2`/`.xz`/`.tar`).
fn target_is_artifact(target: &str) -> bool {
    let t = target.trim_matches(|c| c == '"' || c == '\'');
    [".tar.gz", ".tgz", ".zip", ".gz", ".bz2", ".xz", ".tar"]
        .iter()
        .any(|ext| t.ends_with(ext))
}

/// Whether **every** redirect target in the subcommand is an archive/compression artifact (and there is at least one redirect). Used for the `redirect_write` exemption:
/// exempt only when "everything written is a download/packaging artifact"; if **any**
/// redirect target is a real, non-archive workspace file, it is **not** exempt (the audit
/// signal is preserved). **Judged per target, not per command**: `curl … > x.tar.gz` is
/// exempt, but `curl … > src/generated.rs` still counts; and **every** redirect is checked
/// individually -- `tool 2>err.gz > src.rs` can't be exempted just because the first
/// `2>err.gz` looks like an archive, missing the real `> src.rs`
/// (chatgpt-codex-connector review: inspect every redirection).
fn all_redirect_targets_are_artifacts(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut saw = false;
    while i < b.len() {
        if b[i] == b'>' {
            let mut j = i + 1;
            if j < b.len() && b[j] == b'>' {
                j += 1; // `>>` append
            }
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let start = j;
            while j < b.len() && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            saw = true;
            if !target_is_artifact(&s[start..j]) {
                return false; // one non-archive real-file target -> not exempt
            }
            i = j;
        } else {
            i += 1;
        }
    }
    saw
}

/// Recognizes "write to disk / edit file" operations in a shell string (used for MOC-263 P3 diagnostics). Returns the matched kinds (can be several);
/// purely read-only (git/ls/grep/cargo/cat reads, etc.) returns empty. Better to under-report than over-report: only recognizes clear write-to-disk shapes.
pub fn classify_shell_write(cmd: &str) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = Vec::new();
    let push = |k: &'static str, v: &mut Vec<&'static str>| {
        if !v.contains(&k) {
            v.push(k);
        }
    };
    // Whole-string level: heredoc / -c shapes (subcommand splitting would break a heredoc body, so check the whole string).
    if cmd.contains("apply_patch") && (cmd.contains("<<") || cmd.contains("*** Begin Patch")) {
        push("apply_patch_via_shell", &mut kinds);
    }
    if (has_word(cmd, "python") || has_word(cmd, "python3"))
        && (cmd.contains("<<") || cmd.contains(" -c"))
        && (cmd.contains(".write(")
            || cmd.contains("write_text")
            || cmd.contains("writelines")
            || (cmd.contains("open(")
                && (cmd.contains("'w'")
                    || cmd.contains("\"w\"")
                    || cmd.contains("'a'")
                    || cmd.contains("\"a\"")
                    || cmd.contains("'w+'")
                    || cmd.contains("'x'"))))
    {
        push("python_write", &mut kinds);
    }
    if has_word(cmd, "node")
        && (cmd.contains("writeFileSync")
            || cmd.contains("createWriteStream")
            || cmd.contains("fs.write"))
    {
        push("node_write", &mut kinds);
    }
    // Subcommand level: in-place edit / redirect write.
    let normalized = strip_dev_redirects(cmd);
    for seg in split_subcommands(&normalized) {
        let s = seg.trim();
        if s.is_empty() {
            continue;
        }
        if has_word(s, "sed") && has_inplace_flag(s) {
            push("sed_inplace", &mut kinds);
        } else if has_word(s, "perl") && has_inplace_flag(s) {
            push("perl_inplace", &mut kinds);
        } else if has_word(s, "tee") {
            push("tee_write", &mut kinds);
        } else if has_word(s, "truncate") {
            push("truncate", &mut kinds);
        } else if redirects_to_file(s)
            && !starts_with_reader(s)
            && !all_redirect_targets_are_artifacts(s)
        {
            // echo>/cat>/printf> and generic `prog > file` (with awk/grep/sed -n etc. read-only left sides already excluded).
            // **Exempt per target, and check every redirect individually**: exempt only when
            // **all** redirect targets are archive artifacts (`> x.tar.gz`); downloading/writing
            // to a real project file (`curl … > src/x.rs`), or a real file mixed into multiple
            // redirects (`tool 2>err.gz > src.rs`), still counts -- that's a genuine bypass of
            // apply_patch editing the workspace, and it must stay visible to the audit
            // (MOC-268, chatgpt-codex-connector review).
            push("redirect_write", &mut kinds);
        }
    }
    kinds
}

/// Extracts the shell command text from an exec_command tool's args (`{"cmd":"..."}` / aliases).
fn extract_shell_cmd(args_raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(args_raw.trim()).ok()?;
    let obj = v.as_object()?;
    for k in ["cmd", "command", "script", "input"] {
        if let Some(val) = obj.get(k) {
            if let Some(s) = val.as_str() {
                return Some(s.to_owned());
            }
            if let Some(arr) = val.as_array() {
                let joined: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect();
                if !joined.is_empty() {
                    return Some(joined.join(" "));
                }
            }
        }
    }
    None
}

/// When collection is on and this exec_command is a write-to-disk command, emits a `shell_edit`
/// diagnostic (the model used shell to edit files directly, bypassing structured apply_patch).
/// Otherwise returns at zero cost (gate first, then extract+classify). `tool` is the tool name,
/// `args_raw` is the raw tool arguments. Observation only.
pub fn emit_shell_edit(
    source: &str,
    model: &str,
    call_id: &str,
    fc_id: &str,
    tool: &str,
    args_raw: &str,
) {
    let Some(hook) = HOOK.get() else { return };
    if !(hook.gate)() {
        return;
    }
    let Some(cmd) = extract_shell_cmd(args_raw) else {
        return;
    };
    let kinds = classify_shell_write(&cmd);
    if kinds.is_empty() {
        return;
    }
    (hook.sink)(build_shell_edit_value(
        source, model, call_id, fc_id, tool, &cmd, &kinds,
    ));
}

/// Builds the `shell_edit` diagnostic `Value` (seq/captured_at filled in by the proxy sink). `pub(crate)` for tests.
pub(crate) fn build_shell_edit_value(
    source: &str,
    model: &str,
    call_id: &str,
    fc_id: &str,
    tool: &str,
    cmd: &str,
    kinds: &[&str],
) -> Value {
    let (cmd_text, cmd_trunc) = cap_field(cmd);
    json!({
        "trace_kind": "shell_edit",
        "phase": "call",
        "source": source,
        "model": model,
        "call_id": call_id,
        "fc_id": fc_id,
        "tool": tool,
        "bypass": "apply_patch",
        "write_kinds": kinds,
        "cmd": {
            "len": cmd.len(),
            "truncated_bytes": cmd_trunc,
            "text": cmd_text,
        },
    })
}

/// Builds one [`ApplyPatchTrace`] into a diagnostic `Value` (for the viewer / jsonl). `seq`/`captured_at`/
/// `proxy_version` are filled in by the sink proxy registers (which has access to `next_seq` plus the version number). `pub(crate)` for tests.
pub(crate) fn build_value(t: &ApplyPatchTrace) -> Value {
    let (args_text, args_trunc) = cap_field(t.args_raw);
    let (input_text, input_trunc) = cap_field(t.input);
    let mut reasons: Vec<&str> = Vec::new();
    if t.interrupted {
        reasons.push("interrupted");
    }
    if t.json_truncation.is_some() {
        reasons.push("json_truncated");
    }
    if t.v4a_truncation.is_some() {
        reasons.push("v4a_truncated");
    }
    if t.v4a_validation.is_some() {
        reasons.push("v4a_invalid");
    }
    json!({
        "trace_kind": "apply_patch",
        "phase": "call",
        "source": t.source,
        "model": t.model,
        "call_id": t.call_id,
        "fc_id": t.fc_id,
        "decision": t.decision,
        "extraction": classify_extraction(t.args_raw, t.input),
        "incomplete_reasons": reasons,
        "repairs": t.repairs.cloned().unwrap_or(Value::Null),
        "args": {
            "len": t.args_raw.len(),
            "truncated_bytes": args_trunc,
            "raw": args_text,
        },
        "input": {
            "len": t.input.len(),
            "truncated_bytes": input_trunc,
            "v4a": input_text,
        },
        "checks": {
            "interrupted": t.interrupted,
            "json_truncation": t.json_truncation,
            "v4a_truncation": t.v4a_truncation,
            "v4a_validation": t.v4a_validation.map(|(line, message)| json!({
                "line": line,
                "message": message,
            })),
        },
    })
}

/// Builds one apply_patch **result being fed back** into a diagnostic `Value` (phase=`result`). `pub(crate)` for tests.
pub(crate) fn build_result_value(call_id: &str, output: &str) -> Value {
    let (text, trunc) = cap_field(output);
    json!({
        "trace_kind": "apply_patch",
        "phase": "result",
        "call_id": call_id,
        "is_error": looks_like_error(output),
        "output": {
            "len": output.len(),
            "truncated_bytes": trunc,
            "text": text,
        },
    })
}

/// Whether the apply_patch result looks like a failure (advisory -- the viewer still shows the full text for humans to judge). Matches common failure wording from the
/// Codex apply_patch handler / parse_patch; a successful output is usually a list of changed
/// files or a brief "Success". Determines whether an apply_patch result is a failure. **Must
/// not** use loose substrings like `"error"`/`"context"` -- they can match filenames
/// (`ErrorBoundary.tsx`), code (`asynccontextmanager`) and false-positive (MOC-194 live-traffic
/// seq977: `Exit code: 0 … Success … A …ErrorBoundary.tsx` was misjudged as is_error=true).
/// Signal priority: (1) explicit failure phrases (apply_patch validation failures reported
/// directly, without an Exit code wrapper) -> (2) exec-wrapped `Exit code: N` (nonzero =
/// failure) -> (3) default to not-an-error.
fn looks_like_error(output: &str) -> bool {
    let l = output.to_ascii_lowercase();
    const FAIL_PHRASES: [&str; 9] = [
        "apply_patch verification failed",
        "failed to find",
        "did not apply",
        "does not match",
        "invalid patch",
        "no such file or directory",
        "is not a valid hunk header",
        "update file hunk for path",
        "cannot operate on a completely empty file",
    ];
    if FAIL_PHRASES.iter().any(|m| l.contains(m)) {
        return true;
    }
    // The exec-wrapped `Exit code: N` is the authoritative signal (success = 0).
    if let Some(code) = parse_exit_code(output) {
        return code != 0;
    }
    false
}

/// Extracts the exit code from an exec-wrapped `Exit code: N` (present when apply_patch runs through a shell exec).
fn parse_exit_code(output: &str) -> Option<i32> {
    let idx = output.find("Exit code:")?;
    output[idx + "Exit code:".len()..]
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

// ── pending apply_patch call_id registry (call <-> result pairing + history-replay dedup) ──────────
//
// A completed apply_patch call registers its call_id; the result feed-back emits and removes it
// on first hit. Just a `Mutex<VecDeque<String>>` (only touched once per apply_patch call, a
// linear scan within 512 entries is negligible), oldest evicted once over the cap.

static PENDING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn pending() -> &'static Mutex<VecDeque<String>> {
    PENDING.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Registers a call_id that is "awaiting a result" (evicts the oldest once past [`PENDING_CAP`]). Ignores an empty id.
fn register_pending(call_id: &str) {
    if call_id.is_empty() {
        return;
    }
    if let Ok(mut q) = pending().lock() {
        // Dedup: the same call_id is not registered twice (call_id is theoretically unique; this is defensive).
        if q.iter().any(|x| x == call_id) {
            return;
        }
        q.push_back(call_id.to_owned());
        while q.len() > PENDING_CAP {
            q.pop_front();
        }
    }
}

/// If call_id is in pending, removes it and returns true (= this is the first result for an apply_patch call we emitted).
fn take_pending(call_id: &str) -> bool {
    if let Ok(mut q) = pending().lock() {
        if let Some(pos) = q.iter().position(|x| x == call_id) {
            q.remove(pos);
            return true;
        }
    }
    false
}

/// Truncates to [`MAX_FIELD_BYTES`] (on a char boundary, never cutting a UTF-8 sequence), returns (text, bytes discarded).
fn cap_field(s: &str) -> (String, usize) {
    if s.len() <= MAX_FIELD_BYTES {
        return (s.to_owned(), 0);
    }
    // Find a char boundary <= cap, to avoid cutting in the middle of a multi-byte sequence.
    let mut end = MAX_FIELD_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_owned(), s.len() - end)
}

/// Roughly classifies "how the V4A was extracted from the raw args" (for viewer summary / filtering). A lightweight re-derivation,
/// aligned with `extract_apply_patch_input`'s actual branches but not coupled to its internals:
/// - `json_input`: args is JSON and has an `input` field (standard shape).
/// - `json_alt_key`: args is JSON, has no `input`, but the input text was recovered from an alias key (patch/diff/…).
/// - `bare_v4a`: args is itself bare V4A (no JSON wrapper).
/// - `raw_fallback`: neither valid JSON nor bare-V4A-looking -> passed through as-is (usually truncation / schema drift).
pub(crate) fn classify_extraction(args_raw: &str, _input: &str) -> &'static str {
    let trimmed = args_raw.trim();
    if trimmed.is_empty() {
        return "empty";
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v) => {
            if v.get("input").and_then(Value::as_str).is_some() {
                "json_input"
            } else if v.is_object() {
                "json_alt_key"
            } else {
                "raw_fallback"
            }
        }
        Err(_) => {
            if trimmed.contains("*** Begin Patch") {
                "bare_v4a"
            } else {
                "raw_fallback"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(args: &'a str, input: &'a str) -> ApplyPatchTrace<'a> {
        ApplyPatchTrace {
            source: "chat",
            model: "qwen-test",
            call_id: "call_1",
            fc_id: "fc_1",
            args_raw: args,
            input,
            interrupted: false,
            json_truncation: None,
            v4a_truncation: None,
            v4a_validation: None,
            decision: "completed",
            repairs: None,
        }
    }

    #[test]
    fn classify_covers_the_four_paths() {
        assert_eq!(
            classify_extraction(r#"{"input":"*** Begin Patch\n*** End Patch"}"#, ""),
            "json_input"
        );
        assert_eq!(
            classify_extraction(r#"{"patch":"*** Begin Patch"}"#, ""),
            "json_alt_key"
        );
        assert_eq!(
            classify_extraction("*** Begin Patch\n*** End Patch", ""),
            "bare_v4a"
        );
        assert_eq!(classify_extraction("garbage not json", ""), "raw_fallback");
        assert_eq!(classify_extraction("   ", ""), "empty");
    }

    #[test]
    fn build_value_carries_decision_and_reasons() {
        let mut t = sample(
            r#"{"input":"*** Begin Patch\n*** End Patch"}"#,
            "*** Begin Patch\n*** End Patch",
        );
        t.decision = "incomplete";
        t.interrupted = true;
        t.v4a_validation = Some((3, "expected '*** End Patch'"));
        let v = build_value(&t);
        assert_eq!(v["trace_kind"], "apply_patch");
        assert_eq!(v["decision"], "incomplete");
        assert_eq!(v["extraction"], "json_input");
        assert_eq!(v["checks"]["v4a_validation"]["line"], 3);
        let reasons = v["incomplete_reasons"].as_array().unwrap();
        assert!(reasons.iter().any(|r| r == "interrupted"));
        assert!(reasons.iter().any(|r| r == "v4a_invalid"));
    }

    #[test]
    fn build_result_value_flags_error_and_carries_output() {
        let ok = build_result_value("call_x", "Success. Updated 1 file.");
        assert_eq!(ok["phase"], "result");
        assert_eq!(ok["call_id"], "call_x");
        assert_eq!(ok["is_error"], false);
        assert_eq!(ok["output"]["text"], "Success. Updated 1 file.");

        let err = build_result_value("call_y", "error: context does not match at line 12");
        assert_eq!(err["is_error"], true);

        // Live-traffic seq977 regression: a successful result contains the filename ErrorBoundary.tsx, must not false-positive on the "error" substring.
        let ok2 = build_result_value(
            "call_z",
            "Exit code: 0\nWall time: 0.1 seconds\nOutput:\nSuccess. Updated the following files:\nA frontend/src/components/common/ErrorBoundary.tsx\n",
        );
        assert_eq!(
            ok2["is_error"], false,
            "a successful result containing the ErrorBoundary filename should not false-positive"
        );

        // A genuine failure phrase (without an Exit code wrapper) should still be judged as an error.
        let err2 = build_result_value(
            "call_w",
            "apply_patch verification failed: Failed to find context 'uploadImage' in foo.ts",
        );
        assert_eq!(err2["is_error"], true);
    }

    #[test]
    fn pending_pairs_once_then_dedupes_replay() {
        // A unique call_id to avoid colliding with a parallel test / converter emit
        let id = "call_pending_test_unique_9af3";
        assert!(!take_pending(id), "should not hit when not registered");
        register_pending(id);
        assert!(take_pending(id), "first result should pair successfully");
        assert!(!take_pending(id), "a duplicate result from history replay should be deduped (already removed)");
    }

    #[test]
    fn classify_shell_write_flags_real_writes_only() {
        // MOC-263 P3: write-to-disk commands -> hit the corresponding kind.
        assert!(classify_shell_write("sed -i '' '199,282d' f.rs").contains(&"sed_inplace"));
        assert!(classify_shell_write("sed -i 's/a/b/' f.rs").contains(&"sed_inplace"));
        assert!(classify_shell_write("perl -pi -e 's/x/y/' f.rs").contains(&"perl_inplace"));
        assert!(classify_shell_write("echo \"}\" >> f.rs").contains(&"redirect_write"));
        assert!(classify_shell_write("cat > new.rs <<'EOF'\nx\nEOF").contains(&"redirect_write"));
        assert!(classify_shell_write("tee -a Cargo.toml").contains(&"tee_write"));
        assert!(
            classify_shell_write("python3 - <<'PY'\nopen('f','w').write(1)\nPY")
                .contains(&"python_write")
        );
        assert!(
            classify_shell_write("apply_patch <<'EOF'\n*** Begin Patch\nEOF")
                .contains(&"apply_patch_via_shell")
        );
        // Read-only commands -> empty (no false positives): pipes/reads/awk NR>=/cargo/find/running scripts.
        assert!(classify_shell_write("cd x && git log --oneline 2>/dev/null | head").is_empty());
        assert!(classify_shell_write("cat agent/issues.md 2>/dev/null | head -120").is_empty());
        assert!(classify_shell_write("cargo check 2>&1 | tail").is_empty());
        assert!(classify_shell_write("grep -rn foo src/ > /dev/null").is_empty());
        assert!(classify_shell_write("awk 'NR>=199 && NR<=240' f.rs").is_empty());
        assert!(classify_shell_write("python3 train.py --epochs 3").is_empty());
        assert!(classify_shell_write("ls -la && find . -type f").is_empty());
        assert!(classify_shell_write("cat file.rs").is_empty());
        // [MOC-268] **archive artifact target** -> does not count as redirect_write (judged per target, not per command; phase-2 false-positive fix).
        assert!(
            classify_shell_write("cd /tmp && gh api repos/o/r/tarball/main > sda.tar.gz 2>/dev/null && tar xzf sda.tar.gz")
                .is_empty(),
            "downloading to a tarball artifact should not count as redirect_write"
        );
        assert!(classify_shell_write("curl -sL https://x/y.zip > y.zip").is_empty());
        assert!(
            classify_shell_write("python3 render.py > /tmp/w/plot.tar.gz").is_empty(),
            "an archive artifact target does not count (even when the left side isn't a download)"
        );
        // [MOC-268 review] downloading/fetching into a **real project file** still counts -- that bypasses apply_patch to edit the workspace, and must stay visible to the audit
        // (chatgpt-codex-connector: don't blanket-exempt based on the gh/curl/wget command).
        assert!(
            classify_shell_write("curl -sL https://x/gen > src/generated.rs")
                .contains(&"redirect_write"),
            "curl overwriting a real project file should count as redirect_write"
        );
        assert!(
            classify_shell_write("gh api repos/o/r/contents/x > fixtures/data.json")
                .contains(&"redirect_write"),
            "gh writing a real project file should count as redirect_write"
        );
        // A genuine source-code write still hits (regression guard: don't let the exclusion overreach and miss a real edit).
        assert!(classify_shell_write("echo 'x' > src/real.rs").contains(&"redirect_write"));
        assert!(classify_shell_write("printf 'a' > config.toml").contains(&"redirect_write"));
        // [MOC-268 review] multiple redirects: check each one individually -- an earlier one that looks like an archive (`2>err.gz`) must not exempt a real file target (`> src.rs`).
        assert!(
            classify_shell_write("tool 2>err.gz > src/generated.rs").contains(&"redirect_write"),
            "an early archive decoy should not exempt a real file write"
        );
        // Exempt only when every target is an archive artifact.
        assert!(
            classify_shell_write("gh api repos/o/r/tarball/main > out.tar.gz 2>log.gz").is_empty()
        );
    }

    #[test]
    fn extract_and_build_shell_edit() {
        assert_eq!(
            extract_shell_cmd(r#"{"cmd":"sed -i '' '1d' f.rs"}"#).as_deref(),
            Some("sed -i '' '1d' f.rs")
        );
        let v = build_shell_edit_value(
            "chat",
            "glm-5.2",
            "call_1",
            "fc_1",
            "exec_command",
            "sed -i '' '1d' f.rs",
            &["sed_inplace"],
        );
        assert_eq!(v["trace_kind"], "shell_edit");
        assert_eq!(v["bypass"], "apply_patch");
        assert_eq!(v["tool"], "exec_command");
        assert!(v["write_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "sed_inplace"));
        assert!(v["cmd"]["text"].as_str().unwrap().contains("sed -i"));
    }

    #[test]
    fn cap_field_truncates_on_char_boundary() {
        let big = "あ".repeat(MAX_FIELD_BYTES); // 3 bytes each → well over cap
        let (text, trunc) = cap_field(&big);
        assert!(text.len() <= MAX_FIELD_BYTES);
        assert!(trunc > 0);
        // Didn't cut a UTF-8 sequence: can be fully re-parsed
        assert!(text.chars().all(|c| c == 'あ'));
    }
}
