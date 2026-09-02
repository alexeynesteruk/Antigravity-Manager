//! apply_patch **pre-flight auto-repair**: before sending a V4A patch to Codex for apply,
//! reads the target file and compares it, automatically aligning **safe** context mismatches
//! (trailing whitespace / leading-trailing whitespace differences), eliminating V4A's #1
//! failure mode: `apply_patch verification failed: Failed to find expected lines`.
//!
//! ## Why this is needed
//! Weaker chat models (non-OpenAI) often can't byte-for-byte reproduce the context/deletion
//! lines of an `Update File` on a large file (trailing whitespace, indentation, memory drift)
//! -> Codex can't find the anchor -> apply fails -> the model rewrites the whole file, wasting
//! time and tokens. This is exactly what live-traffic errors (rollout ground truth) showed.
//!
//! ## Safety boundary (never corrupt a file -- aligned with the user's hard rule of "no destructive downgrades")
//! - **Only touches anchors**: the context (space-prefixed) / deletion (`-`) lines inside
//!   `Update File`. `+added` lines are **never touched**.
//! - **Only repairs on a unique match**: the anchor block is searched for in the file ignoring
//!   trailing whitespace / leading-trailing whitespace, and it's aligned only when **exactly
//!   one** location matches; 0 matches (the model genuinely got the content wrong) or >=2
//!   matches (ambiguous) are always **passed through as-is**, leaving Codex's parse_patch to
//!   expose the real breakage rather than guessing.
//! - **Add File / Delete File are untouched** (no anchors, no matching involved). If the file
//!   can't be read / there's no cwd, it's passed through as-is.
//! - Every repair / pass-through is recorded on the apply-patch diagnostics page, for auditing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

/// One pre-flight processing record (for the diagnostics page / logs).
#[derive(Debug, Clone, PartialEq)]
pub struct Repair {
    /// The file path from the patch (relative, as-is).
    pub file: String,
    /// `repaired` (anchor was aligned) / `clean` (already matched exactly, unchanged) / `skipped:<reason>` (passed through unrepaired).
    pub kind: String,
    /// Human-readable detail (how many lines changed / why it was passed through).
    pub detail: String,
}

impl Repair {
    fn to_value(&self) -> Value {
        json!({"file": self.file, "kind": self.kind, "detail": self.detail})
    }
}

/// Converts a set of [`Repair`]s into a diagnostics `Value` array (for ApplyPatchTrace's `repairs` field).
pub fn repairs_to_value(repairs: &[Repair]) -> Value {
    Value::Array(repairs.iter().map(Repair::to_value).collect())
}

/// [MOC-194/MOC-263] Process-level "recently seen cwd" candidate history (most-recent-first, deduplicated, capped).
///
/// **Why this changed from a single slot to a candidate list (MOC-263 P1)**: Codex only sends
/// `<cwd>` on the turn-start request; the apply_patch tool loop's subsequent requests carry no
/// cwd -> so we rely on cross-request memory. The old implementation was a **process-level
/// single slot**, and when multiple Codex sessions run concurrently (the live-traffic norm:
/// N conversations open at once editing different projects) the single slot kept getting
/// overwritten by **another session's** turn-start cwd -> apply_patch requests fell back to
/// **a stale cwd from a different project** -> the Tier B read-from-disk rule resolved to the
/// wrong directory -> everything ended up `skipped:unreadable` (measured in phase-1: 5/5
/// fallback segments all wasted). Changed to **a candidate list of the last N distinct cwds**:
/// on read, each candidate is tried for whether `cwd/relative_path` exists, and we pick the
/// **first one that exists** (only the real project cwd has that file; a stale cwd doesn't ->
/// this auto-selects correctly). At worst, hitting a same-named file under the wrong cwd makes
/// the subsequent anchor match fail -> a safe skip, never a wrong edit (preserving "never guess, never drop").
const CWD_CANDIDATES_CAP: usize = 12;
static CWD_HISTORY: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn cwd_history() -> &'static Mutex<VecDeque<String>> {
    CWD_HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Records a recently seen cwd (dedup-and-move-to-front, evicts the oldest once over [`CWD_CANDIDATES_CAP`]). Ignores an empty string.
fn remember_cwd(cwd: &str) {
    if cwd.is_empty() {
        return;
    }
    if let Ok(mut q) = cwd_history().lock() {
        if let Some(pos) = q.iter().position(|c| c == cwd) {
            q.remove(pos);
        }
        q.push_front(cwd.to_owned());
        while q.len() > CWD_CANDIDATES_CAP {
            q.pop_back();
        }
    }
}

/// Recently seen cwd candidates (most-recent-first).
fn recall_cwd_candidates() -> Vec<String> {
    cwd_history()
        .lock()
        .map(|q| q.iter().cloned().collect())
        .unwrap_or_default()
}

/// Whether any usable cwd exists (the current request's `primary`, or a historical candidate). The byte-exact rule short-circuits on this.
fn has_cwd_candidate(primary: Option<&str>) -> bool {
    primary.map(|c| !c.is_empty()).unwrap_or(false) || !recall_cwd_candidates().is_empty()
}

/// The "anchor probe" for a patch section, each item `(is_header, text)`:
/// - context (` `) / deletion (`-`) lines with the prefix stripped -> `(false, line content)`,
///   compared against candidate files by **exact full-line match** (trim);
/// - the header text of `@@ <header>` -> `(true, header)`, compared by **substring** (a
///   truncated header is a substring of the real full line, e.g. `Architecture Overview` is a
///   substring of `## 6. Architecture Overview`). The two kinds are scored separately: an exact
///   header could be wrongly matched by a stale full line with the same text, so headers never
///   go through exact matching (chatgpt-codex-connector review). Used by [`read_patch_file`] to
///   pick the target file among same-named candidates.
fn anchor_probe<'a>(body: &[&'a str]) -> Vec<(bool, &'a str)> {
    let mut probe = Vec::new();
    for l in body {
        match l.chars().next() {
            Some(' ') | Some('-') => probe.push((false, &l[1..])),
            Some('+') => {} // added line -- not in the target file, not a probe
            _ => {
                if let Some(h) = l.strip_prefix("@@ ") {
                    let h = h.trim();
                    if !h.is_empty() {
                        probe.push((true, h));
                    }
                } else if !l.is_empty() && !l.starts_with("@@") && !l.starts_with("*** ") {
                    // An unprefixed line (the model forgot the prefix; fix_unprefixed_lines
                    // repairs it by matching the full line exactly against the file) -> use the
                    // full line as an exact probe, so the empty-probe path (when the missing
                    // prefix is the only anchor) can still pick the right file among same-named
                    // candidates (chatgpt-codex-connector review).
                    probe.push((false, l));
                }
            }
        }
    }
    probe
}

/// Resolves and reads the patch's target file against candidate cwds (MOC-263 P1 + P2).
/// `primary` (the current request's cwd, usually None for apply_patch requests) is tried
/// first, then each cwd in the recent history in turn. `probe` = the patch's context/deletion
/// anchor line content ([`anchor_probe`]): when multiple candidate cwds all have a same-named
/// relative file (concurrent sessions sharing `README.md`/`package.json` etc.), **pick the
/// candidate whose content hits the most probe anchor lines** (= the file the patch is actually
/// targeting), rather than just the first readable one (chatgpt-codex-connector review P2:
/// taking the first one would align against the wrong file). If every candidate scores 0 probe
/// hits -> none of them is the target -> return None (skip, safe). If `probe` is empty (a pure
/// add-file patch with no anchors) -> fall back to the first readable one (no way to tell, best
/// effort). An absolute path is read directly.
fn read_patch_file(
    relpath: &str,
    primary: Option<&str>,
    probe: &[(bool, &str)],
) -> Option<(PathBuf, String)> {
    let p = Path::new(relpath);
    if p.is_absolute() {
        return std::fs::read_to_string(p)
            .ok()
            .map(|c| (p.to_path_buf(), c));
    }
    // (1) A fresh primary is authoritative: if the current request has its own cwd and the file
    //    is readable -> use it directly, letting downstream decide the match (including
    //    align_at_headers' partial `@@` substring repair). **probe is only a tie-breaker among
    //    multiple same-named candidates, never a gate** -- otherwise a truncated `@@` header /
    //    a single candidate would be wrongly judged unreadable due to 0 probe hits
    //    (chatgpt-codex-connector review P2, round two).
    if let Some(c) = primary {
        if !c.is_empty() {
            let abs = Path::new(c).join(p);
            if let Ok(content) = std::fs::read_to_string(&abs) {
                return Some((abs, content));
            }
        }
    }
    // (2) Otherwise use the recent cwd candidate history (most-recent-first), reading every same-named file that exists.
    let mut readable: Vec<(PathBuf, String)> = Vec::new();
    for c in recall_cwd_candidates() {
        let abs = Path::new(&c).join(p);
        if let Ok(content) = std::fs::read_to_string(&abs) {
            readable.push((abs, content));
        }
    }
    match readable.len() {
        0 => return None,
        // A single candidate -> use it directly (downstream decides the match, giving partial
        // header substring repair a chance); not skipped just because probe hit 0.
        1 => return readable.into_iter().next(),
        _ => {}
    }
    // (3) Multiple same-named candidates (concurrent sessions sharing README.md/package.json
    //    etc.) -> pick the file the patch is actually targeting by anchor probe.
    //    Scoring: context/deletion lines (non-header) hit by **exact full-line match** (trim);
    //    `@@` headers hit the real full line by **substring** (a truncated header is a
    //    substring of the full line, e.g. `Architecture Overview` is a substring of
    //    `## 6. Architecture Overview`). Both kinds are combined into one score, and only the
    //    **unique highest score** is picked; a tie / all-0 -> None (ambiguity is never guessed,
    //    since guessing wrong would align against a stale file, violating "never guess, never
    //    drop"). Headers are excluded from exact matching: otherwise a stale full line that
    //    happens to equal the truncated header would beat the real file's substring match via
    //    exact (review).
    let probe: Vec<(bool, &str)> = probe
        .iter()
        .map(|&(h, t)| (h, t.trim()))
        .filter(|(_, t)| !t.is_empty())
        .collect();
    if probe.is_empty() {
        return readable.into_iter().next(); // no anchors (a pure add-file patch) -> most recent (no alignment needed, downstream is a no-op)
    }
    let scores: Vec<usize> = readable
        .iter()
        .map(|(_, c)| {
            let fl: Vec<&str> = c.lines().map(str::trim).collect();
            probe
                .iter()
                .filter(|&&(is_header, t)| {
                    if is_header {
                        fl.iter().any(|line| !line.is_empty() && line.contains(t))
                    } else {
                        fl.iter().any(|line| *line == t)
                    }
                })
                .count()
        })
        .collect();
    let max = scores.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return None; // no candidate contains any anchor -> none is the target -> skip (safe)
    }
    if scores.iter().filter(|s| **s == max).count() != 1 {
        return None; // a tie for highest = ambiguous -> don't guess
    }
    let best_idx = scores.iter().position(|s| *s == max).unwrap();
    Some(readable.swap_remove(best_idx))
}

/// Extracts `<cwd>...</cwd>` from a Codex Responses request (the environment_context block
/// Codex injects, shaped like `<environment_context>\n  <cwd>/abs/path</cwd>\n  <shell>zsh</shell>...`).
///
/// **Walks the Value tree** to find a string node containing `<cwd>` (its value is already the
/// serde-unescaped original text) and extracts from that -- it **must not** first
/// `serde_json::to_string(the whole request)` and search that: that would **re-escape** the
/// string values as JSON, doubling the backslashes in a Windows path `C:\Users\...` into
/// `C:\\Users\\...`, so resolve_path would get the wrong path (codex-connector #435 P2). Does
/// not depend on whether `<cwd>` lands in instructions or some input message (scans string
/// nodes at any depth).
pub fn extract_cwd(request: Option<&Value>) -> Option<String> {
    fn find_in_value(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => extract_cwd_from_str(s),
            Value::Array(a) => a.iter().find_map(find_in_value),
            Value::Object(o) => o.values().find_map(find_in_value),
            _ => None,
        }
    }
    find_in_value(request?)
}

/// Extracts `<cwd>...</cwd>` from a single (already unescaped) string.
fn extract_cwd_from_str(s: &str) -> Option<String> {
    let start = s.find("<cwd>")? + "<cwd>".len();
    let rest = &s[start..];
    let end = rest.find("</cwd>")?;
    let cwd = rest[..end].trim();
    if cwd.is_empty() {
        None
    } else {
        Some(cwd.to_owned())
    }
}

/// [MOC-194 critical] Records the request's `<cwd>` into the process-level cache. **Must be
/// called for every request** (not just apply_patch): the request carrying `<cwd>` is the
/// **turn-start request** (which produces no apply_patch and never calls [`optimize_patch`]),
/// while apply_patch shows up in **later tool-loop requests that carry no cwd**. Recording only
/// inside `optimize_patch` would mean the cwd is never learned (measured: `LAST_CWD` stayed
/// None forever, and every Tier B read-from-disk rule was a permanent no-op). So the recording
/// point must be somewhere every request passes through (the converter's
/// `with_original_request`), so the turn-start cwd can be fallen back to by later apply_patch requests.
pub fn remember_cwd_from_request(request: Option<&Value>) {
    if let Some(cwd) = extract_cwd(request) {
        remember_cwd(&cwd);
    }
}

pub fn remember_cwd_from_text(text: &str) -> bool {
    let Some(cwd) = extract_cwd_from_str(text) else {
        return false;
    };
    remember_cwd(&cwd);
    true
}

/// apply_patch's **middleware top-level entry point**: **restores known format errors one by
/// one** by whitelisted rule, so a malformed patch produced when the model doesn't follow the
/// prompt can still be correctly applied by Codex. **Only touches confirmed known pitfalls;
/// anything unknown is always passed through as-is (never guess, never drop).**
///
/// A two-tier structure (aligned with the [[MOC-194]] design):
///
/// **Tier A: syntax normalization** (mirrors the lark grammar Codex gives GPT, pure string
/// work, no disk reads -- retroactively guarantees, on the third-party chat path, the validity
/// that GPT's grammar constraints would otherwise guarantee):
/// - [`strip_trailing_at`] -- both-sided `@@ … @@` -> single-sided (grammar
///   `change_context: "@@" | "@@ " /(.+)/`; measured 18x).
/// - [`convert_unified_file_headers`] -- `--- old` / `+++ new` or `file: path` -> `*** Update File: path`.
/// - [`ensure_add_file_plus`] -- an Add File content line missing its `+` -> filled in (grammar
///   `add_line: "+" /(.*)/`, unambiguous for Add File).
/// - [`ensure_v4a_envelope`] -- missing `*** Begin/End Patch` -> filled in (grammar
///   `start: begin_patch hunk+ end_patch`; gotcha #6 + live-traffic seq230). **Only done when
///   `json_complete`** (not a streaming truncation), and **placed last** so it wraps whatever Tier B produced.
///
/// **Tier B: semantic recovery** (the file-state/content layer the grammar can't reach, needs a `cwd` disk read):
/// - [`recover_update_empty_file`] -- Update of an empty file -> Delete+Add (measured 50x, lossless).
/// - [`align_at_headers`] -- a truncated `@@ <header>` anchor -> aligned to the file's real full
///   line (`Failed to find context`).
/// - [`fix_unprefixed_lines`] -- an unprefixed line inside Update -> fills in a context space or
///   drops a duplicate stale line, decided against the file (seq235).
/// - [`recover_empty_move`] -- an empty Update+Move (rename-only) -> Delete+Add copying the original content (measured 76x).
/// - [`preflight_repair`] -- a byte-exact context mismatch in Update -> aligned by reading the disk (measured 134x).
///
/// Error shapes not covered here: **passed through as-is**, left for the Codex applier to error
/// on (never guess, never drop).
/// `json_complete`: the caller passes `detect_json_truncation(args).is_none()` (chat); gemini
/// args are always passed complete in one shot, so `true`.
pub fn optimize_patch(v4a: &str, cwd: Option<&str>, json_complete: bool) -> (String, Vec<Repair>) {
    // [MOC-194/MOC-263] **Two kinds of cwd, used for different purposes**:
    // - `fresh_cwd` = the current request's own `<cwd>` (usually None for apply_patch
    //   requests). **The file it resolves to == the file Codex actually applies to**, so it's
    //   trustworthy.
    // - The candidate history = the last N distinct cwds remembered across requests
    //   ([`recall_cwd_candidates`]). Codex only sends `<cwd>` on the turn-start request; later
    //   apply_patch tool-loop requests carry none -> we fall back to this. MOC-263: changed
    //   from a single slot to a candidate list, so concurrent multi-session use no longer gets
    //   overwritten by another project's stale cwd (disk reads try each candidate in turn and pick the first that exists).
    //
    // For the **state-rewriting rules** (`recover_update_empty_file` / `recover_empty_move`:
    // turning an Update into Delete+Add), the file used to decide and the file actually applied
    // to (Codex applies using the patch's relative path against the real cwd) **may not be the
    // same file** -> under the wrong cwd this could delete the wrong project's same-named file
    // (destructive). So these two rules **only use fresh_cwd** (safe only when decide==apply),
    // **never the candidate history**; an apply_patch request with no fresh cwd automatically
    // skips through (safe).
    // The **byte-exact alignment rules** (align/preflight/fix_unprefixed) pass `fresh_cwd` as
    // primary, then internally consult the candidate history via [`read_patch_file`]: at worst,
    // hitting the wrong file just means "no unique match / bytes don't match" -> a safe no-op.
    let fresh_cwd = cwd;
    // If the current request carries a cwd, record it into the candidate history (the
    // turn-start cwd is mainly recorded per-request by the converter's
    // `remember_cwd_from_request`; this is a fallback in case an apply_patch request happens to carry its own cwd too).
    if let Some(c) = cwd {
        remember_cwd(c);
    }
    let mut repairs = Vec::new();
    let mut s = v4a.to_owned();
    repairs.extend(diagnose_absolute_paths(&s, fresh_cwd));

    // -- Tier A syntax normalization (pure string work) --
    let (s1, r1) = strip_trailing_at(&s);
    s = s1;
    repairs.extend(r1);

    let (s_d, r_d) = convert_unified_file_headers(&s);
    s = s_d;
    repairs.extend(r_d);

    let (s_r, r_r) = strip_unified_hunk_ranges(&s);
    s = s_r;
    repairs.extend(r_r);

    let (s_g, r_g) = ensure_add_file_plus(&s);
    s = s_g;
    repairs.extend(r_g);

    // -- Tier B semantic recovery --
    // Note: the `Add File already exists -> overwrite with Delete+Add` rule has **been revoked**
    // (2026-06-09). It would overwrite an existing file and could lose existing content not
    // present in the Add content (a destructive downgrade); it also robbed the model of the
    // chance to self-correct into a **targeted Update** (lossless) after seeing `already
    // exists`. Changed to pass through as-is, letting Codex report `already exists` so the model can self-correct.
    //
    // State-rewriting rules -> **fresh_cwd** (guards against deleting the wrong file under a stale cwd, see above).
    let (s_f, r_f) = recover_update_empty_file(&s, fresh_cwd);
    s = s_f;
    repairs.extend(r_f);

    let (s3, r3) = recover_empty_move(&s, fresh_cwd);
    s = s3;
    repairs.extend(r3);

    // Byte-exact alignment rules -> pass fresh_cwd as primary, internally read_patch_file also checks the candidate history (worst case a safe no-op).
    let (s_h, r_h) = align_at_headers(&s, fresh_cwd);
    s = s_h;
    repairs.extend(r_h);

    let (s_u, r_u) = fix_unprefixed_lines(&s, fresh_cwd);
    s = s_u;
    repairs.extend(r_u);

    let (s2, r2) = preflight_repair(&s, fresh_cwd);
    s = s2;
    repairs.extend(r2);

    // -- Envelope completion goes last: it wraps structures like Delete+Add that Tier B may have added --
    if json_complete {
        let (s4, r4) = ensure_v4a_envelope(&s);
        s = s4;
        if let Some(r) = r4 {
            repairs.push(r);
        }
    }
    (s, repairs)
}

fn diagnose_absolute_paths(v4a: &str, primary: Option<&str>) -> Vec<Repair> {
    let mut known_cwds: Vec<String> = Vec::new();
    if let Some(cwd) = primary {
        if !cwd.is_empty() {
            known_cwds.push(cwd.to_string());
        }
    }
    known_cwds.extend(recall_cwd_candidates());
    known_cwds.sort();
    known_cwds.dedup();

    if known_cwds.is_empty() {
        return Vec::new();
    }

    let cwd_paths: Vec<PathBuf> = known_cwds.iter().map(PathBuf::from).collect();
    let mut repairs = Vec::new();
    for line in v4a.lines() {
        let Some(file) = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Add File: "))
            .or_else(|| line.strip_prefix("*** Delete File: "))
        else {
            continue;
        };
        let file = file.trim();
        let path = Path::new(file);
        if !path.is_absolute() {
            continue;
        }
        let inside_known_cwd = cwd_paths.iter().any(|cwd| path.starts_with(cwd));
        if !inside_known_cwd {
            repairs.push(Repair {
                file: file.to_string(),
                kind: "diagnostic:absolute_path_outside_known_cwd".to_string(),
                detail: format!(
                    "absolute patch target is outside known cwd candidates: {}",
                    known_cwds.join(" | ")
                ),
            });
        }
    }
    repairs
}

/// **Rule: both-sided `@@ … @@` -> single-sided `@@ …`** (prompt gotcha #1 / chat-path #1).
/// V4A's `@@` is a **single-sided** anchor (`@@ <header>`); the model often writes a
/// both-sided `@@ <header> @@`, and Codex treats the trailing `@@` as literal text ->
/// `Failed to find context '... @@'`. Only handles **column-0 `@@` header lines** (body lines
/// have a `+`/`-`/space prefix and are untouched), stripping the trailing `@@` and its leading
/// whitespace; a **bare `@@`** (section separator) is left alone.
fn strip_trailing_at(v4a: &str) -> (String, Vec<Repair>) {
    let mut changed = 0usize;
    let out: Vec<String> = v4a
        .lines()
        .map(|l| {
            if l.starts_with("@@") {
                let t = l.trim_end();
                // A bare `@@` (len==2) is a valid section separator, skip it; only `@@ x @@` gets its tail stripped.
                if t.len() > 2 && t.ends_with("@@") {
                    let body = t[..t.len() - 2].trim_end();
                    if !body.is_empty() && body != "@@" {
                        changed += 1;
                        return body.to_owned();
                    }
                }
            }
            l.to_owned()
        })
        .collect();
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    let repairs = if changed > 0 {
        vec![Repair {
            file: "(@@ header)".to_owned(),
            kind: "repaired".to_owned(),
            detail: format!("both-sided @@ -> single-sided: {changed} line(s) (prompt gotcha #1)"),
        }]
    } else {
        Vec::new()
    };
    (joined, repairs)
}

fn normalized_diff_path(path: &str) -> Option<String> {
    let mut p = path.trim();
    if p == "/dev/null" || p.is_empty() {
        return None;
    }
    if (p.starts_with("a/") || p.starts_with("b/")) && p.len() > 2 {
        p = &p[2..];
    }
    Some(p.to_string())
}

/// Gemini often writes patches as a unified diff on the non-native OpenAI apply_patch tool:
/// `--- path` / `+++ path` / `@@ -a,+b`. Codex V4A needs the file-operation header
/// `*** Update File: path`, so this only does a lossless header conversion.
fn convert_unified_file_headers(v4a: &str) -> (String, Vec<Repair>) {
    let lines: Vec<&str> = v4a.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut repairs = Vec::new();
    let mut i = 0usize;
    let mut converted = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if let Some(old_path) = line.strip_prefix("--- ") {
            if let Some(next) = lines.get(i + 1) {
                if let Some(new_path) = next.strip_prefix("+++ ") {
                    if let Some(path) =
                        normalized_diff_path(new_path).or_else(|| normalized_diff_path(old_path))
                    {
                        out.push(format!("*** Update File: {path}"));
                        repairs.push(Repair {
                            file: path,
                            kind: "repaired".to_string(),
                            detail: "unified diff file headers -> V4A Update File".to_string(),
                        });
                        converted += 1;
                        i += 2;
                        continue;
                    }
                }
            }
        }

        if let Some(path) = trimmed
            .strip_prefix("file: ")
            .or_else(|| trimmed.strip_prefix("File: "))
            .and_then(normalized_diff_path)
        {
            out.push(format!("*** Update File: {path}"));
            repairs.push(Repair {
                file: path,
                kind: "repaired".to_string(),
                detail: "file: header -> V4A Update File".to_string(),
            });
            converted += 1;
            i += 1;
            continue;
        }

        out.push(line.to_string());
        i += 1;
    }

    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    if converted == 0 {
        (v4a.to_owned(), Vec::new())
    } else {
        (joined, repairs)
    }
}

fn is_unified_range_token(token: &str, sign: char) -> bool {
    let Some(rest) = token.strip_prefix(sign) else {
        return false;
    };
    let mut pieces = rest.split(',');
    let Some(start) = pieces.next() else {
        return false;
    };
    if start.is_empty() || !start.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if let Some(count) = pieces.next() {
        if count.is_empty() || !count.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    pieces.next().is_none()
}

fn is_unified_hunk_range_header(line: &str) -> bool {
    if !line.starts_with("@@") {
        return false;
    }
    let mut body = line[2..].trim();
    if let Some(stripped) = body.strip_suffix("@@") {
        body = stripped.trim_end();
    }
    let mut parts = body.split_whitespace();
    let Some(old_range) = parts.next() else {
        return false;
    };
    let Some(new_range) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && is_unified_range_token(old_range, '-')
        && is_unified_range_token(new_range, '+')
}

/// A unified diff's `@@ -1,2 +1,3` line-number header is not a V4A anchor. Codex treats
/// `@@ <text>` as text context to search for, so a pure line-number range is normalized here into a bare `@@`.
fn strip_unified_hunk_ranges(v4a: &str) -> (String, Vec<Repair>) {
    let mut changed = 0usize;
    let out: Vec<String> = v4a
        .lines()
        .map(|line| {
            if is_unified_hunk_range_header(line) {
                changed += 1;
                "@@".to_string()
            } else {
                line.to_string()
            }
        })
        .collect();
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    let repairs = if changed > 0 {
        vec![Repair {
            file: "(@@ range header)".to_owned(),
            kind: "repaired".to_owned(),
            detail: format!("unified @@ line-number range -> V4A bare @@: {changed} line(s)"),
        }]
    } else {
        Vec::new()
    };
    (joined, repairs)
}

/// Post-hoc validation before sending to Codex's custom apply_patch. When a clearly invalid V4A
/// is found, the caller should mark that tool item as incomplete, avoiding a loop where Codex
/// executes it and the failure history gets fed back to the model.
pub fn validate_v4a_for_codex(v4a: &str) -> Option<(usize, String)> {
    let meaningful: Vec<(usize, &str)> = v4a
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .collect();
    let Some((first_line_no, first)) = meaningful.first() else {
        return Some((1, "empty apply_patch input".to_string()));
    };
    if first.trim() != "*** Begin Patch" {
        return Some((
            first_line_no + 1,
            "apply_patch input must start with *** Begin Patch".to_string(),
        ));
    }
    let Some((last_line_no, last)) = meaningful.last() else {
        return Some((1, "empty apply_patch input".to_string()));
    };
    if last.trim() != "*** End Patch" {
        return Some((
            last_line_no + 1,
            "apply_patch input must end with *** End Patch".to_string(),
        ));
    }

    let mut has_hunk = false;
    for (idx, line) in v4a.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "*** Begin Patch" && idx != *first_line_no {
            return Some((
                idx + 1,
                "apply_patch input contains a nested or repeated *** Begin Patch".to_string(),
            ));
        }
        if trimmed == "*** End Patch" && idx != *last_line_no {
            return Some((
                idx + 1,
                "apply_patch input contains an early or repeated *** End Patch".to_string(),
            ));
        }
        if trimmed.starts_with("*** Add File:")
            || trimmed.starts_with("*** Update File:")
            || trimmed.starts_with("*** Delete File:")
        {
            has_hunk = true;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            return Some((
                idx + 1,
                "V4A apply_patch does not accept unified diff file header lines (---/+++)"
                    .to_string(),
            ));
        }
        if is_unified_hunk_range_header(line) {
            return Some((
                idx + 1,
                "V4A apply_patch uses bare @@ or @@ text anchors, not unified @@ -a,+b ranges"
                    .to_string(),
            ));
        }
        if idx != *first_line_no && idx != *last_line_no && !trimmed.is_empty() {
            match line.chars().next() {
                Some('+') | Some('-') | Some(' ') => {}
                _ if trimmed.starts_with("@@") => {}
                _ if trimmed.starts_with("*** Add File:")
                    || trimmed.starts_with("*** Update File:")
                    || trimmed.starts_with("*** Delete File:")
                    || trimmed.starts_with("*** Move to:")
                    || trimmed == "*** End of File" => {}
                _ if trimmed.starts_with("***") => {
                    return Some((
                        idx + 1,
                        format!(
                            "unrecognized V4A operation: {}",
                            trimmed.chars().take(80).collect::<String>()
                        ),
                    ));
                }
                _ => {
                    return Some((
                        idx + 1,
                        "line missing V4A prefix (expected '+', '-', ' ', '@@', or '*** ' marker)"
                            .to_string(),
                    ));
                }
            }
        }
    }
    if !has_hunk {
        return Some((
            1,
            "apply_patch input has no file operation hunk".to_string(),
        ));
    }
    None
}

/// **Rule G: an Add File content line missing its `+` prefix -> filled in** (grammar
/// `add_hunk: … add_line+`, `add_line: "+" /(.*)/`). Add File semantics = every following line
/// is the new file's **literal content**, and must have a `+` prefix; the model occasionally
/// forgets the `+` -> Codex doesn't recognize it as content. There is **no ambiguity** inside an
/// Add File section (everything is added), so every non-`+` line uniformly gets a `+` prefix
/// (a blank line -> a bare `+`); a line that already has `+` is left alone (never doubled into `++`). Pure string work, no disk reads.
fn ensure_add_file_plus(v4a: &str) -> (String, Vec<Repair>) {
    if !v4a.contains("*** Add File:") {
        return (v4a.to_owned(), Vec::new());
    }
    let lines: Vec<&str> = v4a.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut repairs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(path) = lines[i].strip_prefix("*** Add File: ") {
            out.push(lines[i].to_owned()); // header
            i += 1;
            let mut fixed = 0usize;
            // body up to the next `*** ` control line / EOF; the Add File body is entirely `+` content lines.
            while i < lines.len() && !lines[i].starts_with("*** ") {
                if lines[i].starts_with('+') {
                    out.push(lines[i].to_owned());
                } else {
                    out.push(format!("+{}", lines[i]));
                    fixed += 1;
                }
                i += 1;
            }
            if fixed > 0 {
                repairs.push(Repair {
                    file: path.trim().to_owned(),
                    kind: "repaired".to_owned(),
                    detail: format!("Add File: {fixed} line(s) missing `+` prefix -> filled in (lark add_line)"),
                });
            }
        } else {
            out.push(lines[i].to_owned());
            i += 1;
        }
    }
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    (joined, repairs)
}

/// **Rule: align a `@@ <header>` anchor to the file's real line** (live-traffic seq181:
/// `Failed to find context 'X'`). V4A's `@@ <header>` is a single-sided anchor, and Codex
/// matches it against a section line in the file by **exact full line**; the model often writes
/// a **truncated** header (e.g. `@@ Architecture Overview` when the file's real line is
/// `## 6. Architecture Overview`) -> the anchor can't be found. When `<header>` doesn't match
/// any **full line** in the file, but is **contained in exactly one** file line, `@@ <header>`
/// is aligned to `@@ <that file's full line>`; 0 or multiple matches -> ambiguous, passed
/// through as-is (never guessed). A bare `@@` (no header) is untouched. Needs `cwd`.
fn align_at_headers(v4a: &str, cwd: Option<&str>) -> (String, Vec<Repair>) {
    if !has_cwd_candidate(cwd) {
        return (v4a.to_owned(), Vec::new());
    }
    if !v4a.contains("*** Update File:") {
        return (v4a.to_owned(), Vec::new());
    }
    let lines: Vec<&str> = v4a.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut repairs = Vec::new();
    let mut file_lines: Vec<String> = Vec::new();
    let mut have_file = false;
    let mut fixed = 0usize;
    let mut i = 0;
    while i < lines.len() {
        if let Some(path) = lines[i].strip_prefix("*** Update File: ") {
            // Switching to a new Update File section -> resolve the target file by candidate cwd + anchor probe (MOC-263 P1/P2)
            let mut se = i + 1;
            while se < lines.len() && !lines[se].starts_with("*** ") {
                se += 1;
            }
            let probe = anchor_probe(&lines[i + 1..se]);
            file_lines = read_patch_file(path.trim(), cwd, &probe)
                .map(|(_, c)| c.lines().map(str::to_owned).collect())
                .unwrap_or_default();
            have_file = !file_lines.is_empty();
            out.push(lines[i].to_owned());
            i += 1;
            continue;
        }
        // A `@@ <header>` anchor (not a bare `@@`), and the file has been loaded
        if have_file {
            if let Some(header) = lines[i].strip_prefix("@@ ") {
                let h = header.trim();
                if !h.is_empty() && !file_lines.iter().any(|fl| fl == h) {
                    let hits: Vec<&String> =
                        file_lines.iter().filter(|fl| fl.contains(h)).collect();
                    if hits.len() == 1 {
                        out.push(format!("@@ {}", hits[0]));
                        fixed += 1;
                        i += 1;
                        continue;
                    }
                }
            }
        }
        out.push(lines[i].to_owned());
        i += 1;
    }
    if fixed > 0 {
        repairs.push(Repair {
            file: "(@@ anchor)".to_owned(),
            kind: "repaired".to_owned(),
            detail: format!("@@ anchor truncated -> aligned to file's real full line: {fixed} occurrence(s) (Failed to find context)"),
        });
    }
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    (joined, repairs)
}

/// **Rule: an `Update File` target that is an empty file -> `Delete File + Add File`** (prompt
/// gotcha #3, lossless). `*** Update File:` can't operate on an empty file (Codex reports
/// `cannot operate on a completely empty file`). When the target file exists and **is empty**
/// (genuinely 0 bytes, not just whitespace) and the Update body is **pure `+` lines** (pure
/// content, no `-`/context to match), it's converted into `*** Delete File: X` +
/// `*** Add File: X` + the original `+` body (an empty file has no content to lose ->
/// lossless). If the body contains `-`/context (the model wrote match lines against an empty
/// file, which is already contradictory) or a Move (left to the empty-move rule) -> untouched. Needs `cwd`.
fn recover_update_empty_file(v4a: &str, cwd: Option<&str>) -> (String, Vec<Repair>) {
    let Some(cwd) = cwd else {
        return (v4a.to_owned(), Vec::new());
    };
    if !v4a.contains("*** Update File:") {
        return (v4a.to_owned(), Vec::new());
    }
    let lines: Vec<&str> = v4a.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut repairs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(path) = lines[i].strip_prefix("*** Update File: ") {
            let p = path.trim();
            // Only recognizes **genuinely 0 bytes** (Codex only errors on a `completely empty
            // file`; a whitespace-only file is still readable content and Updates normally).
            // Using `c.trim().is_empty()` would also convert whitespace-only files to
            // Delete+Add -> losing those whitespace bytes (destructive, codex-connector #435 P2).
            let is_empty = std::fs::read_to_string(resolve_path(p, cwd))
                .map(|c| c.is_empty())
                .unwrap_or(false);
            if is_empty {
                let body_start = i + 1;
                let mut j = body_start;
                while j < lines.len() && !lines[j].starts_with("*** ") {
                    j += 1;
                }
                let body = &lines[body_start..j];
                let has_move = body
                    .first()
                    .map(|l| l.starts_with("*** Move to:"))
                    .unwrap_or(false);
                let content: Vec<&&str> = body
                    .iter()
                    .filter(|l| !l.trim().is_empty() && !l.starts_with("@@"))
                    .collect();
                let all_plus = !content.is_empty() && content.iter().all(|l| l.starts_with('+'));
                if !has_move && all_plus {
                    out.push(format!("*** Delete File: {p}"));
                    out.push(format!("*** Add File: {p}"));
                    for b in body {
                        if b.starts_with('+') {
                            out.push((*b).to_owned());
                        }
                    }
                    repairs.push(Repair {
                        file: p.to_owned(),
                        kind: "repaired".to_owned(),
                        detail: "Update of an empty file -> written as Delete+Add (prompt gotcha #3)".to_owned(),
                    });
                    i = j;
                    continue;
                }
            }
        }
        out.push(lines[i].to_owned());
        i += 1;
    }
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    (joined, repairs)
}

/// **Rule: an empty `Update File + Move to` (rename-only) -> `Delete File + Add File`** (prompt
/// gotcha #7). The model wants a pure rename but writes `*** Update File: X` +
/// `*** Move to: Y` with **no hunk** -> Codex reports `Update file hunk for path 'X' is empty`.
/// Recovered per the prompt's **own suggestion**: read X's original content, convert to
/// `*** Delete File: X` + `*** Add File: Y` + copy line-by-line with `+` (a blank line becomes a bare `+`). If X can't be read -> passed through as-is.
fn recover_empty_move(v4a: &str, cwd: Option<&str>) -> (String, Vec<Repair>) {
    let Some(cwd) = cwd else {
        return (v4a.to_owned(), Vec::new());
    };
    if !v4a.contains("*** Move to:") {
        return (v4a.to_owned(), Vec::new());
    }
    let lines: Vec<&str> = v4a.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut repairs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        // Matches `*** Update File: X` immediately followed by `*** Move to: Y`, with no hunk lines between Move and the next `*** ` control line.
        if let Some(old) = lines[i].strip_prefix("*** Update File: ") {
            if i + 1 < lines.len() {
                if let Some(new) = lines[i + 1].strip_prefix("*** Move to: ") {
                    // Checks whether there's any hunk content line after Move and before the
                    // next **file operation** control line. Note: `*** End of File` is a
                    // documented **in-hunk marker** (from the prompt's RENAME/MOVE section), not
                    // a section boundary -- scanning must not stop there (otherwise a
                    // rename+EOF-append would be misjudged as an empty rename and converted into
                    // a content-losing Delete+Add, codex-connector #435 P1). It itself signals
                    // "there is a hunk", so scanning continues past it.
                    let mut j = i + 2;
                    let mut has_hunk = false;
                    while j < lines.len() {
                        let t = lines[j];
                        if t.trim_end() == "*** End of File" {
                            has_hunk = true;
                            j += 1;
                            continue;
                        }
                        if t.starts_with("*** ") {
                            break; // the real next file operation / End Patch boundary
                        }
                        if t.starts_with('+')
                            || t.starts_with('-')
                            || t.starts_with(' ')
                            || t.starts_with("@@")
                        {
                            has_hunk = true;
                        }
                        j += 1;
                    }
                    if !has_hunk {
                        // An empty rename-only -> read the original file and convert to
                        // Delete+Add. If it can't be read / content is empty -> don't convert
                        // (an empty Add File body might be rejected by Codex) -> passed through
                        // as-is for Codex to handle.
                        let abs = resolve_path(old.trim(), cwd);
                        match std::fs::read_to_string(&abs) {
                            Ok(content) if !content.is_empty() => {
                                out.push(format!("*** Delete File: {}", old.trim()));
                                out.push(format!("*** Add File: {}", new.trim()));
                                for cl in content.lines() {
                                    out.push(format!("+{cl}"));
                                }
                                repairs.push(Repair {
                                    file: old.trim().to_owned(),
                                    kind: "repaired".to_owned(),
                                    detail: format!(
                                        "empty Update+Move (rename-only) -> Delete+Add copying original content -> {}(prompt gotcha #7)",
                                        new.trim()
                                    ),
                                });
                                i = j; // skip the original Update/Move (+ empty body)
                                continue;
                            }
                            _ => {
                                repairs.push(Repair {
                                    file: old.trim().to_owned(),
                                    kind: "skipped:unreadable_or_empty".to_owned(),
                                    detail: "empty Update+Move but the original file couldn't be read / was empty -> passed through as-is"
                                        .to_owned(),
                                });
                            }
                        }
                    }
                }
            }
        }
        out.push(lines[i].to_owned());
        i += 1;
    }
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    (joined, repairs)
}

/// Whether the last patch operation is "`*** Add File:` targeting a code / structured config
/// file". Used by [`ensure_v4a_envelope`] to decide whether the last line `+*** End Patch` can
/// safely be stripped into a terminator: it's stripped **only for Add File** (a new file, where
/// a bare `*** End Patch` can never legitimately be the **last line** of real source code -> it
/// must be a mis-prefixed terminator); for `*** Update File:`, a `+*** End Patch` is an
/// **added line** (it could genuinely be adding this string into a string literal / fixture),
/// so stripping it would drop real content -> not stripped (chatgpt-codex-connector review:
/// restricted to Add File). Docs / text / unknown extensions are also not stripped (could be
/// real body content, left as incomplete rather than guessed). The allowlist is conservative. MOC-268.
fn last_op_is_add_file_code(body: &str) -> bool {
    let last_op = body.lines().rev().find(|l| {
        let t = l.trim_end();
        t.starts_with("*** Add File: ")
            || t.starts_with("*** Update File: ")
            || t.starts_with("*** Delete File: ")
    });
    let Some(path) = last_op.and_then(|l| l.trim_end().strip_prefix("*** Add File: ")) else {
        return false; // no operation, or the last operation is Update/Delete (not Add File) -> don't strip
    };
    let ext = std::path::Path::new(path.trim())
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    matches!(
        ext.as_deref(),
        Some(
            "rs" | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "py"
                | "go"
                | "java"
                | "kt"
                | "kts"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "cxx"
                | "hpp"
                | "hh"
                | "cs"
                | "rb"
                | "php"
                | "swift"
                | "scala"
                | "lua"
                | "sql"
                | "sh"
                | "bash"
                | "zsh"
                | "css"
                | "scss"
                | "sass"
                | "less"
                | "html"
                | "htm"
                | "xml"
                | "vue"
                | "svelte"
                | "json"
                | "toml"
                | "yaml"
                | "yml"
                | "gradle"
                | "cmake"
                | "proto"
                | "graphql"
                | "dart"
                | "r"
        )
    )
}

/// **Auto-completes a missing envelope**: the model often writes only `*** Add/Update File:` +
/// content, omitting the `*** Begin Patch` / `*** End Patch` header/footer -> Codex (and this
/// adapter's V4A validation) judges it incomplete -> the model is forced to retry. When the
/// patch contains at least one `*** Add/Update/Delete File:` operation, the JSON is already
/// complete (the caller gates on `detect_json_truncation` being None before calling this
/// function, ensuring it's not a streaming truncation), but the Begin/End envelope is missing,
/// this **purely adds the markers** (changes not a single byte of content, never guesses) and
/// returns `(completed, Some(Repair))`; if it's already complete / not a patch body, returns `(as-is, None)`.
///
/// Safety: when Begin is missing, `*** Begin Patch` is only prepended **when the first
/// non-empty line is itself an operation line** (if there's leading prose, it's left untouched,
/// for `repair_v4a_envelope` / Codex to handle); when End is missing, `*** End Patch` is appended after trimming trailing whitespace.
pub fn ensure_v4a_envelope(input: &str) -> (String, Option<Repair>) {
    let is_op = |l: &str| {
        let t = l.trim_end();
        t.starts_with("*** Add File:")
            || t.starts_with("*** Update File:")
            || t.starts_with("*** Delete File:")
    };
    if !input.lines().any(is_op) {
        return (input.to_owned(), None); // not a recognizable patch body, leave it untouched
    }
    let has_begin = input.lines().any(|l| l.trim_end() == "*** Begin Patch");
    let has_end = input.lines().any(|l| l.trim_end() == "*** End Patch");
    if has_begin && has_end {
        return (input.to_owned(), None);
    }
    let mut body = input.to_owned();
    let mut added: Vec<&str> = Vec::new();
    if !has_begin {
        // Only safe when the first non-empty line is itself an operation line (no leading prose mixed into the envelope).
        let first_nonempty = input.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if !is_op(first_nonempty) {
            return (input.to_owned(), None);
        }
        body = format!("*** Begin Patch\n{body}");
        added.push("Begin Patch");
    }
    if !has_end {
        let trimmed = body.trim_end();
        let last = trimmed.lines().last().unwrap_or("");
        // [MOC-268] **Only `+*** End Patch`** (an Add-line-prefixed terminator) is the shape of
        // "the model mis-prefixed the terminator". ` *** End Patch` (context) / `-*** End Patch`
        // (deletion) are **legitimate Update hunk lines** -- for example the model using Update
        // to **delete** a `*** End Patch` left over in the file (`-*** End Patch`), or using it
        // as a context anchor; treating them as a terminator and stripping them would **silently
        // drop the deletion / break the anchor** (chatgpt-codex-connector review) -> so ` `/`-`
        // always go through the normal append path below (a real terminator is added, hunk lines are kept as-is).
        // `+*** End Patch` is further **disambiguated by file type** (per the user's decision):
        //   - Code / structured config files (a bare `*** End Patch` can never legitimately be
        //     the last line of real source code) -> it must be a mis-prefixed terminator ->
        //     **strip the prefix** (`head` cuts to the start of the last line, keeping the
        //     newline before it; the last line is ASCII, so the boundary is safe).
        //   - Docs / text / unknown (could be a real body line) -> **never guess**: don't strip
        //     (avoids deleting real content), don't append (avoids leaving a residual line),
        //     leave it incomplete for downstream truncation handling, letting the model resend
        //     per guidance rule 2. The prompt is the real fix; the middleware only steps in when it's certain it's safe.
        if last == "+*** End Patch" {
            if last_op_is_add_file_code(&body) {
                let head = &trimmed[..trimmed.len() - last.len()];
                body = format!("{head}*** End Patch");
                added.push("End Patch (code file: stripped mis-added prefix terminator)");
            } else {
                return (
                    body,
                    Some(Repair {
                        file: "(envelope)".to_owned(),
                        kind: "skipped:ambiguous_prefixed_end".to_owned(),
                        detail:
                            "last line is +*** End Patch and the target is not a code file (could be real content) -> not guessed or completed, left incomplete"
                                .to_owned(),
                    }),
                );
            }
        } else {
            // Contains ` *** End Patch` / `-*** End Patch` (legitimate hunk lines) or an ordinary content last line -> the real terminator is appended normally.
            body = format!("{trimmed}\n*** End Patch");
            added.push("End Patch");
        }
    }
    (
        body,
        Some(Repair {
            file: "(envelope)".to_owned(),
            kind: "repaired".to_owned(),
            detail: format!("model omitted the envelope, auto-completed: {}", added.join(" + ")),
        }),
    )
}

/// **Rule: an unprefixed line inside an Update body is completed based on the file** (live
/// traffic seq235: a single unprefixed line -> validate rejects it -> the whole Update gets
/// rewritten, wasted effort). The grammar `change_line: ("+"|"-"|" ") /(.*)/` requires every
/// line to have a prefix; the model occasionally forgets one line's prefix. This is a
/// **non-destructive** repair (only fills in a prefix / drops a provably duplicate stale line,
/// never drops content):
/// - An unprefixed line **duplicating an adjacent `+<same content>` line** (the model wrote it
///   twice) -> that stale line is dropped (the content lives in the `+` line, nothing is lost);
/// - Otherwise, an unprefixed **non-empty** line that **matches a full line in the target file
///   exactly** -> it's a context line missing its space -> a ` ` is filled in (a legitimate,
///   byte-exact context; the least destructive interpretation: the line is kept. If the model
///   actually meant to delete it, at worst the deletion just didn't happen, no data is lost);
/// - Everything else (not in the file, not a duplicate, blank line) -> passed through as-is, letting validate error out so the model self-corrects (never guessed).
///
/// Only applies inside a `*** Update File:` section (a missing `+` in Add File is handled by [`ensure_add_file_plus`]). Needs `cwd`.
fn fix_unprefixed_lines(v4a: &str, cwd: Option<&str>) -> (String, Vec<Repair>) {
    if !has_cwd_candidate(cwd) {
        return (v4a.to_owned(), Vec::new());
    }
    if !v4a.contains("*** Update File:") {
        return (v4a.to_owned(), Vec::new());
    }
    let lines: Vec<&str> = v4a.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut repairs = Vec::new();
    let mut in_update = false;
    let mut file_lines: Vec<String> = Vec::new();
    let mut drop_dups = 0usize;
    let mut add_ctx = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if let Some(path) = l.strip_prefix("*** Update File: ") {
            in_update = true;
            // Resolve the target file by candidate cwd + anchor probe (MOC-263 P1/P2).
            let mut se = i + 1;
            while se < lines.len() && !lines[se].starts_with("*** ") {
                se += 1;
            }
            let probe = anchor_probe(&lines[i + 1..se]);
            file_lines = read_patch_file(path.trim(), cwd, &probe)
                .map(|(_, c)| c.lines().map(str::to_owned).collect())
                .unwrap_or_default();
            out.push(l.to_owned());
            i += 1;
            continue;
        }
        if l.starts_with("*** ") {
            in_update = false; // any other control line ends the Update body
            out.push(l.to_owned());
            i += 1;
            continue;
        }
        let first = l.chars().next();
        let valid = matches!(first, Some('+') | Some('-') | Some(' '))
            || l.starts_with("@@")
            || l.is_empty();
        if in_update && !valid {
            // case 1: a stale line duplicating an adjacent `+<same content>` -> drop it (the content lives in the + line, nothing is lost)
            let plus_dup = format!("+{l}");
            let next_dup = lines.get(i + 1).map(|n| *n == plus_dup).unwrap_or(false);
            let prev_dup = out.last().map(|o| o == &plus_dup).unwrap_or(false);
            if next_dup || prev_dup {
                drop_dups += 1;
                i += 1;
                continue;
            }
            // case 2: an identical full line exists in the file -> context missing its space -> fill in ` `
            if file_lines.iter().any(|fl| fl == l) {
                out.push(format!(" {l}"));
                add_ctx += 1;
                i += 1;
                continue;
            }
            // else: pass through (never guess)
        }
        out.push(l.to_owned());
        i += 1;
    }
    if drop_dups + add_ctx > 0 {
        repairs.push(Repair {
            file: "(unprefixed)".to_owned(),
            kind: "repaired".to_owned(),
            detail: format!(
                "Update unprefixed-line repair: filled in context space {add_ctx} / dropped duplicate stale line {drop_dups} (lark change_line)"
            ),
        });
    }
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    (joined, repairs)
}

/// Does pre-flight repair on a V4A patch. `cwd` is used to resolve the patch's relative paths
/// to real files. Returns `(repaired V4A, processing records)`. With no cwd / no `Update File` / an unreadable file, the V4A is returned as-is.
pub fn preflight_repair(v4a: &str, cwd: Option<&str>) -> (String, Vec<Repair>) {
    if !has_cwd_candidate(cwd) {
        return (v4a.to_owned(), Vec::new());
    }
    // Short-circuit immediately when there's no Update File at all (Add/Delete File don't involve anchor matching).
    if !v4a.contains("*** Update File:") {
        return (v4a.to_owned(), Vec::new());
    }
    let mut repairs = Vec::new();
    let lines: Vec<&str> = v4a.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            out.push(line.to_owned());
            i += 1;
            // Collect this Update File section's body (up to the next `*** ` control line).
            let body_start = i;
            while i < lines.len() && !lines[i].starts_with("*** ") {
                i += 1;
            }
            let body = &lines[body_start..i];
            let (repaired_body, rep) = repair_update_section(path.trim(), body, cwd);
            out.extend(repaired_body);
            repairs.push(rep);
        } else {
            out.push(line.to_owned());
            i += 1;
        }
    }
    // Preserve trailing-newline semantics: lines() drops the trailing newline, so add it back after join if the original text ended with \n.
    let mut joined = out.join("\n");
    if v4a.ends_with('\n') {
        joined.push('\n');
    }
    (joined, repairs)
}

/// [MOC-263 P0] Finds the longest contiguous block in `file[floor..]`, starting from
/// `anchors[0]`, that matches **uniquely**. Anchors are compared "ignoring trailing whitespace"
/// (in-block byte drift is left for the later repair_hunk alignment). Returns `(block length =
/// number of anchors matched, file start position)`. Longest and unique -> Some; if the longest
/// non-empty match occurs at >1 place (ambiguous) -> None (a shorter block would only be more
/// ambiguous); all 0 -> None.
fn longest_unique_block(anchors: &[&str], file: &[&str], floor: usize) -> Option<(usize, usize)> {
    if anchors.is_empty() || floor >= file.len() {
        return None;
    }
    // [MOC-263 P1] The segment's first anchor must be **globally unique** in `file[floor..]`,
    // otherwise the segment's start point is ambiguous -- when the same line also occurs
    // elsewhere, the greedy-longest-block search can use a longer block's "uniqueness" to pick
    // an **unrelated, earlier region** (the file has a stale `A/B/C` block plus the real
    // `A/B…gap…C/D` region; the body `A/-B/ C/-D` gets carved into the stale block's hunk ->
    // deleting B from the wrong block), whereas without splitting this would have safely
    // failed. A non-unique start point means the split isn't uniquely determined -> bail out,
    // passing through as-is for the model to self-correct (chatgpt-codex-connector review; never guess, never drop).
    let first = anchors[0].trim_end();
    if file[floor..]
        .iter()
        .filter(|l| l.trim_end() == first)
        .count()
        != 1
    {
        return None;
    }
    let max_len = anchors.len().min(file.len() - floor);
    for len in (1..=max_len).rev() {
        let block = &anchors[..len];
        let mut hits: Vec<usize> = Vec::new();
        let mut start = floor;
        while start + len <= file.len() {
            if (0..len).all(|t| file[start + t].trim_end() == block[t].trim_end()) {
                hits.push(start);
                if hits.len() > 1 {
                    break;
                }
            }
            start += 1;
        }
        match hits.len() {
            1 => return Some((len, hits[0])),
            0 => continue,    // too long (spans a jump in the file) -> shorten and retry
            _ => return None, // an ambiguous longest non-empty match -> give up (never guess, never drop)
        }
    }
    None
}

/// [MOC-263 P0] Splits an Update body that has **no `@@`** into multiple hunks by the file's real positions.
///
/// The main live-traffic apply failure cause (phase-1 seg1/seg3): the model packs several
/// **non-contiguous** edit groups into one Update File block, omitting the `@@` separator ->
/// the applier treats the whole block as one contiguous context match ->
/// `Failed to find expected lines`. This greedily splits the anchor (context/delete) sequence
/// into **ordered, non-overlapping, each-uniquely-matching** N segments (each segment = the
/// longest contiguous anchor block, uniquely matched, starting after the previous segment),
/// with `+` added lines kept alongside the adjacent segment. Returns `Some` only when **N>=2
/// and every segment can be uniquely located**; a single segment / any segment being ambiguous
/// or unlocatable -> `None` (the caller passes it through as-is, never guessing, never
/// dropping). The caller joins the segments with a bare `@@`.
fn segment_no_at_body<'a>(body: &[&'a str], file: &[&str]) -> Option<Vec<Vec<&'a str>>> {
    let anchors: Vec<(usize, &str)> = body
        .iter()
        .enumerate()
        .filter_map(|(idx, l)| match l.chars().next() {
            Some(' ') | Some('-') => Some((idx, &l[1..])),
            _ => None,
        })
        .collect();
    if anchors.len() < 2 {
        return None;
    }
    let anchor_contents: Vec<&str> = anchors.iter().map(|(_, c)| *c).collect();

    // Greedy segmentation: each segment = the longest contiguous anchor block, uniquely matched, starting from floor.
    // Recorded as (anchor_start, anchor_end_excl, file_start, file_end_excl); floor advances monotonically, guaranteeing order and no overlap.
    let mut raw: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut ai = 0usize;
    let mut floor = 0usize;
    while ai < anchors.len() {
        let (len, pos) = longest_unique_block(&anchor_contents[ai..], file, floor)?;
        raw.push((ai, ai + len, pos, pos + len));
        ai += len;
        floor = pos + len;
    }

    // Merge adjacent segments: if the file gap between segments is **all blank lines** (the
    // model omitted blank lines present in the file) -> treat as the same hunk, don't split
    // here, leave it to repair_hunk's EP-1 blank-tolerant handling (otherwise blank-line drift
    // would be wrongly split into two segments, breaking existing behavior). Only kept as a
    // separate segment when the gap contains **non-blank lines** (a genuinely non-contiguous edit region).
    let mut groups: Vec<(usize, usize, usize, usize)> = Vec::new();
    for g in raw {
        if let Some(last) = groups.last_mut() {
            let gap = &file[last.3..g.2];
            if gap.iter().all(|l| l.trim().is_empty()) {
                last.1 = g.1;
                last.3 = g.3;
                continue;
            }
        }
        groups.push(g);
    }
    if groups.len() < 2 {
        return None; // a single segment (or everything merged into one via blank-line gaps) -> no need to split, hand back to the regular path
    }

    // [MOC-263 P0 safety guard] A "floating `+` insertion line" between segments has an
    // ambiguous landing spot -> always bail. If **any** `+` added line exists between the
    // previous segment's last anchor and the next segment's first anchor, that `+`'s landing
    // spot can't be uniquely determined from the V4A -- it could be an insertion at the end of
    // the previous segment, or it could be a "lead-in line" the model wrote for the next
    // segment; when non-blank content separates the segments, the two landing spots are
    // different positions in the file, and guessing wrong is a **silent, incorrect apply**
    // (violating never-guess-never-drop). **Critically**: this is unsafe even when the previous
    // segment's last anchor is a `-` deletion (per chatgpt-codex-connector review's mixed
    // replace+insert example: `-return 1`/`+return 42`/`+@memoize`/` def beta():` -- `+return
    // 42` is a replacement, but `+@memoize` is a lead-in line for the next segment, and the two
    // can't be told apart) -> so there's no "previous segment has a deletion, so it's fine"
    // exemption; as soon as the gap contains a `+`, the whole split attempt is abandoned and
    // passed through as-is (for the model to self-correct). Multiple regions that are purely
    // deletion / purely context (no `+` in the gap) are still split safely.
    for gi in 0..groups.len() - 1 {
        let last_anchor_line = anchors[groups[gi].1 - 1].0;
        let next_anchor_line = anchors[groups[gi + 1].0].0;
        let gap_has_add = body[last_anchor_line + 1..next_anchor_line]
            .iter()
            .any(|l| l.starts_with('+'));
        if gap_has_add {
            return None;
        }
    }

    // Segment g's body line range: the first segment includes the leading lines at the start
    // (body[0..first anchor]); other segments run from their first anchor to the next
    // segment's first anchor -> `+` lines inside / after a segment are kept with the **preceding** segment.
    let mut subhunks: Vec<Vec<&'a str>> = Vec::new();
    for gi in 0..groups.len() {
        let line_start = if gi == 0 { 0 } else { anchors[groups[gi].0].0 };
        let line_end = if gi + 1 < groups.len() {
            anchors[groups[gi + 1].0].0
        } else {
            body.len()
        };
        subhunks.push(body[line_start..line_end].to_vec());
    }
    Some(subhunks)
}

/// Repairs the body of one `Update File` section. `path` is the (relative) path from the
/// patch. `cwd` is the current request's cwd (a primary hint); the disk read is resolved via
/// [`read_patch_file`], which then also consults the candidate history (MOC-263).
fn repair_update_section(path: &str, body: &[&str], cwd: Option<&str>) -> (Vec<String>, Repair) {
    let probe = anchor_probe(body);
    let Some((_abs, content)) = read_patch_file(path, cwd, &probe) else {
        return (
            body.iter().map(|l| (*l).to_owned()).collect(),
            Repair {
                file: path.to_owned(),
                kind: "skipped:unreadable".to_owned(),
                detail: format!("could not read file {path} (no candidate cwd had it) -> passed through as-is"),
            },
        );
    };
    let file_lines: Vec<&str> = content.lines().collect();

    // [MOC-263 P0] When the body has no `@@` but contains multiple non-contiguous edit groups
    // (the model omitted the `@@` separator) -> automatically split by the file's real
    // positions and join with a bare `@@`, so the applier locates each segment as an
    // independent hunk (otherwise the whole block as one contiguous context is bound to fail
    // to match). Only acts when the split is unique; a single segment / ambiguity -> keeps the
    // original body for the regular path.
    let mut split_owned: Vec<&str> = Vec::new();
    // Uses `@@` at **column 0** (not trim_start) to decide whether hunk separators already
    // exist, matching the actual splitter below (`l.starts_with("@@")`) -- otherwise a context
    // line like ` @@ ...` (leading space, content starting with @@, e.g. markdown/diff text)
    // would be mistaken for a separator, wrongly disabling auto-split, while the splitter still
    // wouldn't split it -> still failing (chatgpt-codex-connector review).
    let did_split = if !body.iter().any(|l| l.starts_with("@@")) {
        match segment_no_at_body(body, &file_lines) {
            Some(subhunks) => {
                for (k, sub) in subhunks.iter().enumerate() {
                    if k > 0 {
                        split_owned.push("@@");
                    }
                    split_owned.extend_from_slice(sub);
                }
                true
            }
            None => false,
        }
    } else {
        false
    };
    let effective_body: &[&str] = if did_split { &split_owned } else { body };

    // Split the body into hunks (segmented on `@@` lines; the `@@` line itself is kept and doesn't take part in anchor matching).
    let mut new_body: Vec<String> = Vec::with_capacity(effective_body.len());
    let mut repaired_hunks = 0;
    let mut clean_hunks = 0;
    let mut skipped: Vec<String> = Vec::new();
    let mut hunk: Vec<&str> = Vec::new();
    let flush = |hunk: &mut Vec<&str>,
                 new_body: &mut Vec<String>,
                 repaired_hunks: &mut usize,
                 clean_hunks: &mut usize,
                 skipped: &mut Vec<String>| {
        if hunk.is_empty() {
            return;
        }
        match repair_hunk(hunk, &file_lines) {
            HunkOutcome::Clean => {
                *clean_hunks += 1;
                new_body.extend(hunk.iter().map(|l| (*l).to_owned()));
            }
            HunkOutcome::Repaired(fixed) => {
                *repaired_hunks += 1;
                new_body.extend(fixed);
            }
            HunkOutcome::Skipped(reason) => {
                skipped.push(reason);
                new_body.extend(hunk.iter().map(|l| (*l).to_owned()));
            }
        }
        hunk.clear();
    };

    for &l in effective_body {
        if l.starts_with("@@") {
            flush(
                &mut hunk,
                &mut new_body,
                &mut repaired_hunks,
                &mut clean_hunks,
                &mut skipped,
            );
            new_body.push(l.to_owned());
        } else {
            hunk.push(l);
        }
    }
    flush(
        &mut hunk,
        &mut new_body,
        &mut repaired_hunks,
        &mut clean_hunks,
        &mut skipped,
    );

    let kind = if repaired_hunks > 0 || did_split {
        "repaired"
    } else if skipped.is_empty() {
        "clean"
    } else {
        "skipped:no_unique_match"
    };
    let detail = format!(
        "{}hunk: repaired {repaired_hunks} / already matched {clean_hunks} / passed through {}{}",
        if did_split {
            "multiple hunks with no @@ separator -> auto-split by file position and inserted bare @@; "
        } else {
            ""
        },
        skipped.len(),
        if skipped.is_empty() {
            String::new()
        } else {
            format!(" ({})", skipped.join("; "))
        }
    );
    (
        new_body,
        Repair {
            file: path.to_owned(),
            kind: kind.to_owned(),
            detail,
        },
    )
}

enum HunkOutcome {
    /// The anchors matched the file exactly, no change needed.
    Clean,
    /// The whole hunk (including `+` lines as-is) after aligning anchors to the file's real bytes.
    Repaired(Vec<String>),
    /// Not repaired (0 or multiple matches), with the reason attached.
    Skipped(String),
}

/// Repairs one hunk: anchors = the **content** of context (space-prefixed) + deletion (`-`)
/// lines (prefix stripped), which in order should form a contiguous block in the file. An exact
/// match -> Clean; otherwise looks for candidates by "ignoring trailing whitespace / leading-trailing whitespace", aligns on a unique candidate, otherwise passes through.
fn repair_hunk(hunk: &[&str], file_lines: &[&str]) -> HunkOutcome {
    // The anchor lines' indices within the hunk, plus their content (single-char prefix stripped).
    let anchors: Vec<(usize, &str)> = hunk
        .iter()
        .enumerate()
        .filter_map(|(idx, l)| match l.chars().next() {
            Some(' ') => Some((idx, &l[1..])),
            Some('-') => Some((idx, &l[1..])),
            _ => None, // '+' added line / blank line / anything else is not an anchor
        })
        .collect();
    if anchors.is_empty() {
        return HunkOutcome::Clean; // pure addition, no anchors
    }
    let anchor_contents: Vec<&str> = anchors.iter().map(|(_, c)| *c).collect();

    // Exact match: a contiguous block in the file equals the anchor content exactly -> no repair needed (Codex can find it itself).
    if !find_block(file_lines, &anchor_contents, |a, b| a == b).is_empty() {
        return HunkOutcome::Clean;
    }

    // Fuzzy match: equal line-by-line "ignoring trailing whitespace"; if still 0, fall back to "ignoring leading-trailing whitespace entirely".
    let mut matches = find_block(file_lines, &anchor_contents, |a, b| {
        a.trim_end() == b.trim_end()
    });
    let mut mode = "trailing whitespace";
    if matches.is_empty() {
        matches = find_block(file_lines, &anchor_contents, |a, b| a.trim() == b.trim());
        mode = "leading/trailing whitespace";
    }
    match matches.len() {
        1 => {
            let pos = matches[0];
            // Align the anchor lines to the file's real bytes (preserving the hunk's +/-/space interleaving and `+` lines).
            let mut fixed: Vec<String> = hunk.iter().map(|l| (*l).to_owned()).collect();
            for (k, (idx, _)) in anchors.iter().enumerate() {
                let prefix = hunk[*idx].chars().next().unwrap(); // ' ' or '-'
                let file_line = file_lines[pos + k];
                fixed[*idx] = format!("{prefix}{file_line}");
            }
            HunkOutcome::Repaired(fixed)
        }
        n if n > 1 => HunkOutcome::Skipped(format!("{n} match(es) under {mode} (ambiguous)")),
        // 0 contiguous matches -> try "ignoring blank-line differences" (EP-1: the model
        // omitted/added extra blank lines, causing the whole block to mismatch). The anchor's
        // **non-blank** line sequence is uniquely located in the file (the file region is
        // allowed to contain blank lines the model omitted); on a hit, the anchors are rebuilt
        // from the file's real region (including blank lines + bytes), with `+` insertion lines
        // kept in their original order. 0 or multiple matches still pass through (never guessed).
        _ => {
            // A blank-tolerant rebuild would discard a blank anchor line and use the file's
            // blank line instead -> it can't faithfully represent a `-` that means "delete a
            // blank line" (it would be silently turned into context = the deletion never
            // happened). If the hunk contains a blank-line deletion, blank-tolerant is
            // abandoned and the hunk is passed through (never guessed).
            let has_blank_deletion = hunk
                .iter()
                .any(|l| l.starts_with('-') && l[1..].trim().is_empty());
            if has_blank_deletion {
                return HunkOutcome::Skipped(
                    "contains a blank-line deletion, blank-tolerant is unsafe -> passed through".to_owned(),
                );
            }
            let regions = find_regions_blank_tolerant(file_lines, &anchor_contents);
            match regions.len() {
                1 => {
                    let (s, e) = regions[0];
                    HunkOutcome::Repaired(rebuild_hunk_with_region(hunk, &file_lines[s..e]))
                }
                0 => HunkOutcome::Skipped("0 matches for the anchors in the file (the model may have changed the wrong content)".to_owned()),
                n => HunkOutcome::Skipped(format!("{n} match(es) ignoring blank lines (ambiguous)")),
            }
        }
    }
}

/// EP-1 helper: finds regions in `file_lines` that the anchor's **non-blank** line sequence
/// uniquely locates (the file region may contain blank lines the model omitted, but no extra
/// non-blank lines are allowed). Returns all matching regions `[start, end)` (end is one past the last matched non-blank line).
fn find_regions_blank_tolerant(
    file_lines: &[&str],
    anchor_contents: &[&str],
) -> Vec<(usize, usize)> {
    let nb: Vec<&str> = anchor_contents
        .iter()
        .map(|c| c.trim_end())
        .filter(|c| !c.trim().is_empty())
        .collect();
    if nb.is_empty() {
        return Vec::new();
    }
    let mut regions = Vec::new();
    for start in 0..file_lines.len() {
        if file_lines[start].trim().is_empty() || file_lines[start].trim_end() != nb[0] {
            continue;
        }
        let mut fi = start;
        let mut ai = 0;
        let mut ok = true;
        while ai < nb.len() {
            if fi >= file_lines.len() {
                ok = false;
                break;
            }
            let fl = file_lines[fi];
            if fl.trim().is_empty() {
                fi += 1; // skip a blank file line (the model may have omitted it)
                continue;
            }
            if fl.trim_end() == nb[ai] {
                ai += 1;
                fi += 1;
            } else {
                ok = false; // an extra non-blank line appeared -> this start doesn't match
                break;
            }
        }
        if ok && ai == nb.len() {
            regions.push((start, fi));
        }
    }
    regions
}

/// EP-1 helper: rebuilds a hunk using the file's real region (including blank lines) --
/// anchors (context/`-`) are aligned to the file's bytes, blank lines the model omitted are
/// filled back in (as context), and `+` insertion lines are kept in the hunk's original order.
/// The model's own blank anchor lines are discarded (the file's blank lines are used instead, to avoid duplication).
fn rebuild_hunk_with_region(hunk: &[&str], region: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut fi = 0usize; // region cursor
    for &hl in hunk {
        match hl.chars().next() {
            Some('+') => out.push(hl.to_owned()), // insertion line kept in place as-is
            Some(' ') | Some('-') => {
                let prefix = hl.chars().next().unwrap();
                let content = &hl[1..];
                if content.trim().is_empty() {
                    continue; // discard the model's blank anchor line, use the file's blank line instead
                }
                // first fill back in blank lines the model omitted from the file (as context)
                while fi < region.len() && region[fi].trim().is_empty() {
                    out.push(format!(" {}", region[fi]));
                    fi += 1;
                }
                if fi < region.len() {
                    out.push(format!("{prefix}{}", region[fi]));
                    fi += 1;
                } else {
                    out.push(hl.to_owned());
                }
            }
            _ => {} // unprefixed blank lines etc. are discarded, using the file's blank line instead
        }
    }
    out
}

/// Finds every start position `i` in `file_lines` such that `file_lines[i..i+anchor.len()]`
/// satisfies `eq` line-by-line against `anchor`. Returns all matching start positions.
fn find_block<F: Fn(&str, &str) -> bool>(
    file_lines: &[&str],
    anchor: &[&str],
    eq: F,
) -> Vec<usize> {
    if anchor.is_empty() || anchor.len() > file_lines.len() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for i in 0..=(file_lines.len() - anchor.len()) {
        if (0..anchor.len()).all(|k| eq(file_lines[i + k], anchor[k])) {
            hits.push(i);
        }
    }
    hits
}

/// Resolves a patch path to an absolute path. An absolute path is left as-is; a relative path is joined against `cwd`.
fn resolve_path(path: &str, cwd: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd).join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_file(name: &str, content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, name.to_owned())
    }

    #[test]
    fn extract_cwd_from_env_block() {
        let req = json!({
            "input": [{"type":"message","role":"user","content":"<environment_context>\n  <cwd>/Users/x/proj</cwd>\n  <shell>zsh</shell>\n</environment_context>"}]
        });
        assert_eq!(extract_cwd(Some(&req)).as_deref(), Some("/Users/x/proj"));
        assert_eq!(extract_cwd(None), None);
        assert_eq!(extract_cwd(Some(&json!({"input":[]}))), None);

        // codex-connector #435 P2: Windows path backslashes must not be doubled (walk the Value
        // tree to get the unescaped original text, don't serialize the whole request first).
        // In json!, "C:\\Users\\me\\repo" = the actual single-backslash path.
        let win = json!({
            "input": [{"type":"message","role":"user","content":"<environment_context>\n  <cwd>C:\\Users\\me\\repo</cwd>\n</environment_context>"}]
        });
        assert_eq!(
            extract_cwd(Some(&win)).as_deref(),
            Some(r"C:\Users\me\repo")
        );
    }

    #[test]
    fn trailing_whitespace_anchor_is_repaired_to_file_bytes() {
        // The file's context line has no trailing whitespace; the patch's context line has trailing whitespace -> should be aligned to the file's real bytes.
        let (dir, name) = tmp_file("a.txt", "fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        let cwd = dir.path().to_str().unwrap();
        // patch: adds a line after `let x = 1;`; the context has trailing whitespace (a common model mistake).
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n    let x = 1;   \n+    let z = 9;\n    let y = 2;\n*** End Patch\n"
        );
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert!(
            out.contains("    let x = 1;\n"),
            "trailing whitespace should be aligned away:\n{out}"
        );
        assert!(out.contains("+    let z = 9;"), "the added line is kept");
        assert_eq!(reps[0].kind, "repaired", "{:?}", reps);
    }

    #[test]
    fn exact_match_left_clean() {
        let (dir, name) = tmp_file("b.txt", "alpha\nbeta\ngamma\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Begin Patch\n*** Update File: {name}\n alpha\n-beta\n+BETA\n gamma\n*** End Patch\n");
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert_eq!(reps[0].kind, "clean");
        assert_eq!(out, v4a, "an exact match should not change a single byte");
    }

    #[test]
    fn ambiguous_match_is_skipped_not_guessed() {
        // The anchor ` x` occurs in multiple places in the file -> ambiguous -> passed through, not guessed.
        let (dir, name) = tmp_file("c.txt", "x\nx\nx\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a =
            format!("*** Begin Patch\n*** Update File: {name}\n x   \n+added\n*** End Patch\n");
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert!(reps[0].kind.starts_with("skipped"), "{:?}", reps);
        assert_eq!(out, v4a, "ambiguous, should not change");
    }

    #[test]
    fn no_match_skipped() {
        let (dir, name) = tmp_file("d.txt", "real content\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Begin Patch\n*** Update File: {name}\n model hallucinated line\n+x\n*** End Patch\n");
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert!(reps[0].kind.starts_with("skipped"));
        assert_eq!(out, v4a);
    }

    #[test]
    fn unreadable_file_passes_through() {
        let v4a = "*** Begin Patch\n*** Update File: nonexistent_zzz.txt\n a\n+b\n*** End Patch\n";
        let (out, reps) = preflight_repair(v4a, Some("/tmp/no_such_dir_xyz"));
        assert_eq!(out, v4a);
        assert_eq!(reps[0].kind, "skipped:unreadable");
    }

    #[test]
    fn envelope_added_when_model_omits_begin_end() {
        // Live-traffic seq230 shape: only Add File + content, no Begin/End.
        let body = "*** Add File: outputs/x.md\n+# Title\n+body\n";
        let (out, rep) = ensure_v4a_envelope(body);
        assert!(out.starts_with("*** Begin Patch\n"), "{out}");
        assert!(out.trim_end().ends_with("*** End Patch"), "{out}");
        assert!(
            out.contains("+# Title") && out.contains("+body"),
            "content is not lost"
        );
        assert!(rep.is_some());
    }

    #[test]
    fn envelope_only_end_added() {
        let body = "*** Begin Patch\n*** Add File: x\n+a\n";
        let (out, rep) = ensure_v4a_envelope(body);
        assert_eq!(out.matches("*** Begin Patch").count(), 1, "Begin should not be added twice");
        assert!(out.trim_end().ends_with("*** End Patch"));
        assert!(rep.unwrap().detail.contains("End Patch"));
    }

    #[test]
    fn envelope_prefixed_end_stripped_for_code_file() {
        // MOC-268 (per the user's decision to "strip by file type"): **only `+*** End Patch`**
        // (an Add-line mis-prefixed terminator) is stripped and normalized into a bare
        // terminator in code / structured config files (a bare `*** End Patch` can never
        // legitimately be the last line of real source code), with nothing appended, zero residue, zero content loss.
        for path in ["x.rs", "c.json", "s.toml", "w.vue"] {
            let body = format!("*** Begin Patch\n*** Add File: {path}\n+a\n+b\n+*** End Patch");
            let (out, rep) = ensure_v4a_envelope(&body);
            assert!(
                out.trim_end().ends_with("\n*** End Patch"),
                "a code file should be stripped into a bare terminator ({path}):\n{out}"
            );
            assert!(
                !out.contains("+*** End Patch"),
                "no prefixed terminator should remain ({path}):\n{out}"
            );
            assert_eq!(
                out.matches("*** End Patch").count(),
                1,
                "only one terminator ({path}):\n{out}"
            );
            assert!(rep.unwrap().detail.contains("stripped mis-added prefix"), "{path}");
        }
    }

    #[test]
    fn envelope_deletion_or_context_end_line_not_stripped() {
        // MOC-268 (chatgpt-codex-connector review): ` *** End Patch` (context) / `-*** End Patch`
        // (deletion) are **legitimate Update hunk lines** (e.g. the model using Update to delete
        // a `*** End Patch` left over in the file), and must **never** be stripped as a
        // terminator (stripping a `-` = silently dropping the deletion). These two last lines
        // -> the real terminator is appended normally, and the hunk line is **kept as-is**. Even when the target is a code file.
        for last in ["-*** End Patch", " *** End Patch"] {
            let body = format!("*** Begin Patch\n*** Update File: src/foo.rs\n keep\n{last}");
            let (out, rep) = ensure_v4a_envelope(&body);
            assert!(
                out.contains(last),
                "the legitimate hunk line {last:?} must be kept as-is (not stripped = the deletion is not lost):\n{out}"
            );
            assert!(
                out.trim_end().ends_with("\n*** End Patch"),
                "a real terminator should be appended normally ({last:?}):\n{out}"
            );
            let r = rep.unwrap();
            assert!(
                r.detail.contains("End Patch") && !r.detail.contains("stripped mis-added prefix"),
                "goes through the normal append path, not stripping ({last:?}):{}",
                r.detail
            );
        }
    }

    #[test]
    fn envelope_prefixed_end_left_incomplete_for_doc_file() {
        // MOC-268 (silent-failure review): in doc / text / unknown file types, a bare
        // `*** End Patch` **could be a real body last line** (this very repo's V4A docs contain
        // this exact string) -> ambiguous, never guessed: neither stripped (avoids deleting real
        // content) nor appended (avoids leaving a residual line) -> not completed, left incomplete.
        for path in ["notes.md", "readme.txt", "data"] {
            let body = format!(
                "*** Begin Patch\n*** Add File: {path}\n+How to end a patch:\n+*** End Patch"
            );
            let (out, rep) = ensure_v4a_envelope(&body);
            assert_eq!(out, body, "an ambiguous last line in a doc file should be kept as-is ({path}):\n{out}");
            assert!(
                out.trim_end().ends_with("+*** End Patch"),
                "the real content line should be kept, not deleted ({path}):\n{out}"
            );
            assert!(
                !out.trim_end().ends_with("\n*** End Patch"),
                "no bare terminator should be appended ({path}):\n{out}"
            );
            assert_eq!(
                rep.unwrap().kind,
                "skipped:ambiguous_prefixed_end",
                "{path}"
            );
        }
    }

    #[test]
    fn envelope_prefixed_end_not_stripped_for_update_even_code() {
        // MOC-268 (chatgpt-codex-connector review): `*** Update File:`'s `+*** End Patch` is an
        // **added line** (possibly genuinely adding this string into a code file's string
        // literal / fixture), not Add File's mis-prefixed terminator -> **not stripped even
        // when the target is a code file** (stripping it = dropping the addition), goes to ambiguous -> left incomplete. Stripping is limited to when the last operation is Add File.
        let body =
            "*** Begin Patch\n*** Update File: src/foo.rs\n keep\n+*** End Patch".to_string();
        let (out, rep) = ensure_v4a_envelope(&body);
        assert_eq!(out, body, "Update's +*** End Patch should not be stripped/touched:\n{out}");
        assert!(
            out.trim_end().ends_with("+*** End Patch"),
            "the added line should be kept:\n{out}"
        );
        assert!(
            !out.trim_end().ends_with("\n*** End Patch"),
            "no bare terminator should be appended:\n{out}"
        );
        assert_eq!(rep.unwrap().kind, "skipped:ambiguous_prefixed_end");
    }

    #[test]
    fn envelope_complete_untouched() {
        let body = "*** Begin Patch\n*** Add File: x\n+a\n*** End Patch\n";
        let (out, rep) = ensure_v4a_envelope(body);
        assert_eq!(out, body);
        assert!(rep.is_none());
    }

    #[test]
    fn envelope_not_added_to_nonpatch_or_leading_prose() {
        // A non-patch body is untouched
        let (o1, r1) = ensure_v4a_envelope("just some text\nno ops here\n");
        assert_eq!(o1, "just some text\nno ops here\n");
        assert!(r1.is_none());
        // Begin is missing and the first non-empty line is not an operation line (leading prose) -> unsafe, don't add Begin
        let prose = "here is my patch:\n*** Add File: x\n+a\n*** End Patch\n";
        let (o2, _r2) = ensure_v4a_envelope(prose);
        assert!(
            !o2.starts_with("*** Begin Patch"),
            "should not rashly add Begin when there's leading prose"
        );
    }

    #[test]
    fn strip_trailing_at_double_sided_to_single() {
        let v4a = "*** Begin Patch\n*** Update File: x\n@@ def f(): @@\n-a\n+b\n*** End Patch\n";
        let (out, reps) = strip_trailing_at(v4a);
        assert!(out.contains("@@ def f():\n"), "should strip the trailing @@:\n{out}");
        assert!(!out.contains("@@ def f(): @@"));
        assert_eq!(reps.len(), 1);
    }

    #[test]
    fn strip_trailing_at_keeps_bare_and_single() {
        // Both a bare @@ (section separator) and a single-sided @@ are left untouched
        let v4a = "*** Update File: x\n@@\n@@ class Foo\n-a\n+b\n";
        let (out, reps) = strip_trailing_at(v4a);
        assert_eq!(out, v4a);
        assert!(reps.is_empty());
    }

    #[test]
    fn strip_unified_hunk_ranges_to_bare_at() {
        let v4a = "*** Begin Patch\n*** Update File: x\n@@ -6,2 +6,4 @@\n-a\n+b\n*** End Patch\n";
        let (out, reps) = strip_unified_hunk_ranges(v4a);
        assert!(out.contains("*** Update File: x\n@@\n-a\n+b"));
        assert!(!out.contains("-6,2 +6,4"));
        assert_eq!(reps.len(), 1);
    }

    #[test]
    fn strip_unified_hunk_ranges_keeps_text_anchor() {
        let v4a = "*** Update File: x\n@@ class Foo\n-a\n+b\n";
        let (out, reps) = strip_unified_hunk_ranges(v4a);
        assert_eq!(out, v4a);
        assert!(reps.is_empty());
    }

    #[test]
    fn optimize_patch_converts_unified_diff_headers_to_v4a_update() {
        let v4a = "*** Begin Patch\n--- C:/Users/32057/Documents/Codex/2026-07-05/zai/data_summary.md\n+++ C:/Users/32057/Documents/Codex/2026-07-05/zai/data_summary.md\n@@ -7,4 +7,5 @@\n old\n+new\n*** End Patch\n";
        let (out, reps) = optimize_patch(v4a, None, true);
        assert!(
            out.contains(
                "*** Update File: C:/Users/32057/Documents/Codex/2026-07-05/zai/data_summary.md"
            ),
            "{out}"
        );
        assert!(out.contains("@@\n old\n+new"), "{out}");
        assert!(validate_v4a_for_codex(&out).is_none(), "{out}");
        assert!(
            reps.iter()
                .any(|r| r.detail.contains("unified diff file headers")),
            "{reps:?}"
        );
    }

    #[test]
    fn optimize_patch_converts_file_header_to_v4a_update() {
        let v4a = "*** Begin Patch\nfile: C:\\Users\\32057\\Documents\\Codex\\2026-07-05\\zai\\data_summary.md\n@@\n-old\n+new\n*** End Patch\n";
        let (out, reps) = optimize_patch(v4a, None, true);
        assert!(
            out.contains(
                "*** Update File: C:\\Users\\32057\\Documents\\Codex\\2026-07-05\\zai\\data_summary.md"
            ),
            "{out}"
        );
        assert!(validate_v4a_for_codex(&out).is_none(), "{out}");
        assert!(reps.iter().any(|r| r.detail.contains("file: header")));
    }

    #[test]
    fn validate_v4a_rejects_unified_headers() {
        let v4a = "*** Begin Patch\n*** Update File: x\n--- a/x\n+++ b/x\n@@ -1 +1\n-a\n+b\n*** End Patch\n";
        let err = validate_v4a_for_codex(v4a).expect("unified diff should be rejected");
        assert!(err.1.contains("unified diff file header"));
    }

    #[test]
    fn validate_v4a_rejects_double_envelope() {
        let v4a = "*** Begin Patch\n*** Update File: x\n@@\n-a\n+b\n*** End Patch\n*** Begin Patch\n*** Update File: y\n@@\n-c\n+d\n*** End Patch\n";
        let err = validate_v4a_for_codex(v4a).expect("double envelope should be rejected");
        assert!(
            err.1.contains("repeated *** Begin Patch") || err.1.contains("repeated *** End Patch"),
            "{err:?}"
        );
    }

    #[test]
    fn validate_v4a_rejects_missing_line_prefix() {
        let v4a = "*** Begin Patch\n*** Update File: x\n@@\nold line without prefix\n+new\n*** End Patch\n";
        let err = validate_v4a_for_codex(v4a).expect("missing prefix should be rejected");
        assert!(err.1.contains("line missing V4A prefix"), "{err:?}");
    }

    #[test]
    fn optimize_patch_strips_unified_range_header() {
        let v4a = "*** Begin Patch\n*** Update File: x\n@@ -6 +6\n-a\n+b\n*** End Patch\n";
        let (out, reps) = optimize_patch(v4a, None, true);
        assert!(out.contains("*** Update File: x\n@@\n-a\n+b"));
        assert!(validate_v4a_for_codex(&out).is_none(), "{out}");
        assert!(reps.iter().any(|r| r.file == "(@@ range header)"));
    }

    #[test]
    fn recover_empty_move_to_delete_add() {
        let (dir, name) = tmp_file("old.md", "line1\nline2\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n*** Move to: new.md\n*** End Patch\n"
        );
        let (out, reps) = recover_empty_move(&v4a, Some(cwd));
        assert!(out.contains(&format!("*** Delete File: {name}")), "{out}");
        assert!(out.contains("*** Add File: new.md"), "{out}");
        assert!(
            out.contains("+line1") && out.contains("+line2"),
            "copies the original content:\n{out}"
        );
        assert!(!out.contains("*** Move to:"), "Move has been replaced");
        assert_eq!(reps[0].kind, "repaired");
    }

    #[test]
    fn recover_empty_move_with_hunk_untouched() {
        // Update+Move but **with** a hunk (rename + content change) -> left untouched (allowed by the prompt).
        let (dir, name) = tmp_file("old2.md", "a\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n*** Move to: new2.md\n-a\n+b\n*** End Patch\n"
        );
        let (out, reps) = recover_empty_move(&v4a, Some(cwd));
        assert_eq!(out, v4a, "a Move with a hunk is left untouched");
        assert!(reps.is_empty());
    }

    #[test]
    fn rename_with_eof_marker_hunk_not_treated_as_empty() {
        // codex-connector #435 P1: a rename + `*** End of File` append hunk must not be treated
        // as an empty rename (otherwise it would convert to a content-losing Delete+Add) ->
        // recognized as having a hunk -> passed through, not converted.
        let (dir, name) = tmp_file("eof_old.md", "a\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n*** Move to: eof_new.md\n*** End of File\n+tail\n*** End Patch\n"
        );
        let (out, reps) = recover_empty_move(&v4a, Some(cwd));
        assert_eq!(out, v4a, "a rename with an EOF hunk should pass through unconverted:\n{out}");
        assert!(reps.is_empty(), "{:?}", reps);
    }

    #[test]
    fn add_on_existing_passes_through_unchanged() {
        // Rule #2 has been revoked: Add on an already-existing file **no longer** converts to
        // Delete+Add (avoids overwriting and losing data), it's passed through as-is so Codex
        // reports already exists and the model self-corrects into a targeted Update.
        let (dir, name) = tmp_file("exists.md", "important old content\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Begin Patch\n*** Add File: {name}\n+new content\n*** End Patch\n");
        let (out, reps) = optimize_patch(&v4a, Some(cwd), true);
        assert!(
            !out.contains("*** Delete File:"),
            "should no longer insert a Delete (rule #2 has been revoked):\n{out}"
        );
        assert!(
            out.contains(&format!("*** Add File: {name}")),
            "Add is kept as-is"
        );
        assert!(
            !reps.iter().any(|r| r.detail.contains("Delete File overwrite")),
            "there should be no overwrite-style repair: {:?}",
            reps
        );
    }

    #[test]
    fn update_empty_file_to_delete_add() {
        let (dir, name) = tmp_file("empty.txt", "");
        let cwd = dir.path().to_str().unwrap();
        let v4a =
            format!("*** Begin Patch\n*** Update File: {name}\n+line1\n+line2\n*** End Patch\n");
        let (out, reps) = recover_update_empty_file(&v4a, Some(cwd));
        assert!(out.contains(&format!("*** Delete File: {name}")), "{out}");
        assert!(out.contains(&format!("*** Add File: {name}")), "{out}");
        assert!(out.contains("+line1") && out.contains("+line2"));
        assert!(!out.contains("*** Update File:"), "Update has been converted");
        assert_eq!(reps[0].kind, "repaired");
    }

    #[test]
    fn update_whitespace_only_file_not_converted() {
        // codex-connector #435 P2: a whitespace-only file (not 0 bytes) doesn't count as empty
        // -> not converted to Delete+Add (otherwise the whitespace bytes would be lost).
        let (dir, name) = tmp_file("ws.txt", "  \n\t\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Begin Patch\n*** Update File: {name}\n+line1\n*** End Patch\n");
        let (out, reps) = recover_update_empty_file(&v4a, Some(cwd));
        assert_eq!(out, v4a, "an Update on a whitespace-only file should not convert to Delete+Add:\n{out}");
        assert!(reps.is_empty());
    }

    #[test]
    fn update_nonempty_file_not_converted() {
        let (dir, name) = tmp_file("nonempty.txt", "existing\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n-existing\n+changed\n*** End Patch\n"
        );
        let (out, reps) = recover_update_empty_file(&v4a, Some(cwd));
        assert_eq!(out, v4a, "a non-empty file's Update is untouched");
        assert!(reps.is_empty());
    }

    #[test]
    fn add_file_missing_plus_prefix_is_filled() {
        // An Add File line missing its `+` (a common model mistake), plus a blank line -> both get `+` filled in; a line that already has `+` is untouched.
        let v4a = "*** Begin Patch\n*** Add File: new.md\n+# Title\nplain line no plus\n\n+already plus\n*** End Patch\n";
        let (out, reps) = ensure_add_file_plus(v4a);
        assert!(
            out.contains("\n+plain line no plus\n"),
            "the line missing + should be filled in:\n{out}"
        );
        assert!(out.contains("\n+\n+already plus"), "a blank line -> a bare +:\n{out}");
        assert!(!out.contains("++already plus"), "a line already with + is not duplicated");
        assert_eq!(reps[0].kind, "repaired");
        assert!(
            reps[0].detail.contains("2 line"),
            "the plain line missing + plus the blank line = 2: {:?}",
            reps
        );
    }

    #[test]
    fn add_file_all_plus_untouched_and_update_not_affected() {
        // An all-`+` Add File is untouched; the Update section's non-`+` lines (context/-) are never touched by G.
        let v4a = "*** Begin Patch\n*** Add File: a\n+x\n+y\n*** Update File: b\n cont\n-old\n+new\n*** End Patch\n";
        let (out, reps) = ensure_add_file_plus(v4a);
        assert_eq!(out, v4a, "an all-+ Add plus an untouched Update:\n{out}");
        assert!(reps.is_empty());
    }

    #[test]
    fn at_header_aligned_to_unique_file_line() {
        // Live-traffic seq181: a truncated @@ header (missing `## 6. `), uniquely contained in one file line -> aligned.
        let (dir, name) = tmp_file("doc.md", "intro\n## 6. 系统架构建议\n建议分层\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n@@ 系统架构建议\n 建议分层\n+新增一行\n*** End Patch\n"
        );
        let (out, reps) = align_at_headers(&v4a, Some(cwd));
        assert!(
            out.contains("@@ ## 6. 系统架构建议"),
            "@@ should be aligned to the file's real full line:\n{out}"
        );
        assert_eq!(reps[0].kind, "repaired");
    }

    #[test]
    fn at_header_exact_or_ambiguous_untouched() {
        // Already a real full line in the file -> untouched; contained in multiple places (ambiguous) -> untouched.
        let (dir, name) = tmp_file("doc2.md", "## A\nx\n## A\n");
        let cwd = dir.path().to_str().unwrap();
        // The exact full line `## A` exists, but is ambiguous (two lines) -> untouched
        let v4a = format!("*** Update File: {name}\n@@ ## A\n x\n+y\n");
        let (out, reps) = align_at_headers(&v4a, Some(cwd));
        assert_eq!(out, v4a);
        assert!(reps.is_empty());
        // The substring `A` occurs in both `## A` lines -> ambiguous, untouched
        let v4a2 = format!("*** Update File: {name}\n@@ A\n x\n+y\n");
        let (out2, reps2) = align_at_headers(&v4a2, Some(cwd));
        assert_eq!(out2, v4a2);
        assert!(reps2.is_empty());
    }

    #[test]
    fn unprefixed_dup_of_plus_line_dropped() {
        // Live-traffic seq235: an unprefixed line immediately followed by `+<same content>` -> the stale line is dropped (the content lives in the + line, nothing is lost).
        let (dir, name) = tmp_file("u.md", "other\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Update File: {name}\n*data source*\n+*data source*\n+more\n");
        let (out, reps) = fix_unprefixed_lines(&v4a, Some(cwd));
        assert!(!out.contains("\n*data source*\n"), "the unprefixed stale line should be dropped:\n{out}");
        assert!(out.contains("+*data source*"), "the + line is kept (content not lost)");
        assert_eq!(reps[0].kind, "repaired");
    }

    #[test]
    fn unprefixed_existing_file_line_gets_context_space() {
        // An unprefixed line matches a line in the file -> context missing its space -> fill in ` `.
        let (dir, name) = tmp_file("u2.md", "alpha\nkeepme\nbeta\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Update File: {name}\nkeepme\n+added\n");
        let (out, reps) = fix_unprefixed_lines(&v4a, Some(cwd));
        assert!(out.contains("\n keepme\n"), "should fill in the space to make it context:\n{out}");
        assert_eq!(reps[0].kind, "repaired");
    }

    #[test]
    fn unprefixed_unknown_passes_through() {
        // Not in the file, not a duplicate -> passed through (never guessed).
        let (dir, name) = tmp_file("u3.md", "real\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Update File: {name}\nhallucinated garbage line\n+x\n");
        let (out, reps) = fix_unprefixed_lines(&v4a, Some(cwd));
        assert_eq!(out, v4a, "an unknown unprefixed line is passed through as-is");
        assert!(reps.is_empty());
    }

    #[test]
    fn cwd_candidates_remember_recall_and_resolve() {
        // MOC-263 P1: candidate history (deque) + read_patch_file resolving by candidate (global state, only non-flaky assertions here).
        let (dir, name) = tmp_file("cand_moc263.txt", "x\n");
        let real = dir.path().to_str().unwrap().to_owned();
        // Simulate concurrent pollution: first record a stale cwd that doesn't have this file, then record the real cwd (moving it to the front).
        remember_cwd("/tmp/stale_zzz_moc263_a");
        remember_cwd(&real);
        assert!(recall_cwd_candidates().iter().any(|c| c == &real));
        assert!(has_cwd_candidate(None), "with a candidate history -> true");
        // primary=None (an apply_patch tool-loop request), the real cwd is among the candidates
        // -> the file is read (the stale cwd has no such file, so it's automatically skipped
        // when tried in turn). This is P1's core fix: no longer wiped out by a single stale slot.
        let got = read_patch_file(&name, None, &[(false, "x")]);
        assert!(got.is_some(), "should read the file via a candidate cwd");
        assert_eq!(got.unwrap().1, "x\n");
        // Extract cwd from a turn-start request into the candidates (for later apply_patch requests to fall back to).
        let req = json!({"input":[{"type":"message","role":"user","content":"<environment_context>\n  <cwd>/tmp/ts_proj_b3f9</cwd>\n</environment_context>"}]});
        remember_cwd_from_request(Some(&req));
        assert!(recall_cwd_candidates()
            .iter()
            .any(|c| c == "/tmp/ts_proj_b3f9"));
    }

    #[test]
    fn read_patch_file_picks_by_probe_not_first_readable() {
        // MOC-263 P2 (chatgpt-codex-connector review): when concurrent sessions share a
        // relative path (README.md etc.) and the stale session updates it, the patch anchor
        // probe should pick the candidate that **actually contains the anchor**, not just the
        // first readable one at the front of the queue (most-recent = stale).
        let stale = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir().unwrap();
        std::fs::write(stale.path().join("shared_moc263.txt"), "stale_only_line\n").unwrap();
        std::fs::write(
            real.path().join("shared_moc263.txt"),
            "real_anchor_line\nmore\n",
        )
        .unwrap();
        // The real cwd is recorded first, stale second -> stale is at the front of the queue
        // (most-recent), simulating the scenario the review was concerned about.
        remember_cwd(real.path().to_str().unwrap());
        remember_cwd(stale.path().to_str().unwrap());
        // The probe hits real (contains real_anchor_line), not stale -> real should be picked, not the front-of-queue stale one.
        let got = read_patch_file("shared_moc263.txt", None, &[(false, "real_anchor_line")]);
        assert!(got.is_some(), "should pick the candidate that contains the anchor");
        assert_eq!(
            got.unwrap().1,
            "real_anchor_line\nmore\n",
            "should pick real (contains the probe anchor), not the front-of-queue stale one"
        );
    }

    #[test]
    fn read_patch_file_single_candidate_partial_header_not_skipped() {
        // MOC-263 P2, round two (chatgpt-codex-connector review): the probe only has a
        // truncated `@@` header (no exact line in the file, the real one is
        // `## 6. 系统架构建议`); a **single candidate** should not be judged unreadable just
        // because the probe hit 0 -- the file should still be returned, letting
        // align_at_headers do the substring repair. probe is a tie-breaker, not a gate.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("doc_moc263.md"),
            "intro\n## 6. 系统架构建议\n建议分层\n",
        )
        .unwrap();
        remember_cwd(dir.path().to_str().unwrap());
        let got = read_patch_file("doc_moc263.md", None, &[(true, "系统架构建议")]);
        assert!(
            got.is_some(),
            "a single candidate + a truncated header probe should not be judged unreadable"
        );
        assert!(got.unwrap().1.contains("## 6. 系统架构建议"));
    }

    #[test]
    fn read_patch_file_partial_header_substring_picks_real_over_stale() {
        // MOC-263 P2, round three (chatgpt-codex-connector review): multiple candidates plus a
        // purely truncated `@@` header, exact match all 0 -> fall back to substring scoring,
        // picking the **real one whose substring contains the header**, not blindly taking the
        // front-of-queue stale one (otherwise align would repair against the stale substring).
        let stale = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir().unwrap();
        std::fs::write(
            stale.path().join("doc2_moc263.md"),
            "stale intro\nunrelated heading\n",
        )
        .unwrap();
        std::fs::write(
            real.path().join("doc2_moc263.md"),
            "intro\n## 6. 系统架构建议\n建议分层\n",
        )
        .unwrap();
        remember_cwd(real.path().to_str().unwrap());
        remember_cwd(stale.path().to_str().unwrap()); // stale is at the front of the queue (most-recent)
        let got = read_patch_file("doc2_moc263.md", None, &[(true, "系统架构建议")]);
        assert!(got.is_some());
        assert!(
            got.unwrap().1.contains("## 6. 系统架构建议"),
            "substring scoring should pick the real one containing the header, not the front-of-queue stale one"
        );
    }

    #[test]
    fn read_patch_file_tied_score_is_ambiguous_none() {
        // MOC-263 P2, round four (chatgpt-codex-connector review): two candidates tie on probe
        // score (sharing the same anchor line) -> ambiguous -> None (never guessed), rather than
        // taking the front-of-queue stale one. Downstream skips, the patch passes through for Codex / the model to self-correct.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("tie_moc263.txt"), "SHARED_ANCHOR\naaa\n").unwrap();
        std::fs::write(b.path().join("tie_moc263.txt"), "SHARED_ANCHOR\nbbb\n").unwrap();
        remember_cwd(a.path().to_str().unwrap());
        remember_cwd(b.path().to_str().unwrap());
        let got = read_patch_file("tie_moc263.txt", None, &[(false, "SHARED_ANCHOR")]);
        assert!(got.is_none(), "a tied score (ambiguous) should return None, never guessed");
    }

    #[test]
    fn read_patch_file_header_probe_not_beaten_by_stale_exact_fragment() {
        // MOC-263 P2, round five (chatgpt-codex-connector review): the probe only has a
        // truncated `@@` header; stale happens to have a full line equal to that fragment, and
        // real is a superstring of it (`## 6. X`). Headers are scored by **substring** (never
        // exact) -> both candidates hit as a substring -> tied -> None, and it is **not**
        // wrongly picked based on stale's exact full-line match.
        let stale = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir().unwrap();
        std::fs::write(stale.path().join("h_moc263.md"), "系统架构建议\nx\n").unwrap();
        std::fs::write(real.path().join("h_moc263.md"), "## 6. 系统架构建议\ny\n").unwrap();
        remember_cwd(real.path().to_str().unwrap());
        remember_cwd(stale.path().to_str().unwrap()); // stale is at the front of the queue
        let got = read_patch_file("h_moc263.md", None, &[(true, "系统架构建议")]);
        assert!(
            got.is_none(),
            "a tied header substring should return None, not wrongly picked based on stale's exact full-line match"
        );
    }

    #[test]
    fn context_line_starting_with_at_at_does_not_block_split() {
        // MOC-263 P2, round five (chatgpt-codex-connector review): a context line whose content
        // starts with @@ (` @@ ...`, column 0 is a space) should not be mistaken for a hunk
        // separator and disable auto-split (the splitter only recognizes @@ at column 0). A
        // multi-region pure deletion containing such a context line should still split.
        let content = "@@ banner\nkeep_a\nREMOVE_1\nmid1\nmid2\nmid3\nREMOVE_2\nkeep_b\n";
        let (dir, name) = tmp_file("atat_moc263.txt", content);
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n @@ banner\n keep_a\n-REMOVE_1\n mid1\n-REMOVE_2\n keep_b\n*** End Patch\n"
        );
        let (out, _reps) = preflight_repair(&v4a, Some(cwd));
        assert!(
            out.contains("\n@@\n"),
            "a multi-region deletion containing a ` @@` context line should still auto-split (decided by column 0):\n{out}"
        );
    }

    #[test]
    fn anchor_probe_includes_unprefixed_lines() {
        // MOC-263 P2, round six (chatgpt-codex-connector review): an unprefixed line (which
        // fix_unprefixed_lines repairs by matching the full line against the file) must also go
        // into the probe, otherwise when it's the only anchor, the empty-probe path would pick
        // the wrong file among same-named candidates. `+` / `***` don't go in.
        let body = vec![
            " ctx",
            "-del",
            "+add",
            "@@ hdr",
            "unprefixed line",
            "*** End Patch",
        ];
        let p = anchor_probe(&body);
        assert!(p.contains(&(false, "ctx")), "the context line goes into the probe");
        assert!(p.contains(&(false, "del")), "the deletion line goes into the probe");
        assert!(p.contains(&(true, "hdr")), "the @@ header goes into the probe (as a header)");
        assert!(
            p.contains(&(false, "unprefixed line")),
            "an unprefixed line should be an exact probe"
        );
        assert!(!p.iter().any(|(_, t)| *t == "add"), "a + added line does not go into the probe");
        assert!(
            !p.iter().any(|(_, t)| t.starts_with("***")),
            "a *** control line does not go into the probe"
        );
    }

    #[test]
    fn preflight_aligns_via_candidate_cwd_with_none_primary() {
        // MOC-263 P1 end-to-end: the apply_patch request has cwd=None, and the real cwd exists
        // only in the candidate history -> disk-read alignment should still work.
        let (dir, name) = tmp_file("p1_e2e_moc263.txt", "alpha\nbeta\ngamma\n");
        let real = dir.path().to_str().unwrap().to_owned();
        remember_cwd(&real);
        // The context has trailing whitespace (needs disk-read alignment); primary=None, relying on the candidate history to find the real file.
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n beta   \n+inserted\n*** End Patch\n"
        );
        let (out, reps) = preflight_repair(&v4a, None);
        assert!(
            out.contains("\n beta\n"),
            "should align trailing whitespace via a disk read through the candidate cwd:\n{out}"
        );
        assert!(out.contains("+inserted"), "the added line is kept");
        assert_eq!(reps[0].kind, "repaired", "{:?}", reps);
    }

    #[test]
    fn multi_hunk_pure_delete_no_at_auto_split() {
        // MOC-263 P0: multiple non-contiguous **pure deletion/context** regions packed into one
        // Update File block, with no @@ -> safely auto-split and a bare @@ inserted.
        // (multi-region with an inserted `+` is not split here due to ambiguous landing spot, see mixed_replace_insert_gap_passthrough)
        let content = "keep_top\nREMOVE_1\nmiddle\nmiddle2\nmiddle3\nREMOVE_2\nkeep_bottom\n";
        let (dir, name) = tmp_file("multi_del.txt", content);
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n keep_top\n-REMOVE_1\n middle\n-REMOVE_2\n keep_bottom\n*** End Patch\n"
        );
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert!(out.contains("\n@@\n"), "two non-contiguous deletion regions should get a bare @@ inserted:\n{out}");
        assert_eq!(reps[0].kind, "repaired", "{:?}", reps);
        assert!(reps[0].detail.contains("auto-split by file position"), "{:?}", reps);
        assert!(
            out.contains("-REMOVE_1") && out.contains("-REMOVE_2"),
            "the deletion lines are kept:\n{out}"
        );
    }

    #[test]
    fn mixed_replace_insert_gap_passthrough() {
        // MOC-263 P0 safety (raised by chatgpt-codex-connector review): a mix of "replace +
        // extra insertion" between segments -> the `+`'s landing spot is ambiguous
        // (`+return 42` is a replacement, `+@memoize` is a lead-in line for the next segment
        // `def beta`, and they can't be told apart) -> not split, passed through as-is, to
        // avoid silently inserting @memoize after return (misplaced). Not exempted even when the previous segment's last anchor is a `-` deletion.
        let content = "alpha\nreturn 1\n# gap\ndef beta():\n";
        let (dir, name) = tmp_file("mixed.py", content);
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n-return 1\n+return 42\n+@memoize\n def beta():\n*** End Patch\n"
        );
        let (out, _reps) = preflight_repair(&v4a, Some(cwd));
        assert!(
            !out.contains("\n@@\n"),
            "a mixed replace+insert with an ambiguous landing spot should not be split:\n{out}"
        );
        assert!(
            out.contains("+@memoize") && out.contains("+return 42"),
            "content is not lost"
        );
    }

    #[test]
    fn single_contiguous_hunk_not_split() {
        // A single contiguous hunk -> not split (group<2 -> None), goes through the regular alignment.
        let (dir, name) = tmp_file("single.txt", "a\nb\nc\nd\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a =
            format!("*** Begin Patch\n*** Update File: {name}\n a\n b\n+x\n c\n*** End Patch\n");
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert!(!out.contains("\n@@\n"), "a single contiguous hunk should not have @@ inserted:\n{out}");
        assert!(!reps[0].detail.contains("split"), "{:?}", reps);
    }

    #[test]
    fn ambiguous_multi_region_passthrough() {
        // The anchor content repeats in the file (ambiguous) -> longest_unique_block returns None -> not split, passed through (never guessed, never dropped).
        let (dir, name) = tmp_file("amb.txt", "x\ny\nx\ny\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a =
            format!("*** Begin Patch\n*** Update File: {name}\n-x\n+X\n-y\n+Y\n*** End Patch\n");
        let (out, _reps) = preflight_repair(&v4a, Some(cwd));
        assert!(!out.contains("\n@@\n"), "ambiguous, should not split:\n{out}");
    }

    #[test]
    fn greedy_split_bails_when_first_anchor_not_globally_unique() {
        // MOC-263 P1 (chatgpt-codex-connector review): a hidden ambiguity where **the block is
        // unique but the segment's first anchor is not** -- the file has a stale ALPHA/BETA/GAMMA
        // block plus the real ALPHA/BETA…gap…GAMMA/DELTA region. The body's
        // ` ALPHA/-BETA/ GAMMA/-DELTA` has [ALPHA,BETA,GAMMA] uniquely occurring as a contiguous
        // block only in the stale block, so the greedy algorithm would pick the stale block and
        // delete BETA from the **wrong block**; meanwhile the segment's first anchor ALPHA
        // occurs twice in the file = an ambiguous start point. After the fix, a non-globally-unique
        // first anchor bails out, not splitting, passing through as-is (never guessed, never dropped).
        let content = "ALPHA\nBETA\nGAMMA\nmid_x\nmid_y\nALPHA\nBETA\nsep_gap\nGAMMA\nDELTA\n";
        let (dir, name) = tmp_file("greedy_moc263.txt", content);
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n ALPHA\n-BETA\n GAMMA\n-DELTA\n*** End Patch\n"
        );
        let (out, _reps) = preflight_repair(&v4a, Some(cwd));
        assert!(
            !out.contains("\n@@\n"),
            "a non-globally-unique segment start anchor (ambiguous start point) should bail and not split, avoiding deleting from the wrong block:\n{out}"
        );
    }

    #[test]
    fn floating_add_after_context_passthrough_not_misplaced() {
        // MOC-263 P0 safety guard: a `+` floating between two non-contiguous regions, where the
        // previous segment's last anchor is context (not a `-` deletion) -> the landing spot is
        // ambiguous (could belong to the previous segment's tail insertion, or be a lead-in line
        // for the next segment) -> not split (otherwise +@memoize would be inserted at the wrong
        // spot, a silent incorrect apply). This is the BLOCKER regression the pre-push review caught.
        let content =
            "def alpha():\n    return 1\n# --- section break ---\ndef beta():\n    return 2\n";
        let (dir, name) = tmp_file("deco.py", content);
        let cwd = dir.path().to_str().unwrap();
        // The model thinks `return 1` and `def beta():` are adjacent and adds +@memoize between them; in reality a section break separates them.
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n     return 1\n+@memoize\n def beta():\n*** End Patch\n"
        );
        let (out, _reps) = preflight_repair(&v4a, Some(cwd));
        assert!(
            !out.contains("\n@@\n"),
            "a floating + with an ambiguous landing spot should not be split (guards against a silent incorrect apply):\n{out}"
        );
        assert!(out.contains("+@memoize"), "content is not lost");
    }

    #[test]
    fn blank_line_drift_block_realigned() {
        // EP-1 live-traffic seq111: the model's context block omitted a blank line present in
        // the file -> the whole block mismatched. Uniquely located while ignoring blank lines ->
        // rebuilt (filling back in the file's blank line + aligning bytes), with `+` insertions kept in place.
        let (dir, name) = tmp_file(
            "main.py",
            "from a import x\nfrom b import y\n\nfrom c import z\nfrom d import w\n",
        );
        let cwd = dir.path().to_str().unwrap();
        // The patch's context omits the blank line between `from b` and `from c`, and wants to insert a line after `from d`.
        let v4a = format!(
            "*** Begin Patch\n*** Update File: {name}\n from a import x\n from b import y\n from c import z\n from d import w\n+from e import v\n*** End Patch\n"
        );
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert!(out.contains("+from e import v"), "the inserted line is kept:\n{out}");
        // After rebuilding, the context block should contain the filled-back-in blank line (a bare ' ').
        assert!(out.contains("\n \n"), "the file's blank line should be filled back in as context:\n{out}");
        assert_eq!(reps[0].kind, "repaired", "{:?}", reps);
    }

    #[test]
    fn blank_tolerant_skips_blank_line_deletion() {
        // A `-` that means "delete a blank line" -> a blank-tolerant rebuild can't faithfully represent it -> passed through unchanged (not silently converted to context).
        let (dir, name) = tmp_file("bd.txt", "x\ny\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Update File: {name}\n x\n-\n y\n+z\n");
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert_eq!(out, v4a, "a blank-line deletion should be passed through unchanged:\n{out}");
        assert!(reps[0].kind.starts_with("skipped"), "{:?}", reps);
    }

    #[test]
    fn blank_tolerant_ambiguous_passthrough() {
        // An exact mismatch (the file has a blank line between p/q that the patch omitted) but
        // ignoring blank lines gives **multiple** matches -> ambiguous, passed through, never guessed.
        let (dir, name) = tmp_file("dup.txt", "p\n\nq\nX\np\n\nq\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!("*** Update File: {name}\n p\n q\n+r\n");
        let (out, reps) = preflight_repair(&v4a, Some(cwd));
        assert_eq!(out, v4a, "ambiguous (multiple matches ignoring blank lines), should not change:\n{out}");
        assert!(reps[0].kind.starts_with("skipped"), "{:?}", reps);
    }

    #[test]
    fn optimize_pipeline_fixes_multiple_issues() {
        // A single patch simultaneously: missing envelope + both-sided @@ + trailing-whitespace context -> all fully recovered.
        let (dir, name) = tmp_file("multi.txt", "fn main() {\n    let x = 1;\n}\n");
        let cwd = dir.path().to_str().unwrap();
        let v4a = format!(
            "*** Update File: {name}\n@@ fn main() {{ @@\n    let x = 1;   \n+    let y = 2;\n"
        );
        let (out, reps) = optimize_patch(&v4a, Some(cwd), true);
        assert!(out.starts_with("*** Begin Patch\n"), "envelope filled in:\n{out}");
        assert!(out.trim_end().ends_with("*** End Patch"), "End filled in:\n{out}");
        assert!(out.contains("@@ fn main() {\n"), "both-sided @@ converted to single-sided:\n{out}");
        assert!(out.contains("    let x = 1;\n"), "trailing whitespace aligned:\n{out}");
        assert!(out.contains("+    let y = 2;"), "the added line is kept");
        // At least 3 kinds of repair should all be recorded
        let kinds: Vec<&str> = reps.iter().map(|r| r.kind.as_str()).collect();
        assert!(
            kinds.iter().filter(|k| **k == "repaired").count() >= 2,
            "{:?}",
            reps
        );
    }

    #[test]
    fn add_file_untouched_no_cwd_noop() {
        let v4a = "*** Begin Patch\n*** Add File: new.txt\n+hello\n*** End Patch\n";
        // No Update File -> short-circuits and returns as-is (even given a cwd).
        let (out, reps) = preflight_repair(v4a, Some("/tmp"));
        assert_eq!(out, v4a);
        assert!(reps.is_empty());
        // No cwd -> as-is
        let (out2, reps2) = preflight_repair(v4a, None);
        assert_eq!(out2, v4a);
        assert!(reps2.is_empty());
    }
}
