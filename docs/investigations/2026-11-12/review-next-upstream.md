# Parent review after the five fixes — 2026-11-12

No new upstream commits or PR heads appeared since the preceding refresh. The current worktree already contains the migration-marker fix and the image, TLS, monitor and Host protections. The worthwhile remaining upstream work is narrow: Unix resource initialization, removal of discarded token-estimation passes, better empty-pool diagnostics, and a small punctuation construction correction. None justifies merging parent beta or the blocked cleanup PR wholesale.

This review used live GitHub metadata and actual patches, compared with the **uncommitted current tree**, during approximately **23:27–23:31 UTC**. It performed no application changes, tests, builds, Google calls or remote mutations. This report is the only repository file written for this review.

## Verified snapshot

| Item | Result |
| --- | --- |
| Local HEAD and origin main | `abbb4da87c751f2797a5940317f6cb889837e0b4`, plus existing uncommitted changes |
| Parent main | `9b82fb7ef51c42e9d1662a14bd5b718da0525cd8` — unchanged |
| Parent beta | `3f759ae9a36fb0b33427fe4f8a4acded4a862372` — unchanged |
| Parent branches | 14; live branch list refreshed |
| Parent open PRs | Eight: #3898, #3921, #3923, #3924, #3925, #3926, #3927, #3928; all heads unchanged |
| Origin PRs | Zero, including closed PRs |
| Main versus beta | Diverged: beta is 94 commits ahead and three behind main |

Connected GitHub tools supplied branch names, PR search/metadata, complete changed-file patches, review submissions and commit status. Read-only `gh api` calls filled branch-SHA, complete open-list and check-run gaps. The current ancestry counts are not a list of missing behavior: many parent changes are already adapted locally, together with stronger safety and credential handling.

## Ranked remaining candidates

### 1. Unix file-descriptor setup on every executable entrypoint

**Source:** [PR #3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928), **koeria**, head `4d78c2fc867b96315a89aa3d2620e31796d7c549`; [issue #3918](https://github.com/lbjlaq/Antigravity-Manager/issues/3918). Open, non-draft, reported mergeable. No returned commit-status contexts, check runs or reviews for this head.

**Actual patch:** changes the helper and invocation in `src-tauri/src/lib.rs` from macOS-only cfg guards to `#[cfg(unix)]`; separately adds the migration boolean to `ThinkingBudgetConfig`.

**Still missing locally:** `lib.rs:433–455` keeps the bounded resource-limit helper macOS-only, and its invocation at line 554 is still macOS-only. The standalone server calls `run_headless_server` directly at `src/bin/server.rs:77`, bypassing `run()` and its resource initialization. Simply applying the two upstream cfg changes would therefore leave the standalone server untreated.

**Accept/adapt:** share the bounded initialization between desktop startup, desktop `--headless` and `antigravity-server`, ensuring each startup performs it once. Retain `min(4096, hard_limit)`, never lower an already higher soft limit, and fail gracefully if the OS rejects the increase. Correct the existing success log, which says “hard file limit” although only `rlim_cur` changes. Windows remains unaffected.

**Benefit and cost:** low implementation cost; can avoid descriptor exhaustion when a Unix launcher supplies a restrictive soft limit. A universal Linux default of 1024, the reported crash mechanism and a measured benefit in this environment were **not** established. Descriptor count is an OS resource control, not permission to increase provider request budgets or concurrency. Linux/macOS runtime behavior and both executable paths require future focused verification.

**Already covered:** the boolean half of #3928 is present at `proxy/config.rs:663,758`. `modules/config.rs:347–354` preserves it across settings saves, and the source includes `thinking_32k_migration_is_durable_and_does_not_repeat_after_settings_saves` at line 381. Do not reapply or continue reporting the old repeated-migration defect as outstanding.

### 2. Remove only the discarded post-transit estimation passes

**Source:** [PR #3898](https://github.com/lbjlaq/Antigravity-Manager/pull/3898), **cubelikeplayDaniel**, head/isolated cleanup commit `a4568358b2a54a29b89fd851fa935ab0da2d6a49`. The PR remains **draft, explicitly blocked and conflicted**, with 57 changed files. Its current head has eight successful checks, including Rust/frontend/platform builds, and zero returned formal reviews. Those existing results do not validate an adaptation against this worktree; no new cloud builds were requested or started.

**Actual patch and local proof:** the patch removes `apply_post_transit_context_mgmt` calls from the four adapter paths. Locally, `mappers/context_manager.rs:909–913` only forwards to `estimate_gemini_token_usage`; that implementation at lines 733–813 reads the payload, walks strings/tools and serializes tool arguments/results into temporary strings, then returns a count. It neither mutates the payload nor publishes metrics. Every production caller discards the return value:

- OpenAI Chat and Responses: `handlers/openai.rs:2682,4903`.
- Claude: `handlers/claude.rs:2053`.
- Gemini: `handlers/gemini.rs:415`.

**Accept narrowly:** remove these four unused calls, leaving the shared pipeline estimator, its QoS/context-budget consumers and other ContextManager methods intact. OpenAI/Gemini subsequently run the real pipeline estimation; Claude also has retained pipeline-estimation consumers elsewhere. This is a cleanup of a result with no consumer, not a tokenizer replacement.

**Drop/defer:** all unrelated module deletions, session/rotation changes, marker queues, cache changes, health-score scheduling removal and large test rewrites from the same PR. The source's whole-prompt cache and latency claims are not necessary to justify this narrow change and were not independently measured.

**Benefit and cost:** low adaptation cost with one unnecessary text/tool traversal removed per affected attempt. Large tool results make the extra temporary serializations concrete allocation work. No benchmark speedup is claimed. Preserve serialized payload bytes, existing estimates and session/attention/admission behavior; a focused equivalence regression is appropriate. This is a proposed isolated change requiring normal review, **not** a recommendation to merge the blocked PR unchanged.

### 3. Distinguish empty, disabled, held and unloaded account pools

**Source:** [PR #3924](https://github.com/lbjlaq/Antigravity-Manager/pull/3924), **koeria**, head `1ca1abcb3a88a1bc74f2ce0705f627a520a07395`; [issue #3919](https://github.com/lbjlaq/Antigravity-Manager/issues/3919). Open/non-draft; no returned commit-status contexts, check runs or formal reviews.

**Still missing locally:** five `TokenManager` paths at lines 4244, 4321, 4432, 4598 and 4610 still return only `Token pool is empty`. The durable global halt handles newly observed restrictions, but does not make every legacy, manually held, intentionally disabled or unloaded pool diagnostic precise.

**Actual patch problem:** upstream scans arbitrary `accounts/*.json` synchronously whenever the pool is empty, ignores read/parse failures, treats every `quota.is_forbidden` as a ban, and announces all accounts banned/disabled whenever **any** banned record exists—even if its own available counter is positive. The policy change only adds a text alias; it does not establish a typed permanent-attention response or appropriate HTTP status.

**Accept the objective, reimplement narrowly:** diagnose managed/indexed account metadata through existing validated boundaries, without native-secret hydration or arbitrary directory enumeration. Bound/cache the work or move it to the blocking pool. Distinguish no configured accounts, disabled accounts, explicit manual attention, confirmed restrictions, unloaded records, unreadable/locked state and transient provider cooldowns. Preserve global-stop precedence and generation fencing; permanent/manual holds should use shared actionable attention responses, while ordinary transient limits retain genuine retry delays.

**Benefit and cost:** medium adaptation cost; useful operator guidance, especially after restart or when a global halt is disabled/acknowledged. Do not import the submitted scanner or its banned classification. Focused validation should cover mixed enabled/banned records, forbidden-but-unconfirmed records, manual holds, unreadable metadata and global halt. No such tests were run in this review.

### 4. Fix punctuation at the generated join, without a global prose rewrite

**Source:** [PR #3921](https://github.com/lbjlaq/Antigravity-Manager/pull/3921), **koeria**, head `f2143689a5a35c8e449f1a82a5c5d22e266b00a4` (functional commit `7e59e2d55ecee69da8a3d4cb829cfcb260722c54`); [issue #3915](https://github.com/lbjlaq/Antigravity-Manager/issues/3915). Open/non-draft; no returned status contexts, check runs or formal reviews.

**Verified remaining source issue:** `prompt_sanitizer.rs:123–124` defines an identity constant ending in a period; the comma-continuation branches at lines 473 and 481 append a comma directly. The submitted source-boundary change removes that generated final period before joining the comma.

**Accept only:** those two construction fixes and suitable literal-output assertions, preserving any existing opt-out and pass-through behavior. This is low-priority formatting maintenance within an existing transformation.

**Drop:** the new global `\.[,，]` substitution across normalized system prose. Despite its comment, it also changes valid `etc., if needed` text; its test does not actually exercise that counterexample. Do not change user/client text broadly, enable additional identity rewriting, or claim punctuation prevents Google's private enforcement. Existing provider-enforcement assertions in the PR are not an established causal guarantee.

## Exclusions and completed work

| Item | Fresh disposition |
| --- | --- |
| #3923 `c0666dcff9a1395cdc91da126b4d1e3126da097b` | Head unchanged. Previous actual-blob review established destructive one-line placeholder Rust files. Excluded; no unnecessary re-download. A corrected real-source head is required before considering bounded OAuth exchange retries. |
| #3927 `59abba6178322d911fc58690795b8a22e6230306` | Head unchanged. Previous blob review established placeholder catalog/source files. Excluded; advertised model names/capabilities are not integration evidence. |
| #3925 `bfc6927b3432615bcb04eb4c3678aafc738cb599` | Skills pass-through already covered by current code and `preserves_skills_blocks_verbatim_without_injecting_shell_command`. |
| #3926 `b8862658bb1b3c89514b46b49e7cd1a8ccd3ec60` | EOF tail flush already covered more defensively by current lifecycle and `first_content_or_terminal_line_without_newline_survives_eof`. |
| Image fan-out, TLS dependencies, monitor record preservation, Host/Origin guard | Already implemented in the current five-fix worktree. Do not relist as new upstream integrations. Current image admission uses lazy `BoundedJoinSet`; Host protection validates actual local socket authority and explicit trusted hosts. |
| Parent beta sanitizer/default-rewrite bundle | No new head; do not import default-on prompt rewriting or unrelated retry/classification changes wholesale. |
| Platform/headless build branches | No fresh parent delta establishing a needed integration. Keep automatic platform packaging disabled per the user's preference. |

The first two candidates can be made small and independently reviewable; the diagnostic candidate needs adaptation to this fork's safety architecture. The fourth is optional polish. There is **no newly arrived, ready-to-merge parent PR** in this refresh, and no reason to repeat the five completed fixes.
