# Parent and PR review refresh — 2026-11-12

The best fresh integration is **PR #3928's missing configuration-migration flag**, followed by its Linux resource-limit fix adapted for both executable entrypoints. Empty-pool diagnostics from **#3924** are useful, but its implementation needs substantial narrowing around this worktree's stronger account holds and managed-file boundaries. Two new feature/fix PRs contain destructive placeholder files and must be excluded as submitted.

This is a read-only review of remote metadata, actual PR patches, pinned raw blobs and the **current uncommitted working tree**, not just `HEAD`. No application tests, builds, merges, remote comments, source edits or authenticated Google calls were performed. This new report is the only repository file written for this refresh.

## Snapshot and coverage

Remote metadata was refreshed at approximately **22:38–22:46 UTC**. Connected GitHub tools supplied branch names, PR metadata, full changed-file patches, raw file contents, reviews and commit status. Read-only `gh api` calls filled connector gaps: branch head SHAs, complete open-PR enumeration, check-run details, exact file deletion counts and repository PR totals.

| Item | Current result |
| --- | --- |
| Local `main` HEAD | `abbb4da87c751f2797a5940317f6cb889837e0b4`, plus the existing uncommitted hardening changes |
| Origin `main` | `abbb4da87c751f2797a5940317f6cb889837e0b4` |
| Parent `main` | `9b82fb7ef51c42e9d1662a14bd5b718da0525cd8`, unchanged from the earlier inventory |
| Parent `beta` | `3f759ae9a36fb0b33427fe4f8a4acded4a862372`, unchanged |
| Parent branches | 14 live branches |
| Parent PRs | 590 total, **8 open**, versus one open in the earlier inventory |
| Origin PRs | **0 total**, including closed PRs |
| Parent-only commits relative to local HEAD | `main`: 0; `beta`: 26 by ancestry |

The 26 beta-only commit objects do **not** mean 26 missing functional fixes: sticky sessions, QoS context, transport/upload handling, skills pass-through and related changes have already been adapted into this worktree. Parent `main...beta` itself is diverged: beta is 94 commits ahead and three behind main; a bulk merge would be misleading and unsafe.

All eight currently open parent PRs were inspected: **#3898, #3921, #3923, #3924, #3925, #3926, #3927, #3928**. Actual changed-file patches were read for every newly opened PR. The seven new heads returned **no commit-status contexts, no check runs and no formal reviews** through the queried parent-repository APIs. Author-described local validation is not independently verified CI evidence. These results do not establish whether private or unlisted checks exist.

## Ranked integrations

### 1. Persist the migration marker — high priority, small change

Source: [PR #3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928), author **koeria**, head `4d78c2fc867b96315a89aa3d2620e31796d7c549`, open/non-draft, linked [issue #3918](https://github.com/lbjlaq/Antigravity-Manager/issues/3918).

**Verified local defect:** `modules/config.rs:199–236` reads `thinking_budget_32k_legacy_migrated`, defaults a missing value to false, inserts true and sets `modified = true`. `ThinkingBudgetConfig` in `proxy/config.rs:644` declares `flash_high_legacy_migrated`, but does not declare the 32k marker and has no flatten field. Conversion to typed `AppConfig` drops the unknown marker; saving cannot preserve it. `load_app_config_locked` then rewrites the configuration on each applicable load.

The deterministic source/serialization projection, using only a synthetic object, is:

```text
after migration: {flash_mode: "default", flash_high: -1,
                  thinking_budget_32k_legacy_migrated: true}
typed serialization: {flash_mode: "default", flash_high: -1, ...}
next load: missing marker -> false -> modified=true -> save again
```

The projection was checked by extracting the real struct's declared fields and projecting synthetic JSON through that field set. No Rust execution or real configuration/credential reads were used for this proof.

**Native-store implication:** repeated configuration writes are verified; unconditional native-secret generation churn is **not**. Current `modules/config_secrets.rs:206–212` reuses an existing vault reference when the secret payload is unchanged. Each unnecessary save still performs native-store comparison and an atomic configuration write at line 218; normal loading also hydrates protected configuration. Rotation occurs only when reuse fails or secret values change. The new metadata-only admission-policy loader avoids full configuration loading, but other application configuration readers still encounter the migration defect.

**Accept:** the internal serde-defaulted boolean and its default initialization. **Adapt:** ensure the marker survives GUI save/normalization as well as desktop and headless loads. Preserve `CONFIG_IO_LOCK`, private atomic writes and native-secret reuse. No user-facing toggle is appropriate for an internal migration marker.

**Readiness/cost:** narrow and directly applicable; remote head has no observed checks/reviews. A future focused validation should confirm first migration writes once, a second read performs no migration write, and user-selected budgets are not repeatedly overwritten. Those checks were not run in this review.

### 2. Extend file-descriptor setup to Linux and the standalone server — medium priority

Same [PR #3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928), author/head above. Its other actual change replaces two macOS-only cfg guards with `#[cfg(unix)]`.

**Verified local gap:** `lib.rs:433–455` contains a bounded soft-limit increase to `min(4096, hard_limit)`, but both the helper and its call at line 554 are macOS-only. Linux therefore receives whatever limit the launcher/service provides. The issue's specific 1024-FD limit and GLib crash explanation are reported observations, not universally established defaults or locally reproduced crashes.

**Necessary local adaptation:** simply applying the two cfg substitutions would miss `antigravity-server`: `src/bin/server.rs:77–78` invokes `run_headless_server` directly, bypassing the desktop `run()` initialization containing the current limit call. Place resource initialization on a shared path used by desktop, `--headless` and the standalone server, without performing it twice. Preserve the hard ceiling and fail gracefully when the OS denies an increase. Existing Windows behavior should remain unaffected.

**Readiness/cost:** small Unix-specific change, but Linux/macOS behavior and both executable entrypoints remain unverified here. Keep this independently revertible from the migration-field fix. No native packaging matrix or cloud platform builds are needed to adopt the source change.

### 3. Explain permanently unavailable account pools — medium priority, adapt the design

Source: [PR #3924](https://github.com/lbjlaq/Antigravity-Manager/pull/3924), author **koeria**, head `1ca1abcb3a88a1bc74f2ce0705f627a520a07395`, open/non-draft, linked [issue #3919](https://github.com/lbjlaq/Antigravity-Manager/issues/3919).

**Still missing locally:** five `TokenManager` paths at lines 4244, 4321, 4432, 4598 and 4610 return the generic `Token pool is empty`. The new durable global stop already makes newly observed restrictions actionable, so the remaining value is legacy/restarted pools, all-disabled pools and account-specific manual holds when global halting is disabled or acknowledged.

**Do not import unchanged:** the source synchronously enumerates arbitrary `accounts/*.json` files, silently ignores read/parse failures, labels every `quota.is_forbidden` as a ban and does not model this fork's `manual_attention_required:` holds, explicit `banned` evidence, trusted account membership or native-store unavailability. It can even claim that all accounts are banned while displaying a positive available count. Its textual policy alias does not itself give permanent unavailability a distinct HTTP status.

**Accept:** distinguish permanent/manual attention, intentionally disabled accounts, no configured accounts, not-loaded records and transient provider limits. **Adapt:** read only managed/indexed identities through existing validated raw-metadata boundaries; use bounded/cached diagnostics or the blocking pool, avoid vault hydration and never scan arbitrary files on every failed token attempt. Return shared typed attention errors for permanent holds instead of telling clients to wait for a 429 cooldown. Preserve global halt precedence, generation fencing, no rotation on account attention, and real retry delays for transient limits.

**Readiness/cost:** the objective is worthwhile, but a narrow reimplementation against current safety types is safer than this patch. Validate old forbidden-but-not-confirmed-banned records, manual-only holds, global halt, unreadable metadata and a disk pool containing both enabled and banned accounts. No such tests were executed here.

### 4. Repair generated punctuation at its construction boundary — low priority, partial accept only

Source: [PR #3921](https://github.com/lbjlaq/Antigravity-Manager/pull/3921), author **koeria**, head `f2143689a5a35c8e449f1a82a5c5d22e266b00a4`, open/non-draft, linked [issue #3915](https://github.com/lbjlaq/Antigravity-Manager/issues/3915).

Local `prompt_sanitizer.rs:473,481` appends a comma to an identity constant that already ends in a period. Trimming that generated trailing period before appending the comma is a deterministic formatting correction, independent of any provider enforcement claim.

**Accept only:** the two source-boundary joins and their focused punctuation assertions if the existing normalization behavior is retained. **Drop:** the new global `\.[,，]` replacement across normalized system text. It also matches valid prose such as `etc., if needed`; the source comment claiming this normal form is untouched is inconsistent with its regex. Preserve code-block protection and client/user text boundaries. Do not bundle default-on identity rewriting or assert that punctuation changes prevent Google's private detection or account restrictions; the linked reports cannot establish that causal guarantee.

**Readiness/cost:** tiny partial change, no observed head checks/reviews, but lower value than the persistence and diagnostic fixes. This is formatting maintenance, not an enforcement-bypass recommendation.

## Hard exclusions: descriptions do not match the submitted files

### PR #3923 — do not import any file as submitted

[PR #3923](https://github.com/lbjlaq/Antigravity-Manager/pull/3923), author **koeria**, head `c0666dcff9a1395cdc91da126b4d1e3126da097b`, claims bounded OAuth exchange retries. The actual changed files are:

| File | Deleted lines | Actual one-line replacement |
| --- | ---: | --- |
| `src-tauri/src/modules/mod.rs` | 50 | `@src-tauri/src/modules/mod.rs` |
| `src-tauri/src/modules/oauth.rs` | 806 | `@src-tauri/src/modules/oauth.rs` |
| `src-tauri/src/modules/oauth_retry.rs` | 0; new file | `@src-tauri/src/modules/oauth_retry.rs` |

The [pinned OAuth blob](https://github.com/koeria/Antigravity-Manager/blob/c0666dcff9a1395cdc91da126b4d1e3126da097b/src-tauri/src/modules/oauth.rs) independently confirms the placeholder. These are invalid Rust source contents, not a functioning retry patch. Author-reported successful checks cannot validate these submitted blobs.

**Actionable recovery:** request a corrected original PR containing real source and inspect its new SHA. Even after correction, challenge the description's assumption that every timeout or 5xx means the authorization code was unconsumed. OAuth codes are single-use and replay can invalidate issued tokens; a lost response does not establish non-delivery. Review bounded retries with actual delivery ambiguity, fixed client/redirect identity, existing rejection handling and secret-safe diagnostics. [RFC 6749 §4.1.2](https://www.rfc-editor.org/rfc/rfc6749#section-4.1.2) supports the single-use constraint; it does not endorse the PR's blanket retry claim.

### PR #3927 — do not import any file as submitted

[PR #3927](https://github.com/lbjlaq/Antigravity-Manager/pull/3927), author **koeria**, head `59abba6178322d911fc58690795b8a22e6230306`, claims Gemini Hydrogen catalog support. Its actual files are:

| File | Deleted lines | Actual one-line replacement |
| --- | ---: | --- |
| `src-tauri/resources/official_models.json` | 2458 | `@official_models.json` |
| `src-tauri/src/proxy/cli_sync.rs` | 2534 | `@cli_sync.rs` |
| `src-tauri/src/proxy/common/model_mapping.rs` | 1769 | `@model_mapping.rs` |
| `src/config/modelConfig.ts` | 530 | `@modelConfig.ts` |
| `src/hooks/useProxyModels.tsx` | 313 | `@useProxyModels.tsx` |

The [catalog blob](https://github.com/koeria/Antigravity-Manager/blob/59abba6178322d911fc58690795b8a22e6230306/src-tauri/resources/official_models.json) and [CLI blob](https://github.com/koeria/Antigravity-Manager/blob/59abba6178322d911fc58690795b8a22e6230306/src-tauri/src/proxy/cli_sync.rs) independently confirm invalid JSON/source. One patch entry was null, so reading the raw catalog blob was essential. There is no usable model integration in this head.

**Actionable recovery:** request corrected source, then derive model capabilities and supported effort levels from authoritative provider/account discovery. Preserve current generic variant routing, model-spec limits, CLI/UI parity and per-account authorization. Do not add advertised IDs or guessed model limits based solely on the PR description. No release/model-availability claim was independently verified in this review.

## Already covered or intentionally deferred

| Source | Current-tree comparison and decision |
| --- | --- |
| [#3925 skills pass-through](https://github.com/lbjlaq/Antigravity-Manager/pull/3925), head `bfc6927b3432615bcb04eb4c3678aafc738cb599` | Already implemented: injection helper/call are absent; `context_blocks.rs` includes `preserves_skills_blocks_verbatim_without_injecting_shell_command`. Do not reapply. |
| [#3926 EOF tail flush](https://github.com/lbjlaq/Antigravity-Manager/pull/3926), head `b8862658bb1b3c89514b46b49e7cd1a8ccd3ec60` | Already implemented more defensively: `stream_lifecycle.rs:364–375` flushes the tail before the no-content guard and checks terminal state. Existing `first_content_or_terminal_line_without_newline_survives_eof` covers content and terminal tails. Preserve bounded buffering, attention errors and terminal semantics; do not duplicate the flush. |
| [#3898 cleanup bundle](https://github.com/lbjlaq/Antigravity-Manager/pull/3898), head `a4568358b2a54a29b89fd851fa935ab0da2d6a49` | Still draft, explicitly blocked, conflicted, 57 changed files. Some redundant estimation/marker machinery remains locally, but a broad removal risks current tenant state, diagnostics, request scopes and safety gates. Seek an isolated original performance patch with preserved behavior. |
| [#3913 sanitizer/429 bundle](https://github.com/lbjlaq/Antigravity-Manager/pull/3913) and parent beta defaults | Not a fresh merge and not automatically safe: avoid importing default-on identity/prompt rewriting or claimed private WAF classifications alongside unrelated robustness work. Existing local safety/hold/admission logic must take precedence. |
| Parent headless/Termux branch | Existing headless entrypoints and strengthened argument/auth/TLS handling already cover the relevant server objectives. Android packaging and platform-build additions are outside the user's cloud-build preference; no fresh ready integration established. |
| Parent IDE-switch and sliding-quota revert branches | Heads are unchanged and old. Their isolated nonancestor commits do not justify replacing current credential persistence/integration or protocol policy wholesale. No new ready candidate established. |

Four integration opportunities are ranked above; the remaining PRs are duplicates, broken submissions or deliberately deferred broad changes. Preserve original PR attribution and source SHA when any later implementation is authorized. Nothing in the source reports establishes Google's private account-enforcement rules or a safe request threshold.
