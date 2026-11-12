# Antigravity Manager investigation results

Date: 2026-11-05. Audience: maintainers of `alexeynesteruk/Antigravity-Manager`.

The most valuable work to integrate is shared media-source protection, correct user-token admission, DNAlec's QoS probe recovery, and upstream beta's quota, Responses media, and account-pool fixes. These address defects still present in the reviewed checkout. Integrate them as separate functional changes, retaining the security, credential, streaming, and persistence safeguards already added by this fork.

The investigation screened the public fork inventory and PR metadata comprehensively, then examined selected code changes against a pinned local commit. It did not audit every line of every fork or enumerate every nondefault branch. Confirmed code paths below are static findings; suggested test cases are acceptance criteria for future implementation, not tests performed during this review.

## Repository baseline and scope

| Item | Reviewed value |
| --- | --- |
| Local repository | [alexeynesteruk/Antigravity-Manager](https://github.com/alexeynesteruk/Antigravity-Manager) |
| Parent and source repository | [lbjlaq/Antigravity-Manager](https://github.com/lbjlaq/Antigravity-Manager) |
| Local branch and commit | `main`, `87fd5464e80e84b2f7e19b64fa4dba0f76afc902` |
| Upstream main | `0067fe045ddd77f4462ea8e2c25384839d869f32` |
| Upstream beta | `ee4c643705e6b9139b965e75139c4e0db2fb4a24` |
| Additional branch inspected | `feat/headless-termux-web`, `90bac68d3b68acbac363aac9c642fdc9f9d2e391` |
| Local versus upstream main | 134 local commits, zero upstream-only commits |
| Local versus upstream beta | 69 local-only and 72 beta-only commits |
| Review activity | Three subagents with separate local-code, upstream-PR, and fork assignments, followed by cross-checks and supplemental reviews |
| Application verification | No builds, application tests, exploit execution, or live Google requests |

Upstream main had no additional commits beyond the imported ancestry. New upstream fixes were on beta. Beta's changes since the common ancestor span 139 files, with approximately 18,751 insertions and 7,376 deletions. Those figures describe the upstream side of the divergence, not the final diff of a proposed merge. A broad merge would require reconciling the fork's 69 divergent commits.

The local manifests report `4.9.9-beta.4` despite incorporating later development. Commit ancestry and actual code determined whether a behavior was present; release labels alone did not.

## Coverage and evidence inventory

| Inventory | Count | Interpretation |
| --- | ---: | --- |
| Fork API pages retrieved | 36 | All pages in the captured listing |
| Unique listed forks | 3,554 | Actual returned records; the repository's displayed fork count was 3,517 |
| Readable default branches | 3,453 | Repository and default-head queries succeeded |
| Listed forks unavailable to this session | 101 | Every missing record had explicit GraphQL `NOT_FOUND` |
| Distinct readable default-head SHAs | 449 | Reduces repeated copies of the same history |
| Fork heads that are local ancestors | 1,599 | Already in the reviewed commit's ancestry |
| Fork heads that are not local ancestors | 1,854 | Represent 289 distinct SHAs; not automatically useful or missing functionality |
| Unfamiliar nonancestor SHAs | 195 | Represent 197 forks; these objects were absent from the local object database |
| Locally known nonancestor SHAs | 94 | Represent 1,657 forks; local object presence did not imply integration |
| Upstream PRs | 550 | 356 merged, 194 closed without merge, zero open |
| Fork PRs | 1,264 | 1,180 merged, 23 closed, 61 open |
| Readable forks with PRs | 80 | The other 3,373 readable forks had no PRs |
| Fork synchronization PRs | 1,080 | Titles identified upstream-main synchronization |
| Total PR records screened | 1,814 | Upstream plus fork PR records; metadata screening, with selected patch review |

The origin repository had no PRs in the captured inventory. Fork PR collections exceeding 100 items were paginated fully. Three representative REST retries for unavailable forks also returned 404. These responses cannot distinguish deletion, privacy changes, or suspension; the report therefore calls them unavailable rather than deleted.

Fork selection considered default-head ancestry, original commit subjects, development activity, and relevance to current code. The fork pass compared nine selected repositories and inspected targeted patches in seven, with additional larry109 and YuanYeYouTao branch and PR checks. Supplemental review examined jul-ienfr, uabidotfun, LAZYGASS, ScholarBuilds, and longgexx, plus selected open PRs. The older locally known nonancestor heads were also screened by dates and subjects.

Machine-readable appendices preserve the captured inventory without API credentials or full API response bodies:

- [Fork inventory](forks.csv): repository, default head, availability, ancestry, and PR count.
- [PR inventory](pull-requests.csv): repository, number, title, state, dates, head SHA, and source URL.
- [Coverage summary](summary.json): counts, pinned commits, artifact hashes, and verification limits.

An inventory row means the repository or PR was enumerated. It does not mean every associated patch was reviewed or accepted. Nondefault branches were inspected selectively.

## Priority and confidence

P1 findings merit correction before relying on the affected security or recovery control. P2 findings affect correctness, availability, or operational behavior. Optional features require an explicit product or deployment need.

“Confirmed” means the relevant implementation and its callers were inspected at the pinned baseline. It does not imply a successful runtime exploit, measured performance improvement, or production reproduction. Fork authors' CI or test statements remain source claims unless independently verified.

## Confirmed local defects

### F01 Untrusted media sources can read server files

**Priority:** P1. **Decision:** implement a shared fix before feature imports.

The OpenAI image adapter accepts `file://` and ordinary filesystem paths and reads them using `std::fs::read`. The read occurs before the resulting content reaches image validation. A request can therefore initiate an unrestricted file read even when the bytes are later rejected as an invalid image.

The related audio path accepts `file://` or an existing local path, reads the complete file, Base64-encodes it, and labels it with a declared or inferred audio MIME type. The size check logs a warning but continues. Shared media handling does not apply image-style signature checks to arbitrary audio bytes. Server file bytes can consequently enter the outgoing payload. Whether those bytes are retained in logs depends on the active logging path and configuration.

Evidence:

- [Image filesystem branch at request.rs line 544](https://github.com/alexeynesteruk/Antigravity-Manager/blob/87fd5464e80e84b2f7e19b64fa4dba0f76afc902/src-tauri/src/proxy/mappers/openai/request.rs#L544).
- [Audio filesystem branch at audio/mod.rs line 90](https://github.com/alexeynesteruk/Antigravity-Manager/blob/87fd5464e80e84b2f7e19b64fa4dba0f76afc902/src-tauri/src/proxy/audio/mod.rs#L90).
- [LAZYGASS PR 3](https://github.com/LAZYGASS/Antigravity-Manager/pull/3) identifies the image-side problem.

Accept the PR's security behavior, broaden it to audio, and leave its `.jules` documentation out. Establish a shared source policy that distinguishes untrusted network input from an explicitly trusted local import operation. Enforce it before filesystem inspection or reads, using a shared ingress or media utility boundary where adapter conversion currently performs I/O. Keep canonical Gemini processing protocol-independent. A fix applied only after Gemini conversion is too late to prevent the read.

Compatibility cost: clients currently submitting server-local paths through the network API would need to upload content or use an explicitly authorized local import mechanism.

Acceptance criteria: reject Unix, Windows, UNC, relative, and `file://` paths from untrusted image and audio input before I/O; retain supported data and remote URL behavior; enforce limits before allocating full local file contents. Remote-URL policy should be assessed separately rather than assumed safe by this finding.

### F02 Disabled user API tokens remain valid

**Priority:** P1. **Decision:** fix central validation.

`get_token_by_value` retrieves disabled rows as well as enabled rows. `validate_token` checks expiration, IP restrictions, and curfew, but never checks `enabled`. The authentication middleware accepts a successful result and proceeds with the request. Disabling a user token therefore does not revoke its access on routes using this validation path.

Evidence: [user_token_db.rs line 595](https://github.com/alexeynesteruk/Antigravity-Manager/blob/87fd5464e80e84b2f7e19b64fa4dba0f76afc902/src-tauri/src/modules/user_token_db.rs#L595), called from [auth.rs line 254](https://github.com/alexeynesteruk/Antigravity-Manager/blob/87fd5464e80e84b2f7e19b64fa4dba0f76afc902/src-tauri/src/proxy/middleware/auth.rs#L254).

Reject disabled tokens at the shared admission boundary. Keep unrestricted lookup available to administration screens. Preserve the existing distinctions between the primary proxy key, administrator credentials, user tokens, and authenticated internal requests.

Acceptance criteria: a disabled user token is rejected on subsequent admission; enabled tokens retain expiry, curfew, and IP behavior; administrator and primary-key authentication remain unchanged. Define any stronger in-flight revocation requirement separately.

### F03 IP admission does not reserve capacity

**Priority:** P2. **Decision:** combine with central token admission while keeping usage accounting separate.

Validation queries the existing binding count without claiming a slot. New bindings are inserted later by response accounting. With `max_ips=1` and no bindings, requests from two IPs can both observe a count of zero and proceed. After both complete, both IPs are bound; later requests from either IP bypass the count because they are already bound.

Evidence: [admission check at user_token_db.rs line 612](https://github.com/alexeynesteruk/Antigravity-Manager/blob/87fd5464e80e84b2f7e19b64fa4dba0f76afc902/src-tauri/src/modules/user_token_db.rs#L612), binding insertion at line 557, and response accounting in `proxy/middleware/monitor.rs` around lines 1757 and 1868.

Perform enabled, expiry, applicable restrictions, binding lookup, capacity check, and reservation through a serialized admission transaction. An SQLite immediate transaction is a suitable candidate. Return the admitted identity from that operation rather than validating and fetching it independently. Keep request counts and token usage in response accounting.

Acceptance criteria: simultaneous new-IP admissions never exceed capacity; existing bound IPs work; same-IP concurrency does not create duplicate bindings; database errors do not silently grant access. Define the lifetime of reservations for failed or cancelled requests.

### F04 Transport activity can bypass stream inactivity limits

**Priority:** P2. **Decision:** fix the shared lifecycle before expanding its use.

The lifecycle ignores empty transport chunks, but updates `last_activity` for other received bytes regardless of meaningful handler progress. SSE comments, blank lines, and incomplete frames can renew the clock. A stalled generation can therefore outlive the configured stream inactivity allowance. The incomplete-line buffer also lacks an explicit size bound.

Evidence: [stream_lifecycle.rs line 172](https://github.com/alexeynesteruk/Antigravity-Manager/blob/87fd5464e80e84b2f7e19b64fa4dba0f76afc902/src-tauri/src/proxy/pipeline/stream_lifecycle.rs#L172), followed by unconditional activity refreshes through line 240. Latest reviewed upstream beta retains this problem and does not preserve the local empty-chunk guard when the whole file is replaced.

Track transport liveness and meaningful protocol progress separately. Have adapters report complete-event progress while the shared driver owns phase transitions and deadlines. Bound unfinished frames and persistence acknowledgements. Do not infer progress solely from whether a handler has ever emitted content.

Acceptance criteria: ignored keepalives do not renew generation progress deadlines; valid thinking and tool deltas count appropriately; coalesced and fragmented SSE events are handled correctly; oversized unfinished frames fail predictably; trailing content is processed before EOF validation.

## Recommended integrations

### I01 DNAlec QoS probe ownership and recovery

**Priority:** high. **Sources:** [DNAlec PR 3](https://github.com/DNAlec/Antigravity-Manager/pull/3), [commit ac9eca74](https://github.com/DNAlec/Antigravity-Manager/commit/ac9eca74026b9aa221de8181478a42a29cffc458).

The current guardian's `mark_probe_passed` and `mark_probe_failed` unconditionally replace state. A delayed result can overwrite a manual reset, a renewed jail, or removal. Local commit `b4fd59c8` improves candidate scanning and rechecks renewed jails, but does not establish ownership of asynchronous completion.

The current probing tick also claims an expired jail before confirming a token exists, which can leave a `Probing` state without a task. Probe requests use a separate client rather than the account's configured proxy, and the first arbitrary response bytes can be treated as evidence of recovery.

Evidence: [qos_guardian.rs line 677](https://github.com/alexeynesteruk/Antigravity-Manager/blob/87fd5464e80e84b2f7e19b64fa4dba0f76afc902/src-tauri/src/proxy/qos_guardian.rs#L677) and `token_manager.rs` around lines 5345, 5497, and 5544.

| Files | Integration decision |
| --- | --- |
| `proxy/qos_guardian.rs` | Accept unique probe claims, atomic claim transitions, and completion checks against the current claim. |
| `proxy/token_manager.rs` | Accept credential/model eligibility before claiming, configured-client reuse, and bounded SSE validation requiring actual non-thinking text. Preserve current model/envelope and endpoint fallback behavior. |
| `proxy/server.rs` | Adapt the required shared-client registration and lifecycle wiring. |
| Focused QoS regressions | Accept ownership, deletion/reset, missing-token, proxy-routing, and deadline cases. Rebase onto current test helpers. |
| Dockerfiles and validation documentation | Keep the healthcheck change separate from the functional backport. Retain attribution and distinguish contributor-reported checks from local verification. |

Compatibility cost: recovery becomes stricter; accounts without a suitable authorized text model or usable credentials may need metadata refresh or manual recovery. Preserve the HalfOpen stage: a successful Gemini probe does not establish that every other model is healthy. The fork's hardcoded Daily probe target should not replace local fallback.

Acceptance criteria: stale completions cannot resurrect deleted accounts or overwrite newer state; only one task owns a claim; missing prerequisites leave a recoverable state; probes use the account proxy; empty, thinking-only, error, and keepalive-only streams fail within the bounded probe deadline.

### I02 Quota exhaustion and scheduler eligibility

**Priority:** high. **Sources:** [upstream PR 3845](https://github.com/lbjlaq/Antigravity-Manager/pull/3845), [PR 3840](https://github.com/lbjlaq/Antigravity-Manager/pull/3840), and commits `41714953`, `8952121a`, and `ad125244`.

Local quota handlers around `token_manager.rs` lines 4709 and 4859 return before recording hard depletion when the optional circuit breaker is disabled. The settings default leaves that breaker off. Separately, scheduler priority narrowing occurs before quota eligibility, allowing an exhausted high-priority account to displace healthy lower-priority accounts. Retry paths also lack a shared per-request exclusion set for accounts that already failed.

Port three related invariants: authoritative depletion applies independently of optional adaptive penalties; model and quota eligibility precede preference and priority narrowing; retries remember failed accounts within the current request. Keep transient capacity, rate limits, hard quota depletion, and internal pool exhaustion distinct.

Accept shared classification and lock handling in `rate_limit.rs`, eligibility in `token_manager.rs`, and request-level retry state in the shared pipeline. Adapters should report protocol outcomes rather than own independent scheduler policies. Include the relevant UI/configuration interpretation so “breaker off” does not imply “ignore physical quota.”

This is not a standalone cherry-pick of `ad125244`: its shared depletion classifier comes from `41714953`. The upstream helper named `is_quota_exhausted_or_explicit_limited` checks quota-exhaustion reason locks; explicit Retry-After recorded under a rate-limit reason needs a deliberate contract when the breaker is off. Preserve compatibility for unknown quota separately from confirmed zero quota.

Acceptance criteria: depleted accounts remain excluded with the breaker off; healthy lower-priority accounts remain selectable; TPM transients do not become hard depletion; reset timestamps and weekly/short-window precedence are respected; retry exclusions are scoped to one request; unknown-quota accounts retain the intended compatibility policy.

### I03 Responses media logging and estimation

**Priority:** high. **Source:** [upstream PR 3830](https://github.com/lbjlaq/Antigravity-Manager/pull/3830), commit `7a056857`.

Responses input simplification copies `output` and related fields back into the concise log representation, while string-form image URLs are not consistently reduced. Raw Base64 can survive concise logging. The estimator falls back to serializing a Responses body as text, inflating token estimates with encoded media. Aggregate ingress image validation also runs before configured history pruning can reduce old media.

| Files | Integration decision |
| --- | --- |
| `proxy/payload_audit.rs` | Accept bounded simplification for Responses items, tool output, and string/object image URL forms. Preserve useful call identifiers and debugging structure. |
| `proxy/pipeline/estimator.rs` | Accept explicit Responses-aware estimation that distinguishes media from text. Keep generic estimation in the pipeline. |
| `proxy/handlers/openai.rs` | Adapt aggregate media validation to the shared pruning lifecycle. Preserve data-format and single-image checks. |

The local sliding-window facility exists but defaults to disabled. Pruning cannot be assumed active for every deployment. This integration must preserve limits when pruning is disabled and must not silently discard required current-turn media.

Acceptance criteria: concise logs stay bounded for large image/tool outputs; equivalent media does not acquire text-token costs from Base64 length; pruning and aggregate limits follow an explicit order; invalid and individually oversized media remains rejected.

### I04 Index membership and orphan credential files

**Priority:** high. **Source:** [final upstream commit f2f8a0d8](https://github.com/lbjlaq/Antigravity-Manager/commit/f2f8a0d851b6504ca501eae64a731380ea944d36), associated with issues/PRs 3806, 3807, and [3808](https://github.com/lbjlaq/Antigravity-Manager/pull/3808).

Current pool loading scans account JSON files and checks disabled state, but does not require membership in the account index. Credentials left behind after a deletion failure can re-enter scheduling. Single-account deletion cannot clean an orphan after the index entry is gone. Batch deletion removes files before persisting its final index and ignores file-removal errors.

Accept the final upstream membership and reload behavior in `proxy/token_manager.rs`, orphan-aware cleanup in `modules/account.rs`, and necessary administration reload wiring. Preserve current managed-path validation and import/migration writer boundaries. Loading should skip orphan files without deleting them; a reader must not destroy credentials during an in-progress import.

The final source is preferable to treating PR 3808's closed-unmerged status as proof that its idea was rejected. Its behavior was incorporated through another commit. The source still needs adaptation for safe batch deletion in this fork.

Acceptance criteria: valid indexed accounts load; orphan files stay on disk but never enter the pool; reload removes nonmembers from memory; index-write failure occurs before destructive deletion; partial cleanup failures remain visible and retryable; legacy filename/internal-ID compatibility remains deliberate.

### I05 Primary endpoint preference and sandbox failures

**Priority:** medium. **Source:** [upstream PR 3833](https://github.com/lbjlaq/Antigravity-Manager/pull/3833), commit `8fdab26f`.

Current endpoint ordering places healthy endpoints ahead of recently failed ones without separating primary and sandbox roles. A transient primary failure can promote a restricted sandbox endpoint. A failure at the last sandbox endpoint can also acquire a terminal classification that blocks useful account rotation.

Accept host-aware primary/sandbox ordering and narrowly classified sandbox failures in `proxy/upstream/client.rs`. Adapt terminal behavior to the shared budget and rotation policy. Use parsed host boundaries rather than broad substring matching for a generalized implementation.

The upstream 300-second sandbox cooldown changes ordering; it does not guarantee that sandbox is skipped whenever the primary fails. Preserve global request-budget exhaustion as terminal and retain a fallback when all endpoints are unhealthy.

Acceptance criteria: a transient primary failure does not promote sandbox above it; regional sandbox failures do not poison unrelated accounts; global budget and large-body safeguards remain enforced; all-unhealthy operation has a defined bounded fallback.

### I06 Shared Codex and legacy streaming lifecycle

**Priority:** medium, after F04. **Sources:** [upstream PR 3817](https://github.com/lbjlaq/Antigravity-Manager/pull/3817), commits `bab28086` and final shared conversion `ab423b86`.

Local Codex and legacy streams use separate fixed-idle loops where other paths already use the shared lifecycle. The upstream follow-up reduces behavioral drift across first output, thinking, transition, and steady streaming.

Accept the shared-driver conversion in `proxy/mappers/openai/streaming.rs` and required Gemini state reporting. Keep protocol event encoding in adapters. Preserve local commit `2bb59afb`, which processes trailing content before EOF validation, and the explicit empty-chunk guard.

Latest upstream does not solve F04. It also retains an unbounded persistence acknowledgement through `finalize_barrier`; the old local Codex path already has an analogous unbounded wait. Bound it during adaptation rather than presenting the migration as a complete lifecycle fix.

Acceptance criteria: all four protocols use consistent phase/deadline semantics; thinking-to-tool transitions do not become empty-success responses; partial output terminates according to the existing contract; ignored heartbeats do not count as content; cancellation and persistence waits finish within configured bounds.

## Useful optional integrations

| Candidate | Value and required adaptation |
| --- | --- |
| [DNAlec PR 4](https://github.com/DNAlec/Antigravity-Manager/pull/4), commit `7a0babd6abdc496aea754c027faf7657716c854c` | Validated per-instance `proxy.qos`, partial-field defaults, API/disk/desktop save parity, hot updates, and runtime readback. Adopt after I01. Accept `qos_config.rs`, configuration/type fields, guardian policy reads, and required save wiring. Preserve current deadlines and probe policy snapshots; unchanged defaults are safer than an unexplained threshold relaxation. |
| [jul-ienfr weighted selection](https://github.com/jul-ienfr/Antigravity-Manager/commit/8392da55d39955d88839687771f679a1d1929ae6) | Current `proxy_pool.rs:367` implements `WeightedRoundRobin` by calling strict priority selection. Adapt only the selection policy. The fork supplies weighted random behavior, not round-robin, and its signed priority arithmetic can overflow. Define supported weights and deterministic scheduling semantics before implementation. |
| [uabidotfun trusted CA support](https://github.com/uabidotfun/Antigravity-Manager/commit/c570c8905e9626af81b40cc84bd704aa6ab47130) | Useful for deployments using installed corporate CAs. Introduce an explicit trust policy across shared, gateway, proxy-health, and relevant reqwest clients. The fork only changes `utils/http.rs` because it removed the gateway. Installed rquest source prefers WebPKI when both root features are enabled; a feature toggle alone is insufficient. Preserve certificate verification. No certificate failure was reproduced during this review. |
| [LAZYGASS stored-log redaction](https://github.com/LAZYGASS/Antigravity-Manager/commit/e6c760b56cce14754f32701728207100ecc1762d) | Centralize precise credential-field redaction at shared audit persistence. The fork's broad key matcher can redact token metrics and misses a successful consolidated-SSE path. Accept the objective, replace the matcher, and preserve tool schemas and request semantics. |
| [jul-ienfr routing read reduction](https://github.com/jul-ienfr/Antigravity-Manager/commit/5ee3b38d200dfd1da121998ec2a8556b9849327f) | `token_manager.rs` reads account JSON synchronously during dynamic model resolution. Consider existing `ProxyToken.available_models` with explicit reload invalidation. Drop the fork's substitution using quota-map keys because physical/tier identities can differ. Measure before claiming performance improvement. |
| [atengk data-frame heartbeats](https://github.com/atengk/Antigravity-Manager/commit/472db971b23a3f64f6cd151f960bccb3229020a4) | Potentially useful where intermediaries discard SSE comments. Accept only with a deployment need and complete-event parsing. Its chunk-prefix heartbeat detector can drop real events coalesced with comments. Adapt mapper representation and shared preflight handling together. |
| [Classic credential synchronization PR 3834](https://github.com/lbjlaq/Antigravity-Manager/pull/3834) | Consider missing standalone/classic credential synchronization in `modules/integration.rs`. Retain `CredentialFiles` target isolation, private atomic writes, and propagated failures. Do not replace them with the older boolean-based cross-client write flow or warning-only failures. |

### Standalone headless server and Termux

The [headless branch](https://github.com/lbjlaq/Antigravity-Manager/tree/90bac68d3b68acbac363aac9c642fdc9f9d2e391) provides useful runtime extraction, a dedicated server entry point, and a static-Web toggle. It is a separate feature track requiring rework.

| Files | Accept or rework | Exclude or correct |
| --- | --- | --- |
| `src-tauri/src/lib.rs` | Extract reusable headless initialization. | Preserve fail-fast configuration handling; the branch uses `unwrap_or_default()` on load errors. |
| `src-tauri/src/bin/server.rs` | Add the dedicated entry point. | `--host` currently toggles LAN access instead of binding the requested address. Validate missing/unknown arguments and make dedicated help reachable. |
| `src-tauri/src/proxy/server.rs` | Add `--no-web` / `ABV_DISABLE_WEB` for static hosting. | Explain that management APIs remain enabled; this is not an API-isolation mode. Preserve current security layers. |
| `src-tauri/src/proxy/middleware/auth.rs` | Keep the current file. | Drop the suffix whitelist from `47e909c7`. Treating any `.js`, `.css`, or `.svg` path as a health check is too broad and unnecessary for the current static fallback. |
| `src-tauri/Cargo.toml` and module gates | Separate desktop/server features if reduced Linux dependencies are required. | A second binary alone does not remove unconditional Tauri, tray, desktop-plugin, and GTK dependencies. |
| `src-tauri/src/utils/crypto.rs` | Make Android device identity durably reusable. | Ignored identity-write failures and concurrent first creation can produce changing encryption keys. Use atomic creation and explicit errors. |
| `scripts/install-termux.sh` and workflows | Adopt only with a complete supported packaging track. | The branch has CI artifacts without matching Android release publishing. The installer advertises Web behavior without shipping matching frontend assets and automatically falls back to preview after a warning, without an explicit channel choice. |
| Release manifests and announcements | Generate for this fork when releasing. | Do not carry unrelated version bumps, Cask changes, or release claims into implementation commits. |

Teee3047 PRs 1 through 3 were identified as Termux-specific candidates during metadata screening. They were not established as ready integrations in the current desktop/server architecture.

## Changes to defer or exclude

These decisions apply to the reviewed patches, not to the contributors or the broader goals of their work.

| Source | File-level decision | Reason and actionable alternative |
| --- | --- | --- |
| [Upstream PR 3805 OAuth](https://github.com/lbjlaq/Antigravity-Manager/pull/3805) | Preserve local `modules/oauth_server.rs`. Consider `AddAccountDialog.tsx` fallback-state handling independently. | The patch assumes token exchange occurs inside `submit_oauth_code`; this fork uses a one-time claim/channel flow with nonce and redirect validation. Port only a demonstrated remaining UI gap. |
| [taiab0x1 PR 1](https://github.com/taiab0x1/Antigravity-Manager/pull/1) | Drop replacement `proxy/middleware/auth.rs` and its helper-only tests. | Existing identity propagation is already present. The replacement preserves unconditional internal bypass and raw forwarded-IP trust, loses current credential handling, and fixes neither F02 nor F03. Its missing-secret test contradicts the helper's empty-string equality. Implement admission fixes in current `user_token_db.rs`. |
| [Upstream PR 3822](https://github.com/lbjlaq/Antigravity-Manager/pull/3822) | Exclude fixed 135/160-second caps in `upstream/client.rs` and associated constants. | Later commit `41714953` removes those caps after valid slow requests regressed. Preserve configurable budgets and useful terminal/sticky-release behavior from the final state. |
| [Upstream PR 3840 pedal feature](https://github.com/lbjlaq/Antigravity-Manager/pull/3840) | Defer `usePedalModeStore.ts`, Layout polling, banner/modal, types, and related UI. Accept the scheduler eligibility invariant separately under I02. | Recovery depends on browser localStorage and GUI polling. Move lifecycle ownership to the backend before presenting it as headless-compatible account control. |
| [jul-ienfr performance bundle](https://github.com/jul-ienfr/Antigravity-Manager/commit/5ee3b38d200dfd1da121998ec2a8556b9849327f) | Drop stale disk-state and quota-setting caches in `token_manager.rs`; drop fixed timeout/pool tuning in `upstream/client.rs`; reassess `proxy_pool.rs` health flags independently. | Quota-cache invalidation has no callers; a five-second disk cache delays disable/delete visibility; old timeout tuning conflicts with staged budgets; health flags do not cover manual checks and can remain set after cancellation. Use explicit snapshots/invalidation and measured changes. |
| [longgexx cache speculation](https://github.com/longgexx/Antigravity-Manager/commit/f81d3083199ebe3ce1ae736aafb8ad0ec2724b5e) | Drop speculative cache accounting, its tokenizer dependency, and related response/stream usage rewriting. | It replaces measured usage with estimates, keys cache state by model/session without tenant/account isolation, and substitutes an image marker for image content. Keep upstream usage authoritative; label any future estimate separately. |
| [atengk HTTP2 PING](https://github.com/atengk/Antigravity-Manager/commit/533efa9a5bd78f01bcee682ddf6882812b6b3f78) | Exclude global PING changes in `upstream/client.rs`. | Current builder comments document prior GFE/PING-ACK failures. Require deployment-specific evidence and an opt-in before revisiting this transport choice. |
| [Huo cloudflared cleanup](https://github.com/Huo-zai-feng-lang-li/Antigravity-Manager/commit/8ae1ad9588b3a95a54417c1a13b6af9f41273bba) | Drop broad process scanning, hardcoded port matching, and unescaped Unix-regex cleanup in cloudflared management. | Current code already owns its child and uses bounded shutdown. If orphan cleanup is needed, identify only the managed executable and recorded process identity. |
| [cc1027114472 warmup](https://github.com/cc1027114472/Antigravity-Manager/commit/e98da2e292cf) | Drop the alternate warmup/scheduling bundle, blanket 403 account removal, and fixed model choice. | Current warmup already consumes the body and distinguishes transient 403s. Isolate a reproducible missing behavior before replacing this architecture. |
| [LAZYGASS PR 7](https://github.com/LAZYGASS/Antigravity-Manager/pull/7) | Accept nonblocking persistence as a goal; rewrite `scheduler.rs` and relevant `quota.rs` callers. | Moving writes outside the mutex lets concurrent snapshots persist out of order. Use an ordered worker or serialized atomic writer so asynchronous I/O cannot lose the newest state. |
| fengyi-doro PRs 2 and 3 | Defer tenant/portal/model-routing changes as a separate product track. | Their metadata describes broader account and portal architecture. They were not validated as fixes for this fork's current token admission defects. |
| [Upstream PR 3056](https://github.com/lbjlaq/Antigravity-Manager/pull/3056) | Defer adapter-level credit/quota-bypass changes. | Any supported policy belongs in shared, explicit configuration rather than a local adapter exception. |
| [Upstream PR 3020](https://github.com/lbjlaq/Antigravity-Manager/pull/3020) | Defer the 116-file experiment as an integration unit. | Extract a cohesive, independently reviewable problem and demonstrate current relevance before adopting individual changes. |

## Existing integrations and superseded proposals

| Source | Current status |
| --- | --- |
| Upstream 3765 and 3774 | Signature SQLite/cache work already incorporated, with local retry/invalidation corrections. |
| Upstream 3775 | Timeout, replay, and streaming protections incorporated, with local EOF correction. |
| Upstream 3766 | UI scaling incorporated. |
| Upstream 3771 and 3780 | IDE 2.6 credential/process changes incorporated; local code additionally propagates credential fallback failures. |
| Upstream 3777 | Windows test-manifest fixes incorporated. |
| Upstream 3726 | Import and directory-migration behavior incorporated with state-preserving corrections. |
| larry109 PR 1 | Security, OAuth, and UI-save work selectively incorporated and strengthened. |
| [larry109 PR 2](https://github.com/larry109/Antigravity-Manager/pull/2) | Single-account grace retries already use a counted fixed-delay path. |
| YuanYeYouTao PR 4 | Responses history, tool recovery, and usage accounting incorporated with corrections. |
| YuanYeYouTao PRs 6 and 7 | Safe-dispatch/raw-usage diagnostics and metadata-only logging hooks already present. |
| ScholarBuilds tool-result images | Deferred media handling already exists in Claude conversion and shared processing. Drop the older adapter-local repair. |
| [Quorafind PR 1](https://github.com/Quorafind/Antigravity-Manager/pull/1) | Image generation and editing already pass the actual selected model to image-account selection. |
| Quirrel internal authentication | Current loopback/internal credential hardening already covers the relevant restriction. |
| SanHsien ciphertext guard | Existing `proxy_pool.rs` checks cover the inspected guard. Broad dependency upgrades were not established as an integration need. |
| Upstream 3632, 3543, and 3554 | Earlier selective integrations already cover the reviewed proposals. |
| Upstream 3019 | The reported duplicate usage-recording call is already absent. |
| Upstream 954 | Multi-wildcard/specificity handling exists; deterministic ordering for equal-specificity rules remains a possible focused follow-up. |

A separate historical follow-up is [PR 3179](https://github.com/lbjlaq/Antigravity-Manager/pull/3179): commit `64a4b165` appears in local history, but reviewed export writes at `commands/proxy.rs` around lines 542 and 556 do not retain the earlier shared path validation. Reassess those export APIs against the current administration boundary before restoring a narrow validation helper; exclude unrelated locale and warning cleanup.

## Integration sequence and review boundaries

| Stage | Functional unit | Dependency and rollback boundary |
| --- | --- | --- |
| 1 | F01 shared untrusted-media source policy | Preserve supported uploads. Separate any trusted local import API from network conversion. Reverting restores the filesystem-read exposure. |
| 1 | F02 and F03 central user-token admission | One coherent admission change with atomic IP reservation and separate usage accounting. Preserve existing auth/IP layers. Reverting restores ineffective disable/IP controls. |
| 2 | I01 QoS probe ownership and validation | Keep Docker healthcheck changes separate. Retain current fallback and sample-expiry behavior. Revert as one state-machine unit if needed, acknowledging the stale-completion risk. |
| 2 | I03 Responses media governance | Rebase on current canonical pipeline and pruning configuration. Preserve image decoding/format protections. |
| 2 | I04 account membership and deletion | Preserve managed paths, writer locks, and import semantics. Keep orphan files available for recovery. |
| 3 | I02 quota classification, eligibility, and retry memory | Port shared primitives before dependent handler wiring. Validate policy semantics before adding pedal UI or relaxing protection. |
| 3 | I05 endpoint policy | Integrate against the final budget/rotation contract. Keep global-budget exhaustion terminal. |
| 3 | F04 and I06 stream progress/lifecycle | Correct the shared progress clock first or in the same functional unit; preserve local EOF behavior and bound persistence completion. |
| 4 | Optional QoS configuration, weighted selection, CA policy, or standalone server | Separate product decisions and PRs. Require headless parity and cross-platform behavior for each. |

Stages identify dependencies, not a requirement to group unrelated fixes into one PR. Each change should remain individually revertible. Preserve contributor authorship and link the actual source PR/issue in each implementation commit. No implementation commits, PRs, releases, or deployments were created by this investigation.

Repository guidelines require maintainer confirmation of beta staging for nontrivial implementation. The report and inventories are documentation only; they do not authorize or perform those code changes. When implementation is requested, create the branch from the chosen remote base, reconcile the fork's divergent safeguards, and complete the repository PR template with affected behavior, unverified paths, and rollback implications.

## Validation required during implementation

The following checks are proposed to settle the remaining engineering questions. They were not executed during the investigation.

| Area | Focused evidence required |
| --- | --- |
| Media source policy | Filesystem access is rejected before I/O for image/audio variants; supported data and remote media still normalize; limits do not require unbounded reads. |
| Token admission | Disabled-token denial, serialized competing-IP admission, same-IP uniqueness, existing-binding compatibility, and failure handling. |
| QoS state machine | Probe claim uniqueness, stale completion after reset/removal/re-jail, missing prerequisites, account proxy selection, actual text success, and bounded keepalive-only streams. |
| Quota scheduling | Breaker-off hard depletion, authoritative resets, weekly/short-window precedence, unknown quota, exhausted high-priority versus healthy lower-priority selection, and per-request exclusions. |
| Responses | Bounded concise logs, media-aware estimation, disabled/enabled pruning, current-turn preservation, and strict single-item validation. |
| Account lifecycle | Orphan exclusion without deletion during reads; failed index writes precede deletion; partial failures remain recoverable; managed paths remain enforced. |
| Endpoint fallback | Primary preference after transient failure, sandbox regional failures, global deadline exhaustion, large-body replay bounds, and all-unhealthy fallback. |
| Streaming | Fragmented/coalesced events, comments and empty chunks, thinking/tool transitions, trailing EOF data, bounded unfinished frames, cancellation, and bounded persistence waits. |
| Optional configuration | Partial defaults, invalid-value rejection, disk/API/desktop parity, hot reload, and preservation of in-flight policy snapshots. |
| Optional platform work | Windows/macOS/Linux behavior; Android only if adopted; actual interface binding; durable device identity; matching release artifacts and frontend assets. |

Use the project's targeted-test policy and the user's implementation-time testing preference. Do not run release pre-flight commands merely to create this report. A source PR's green CI does not establish compatibility with this fork's rebased combination; actual verification belongs on the final implementation.

## Snapshot provenance and maintenance

The appendices contain public GitHub metadata captured during the investigation on 2026-11-05. They are a historical snapshot, not a live integration queue. Repository access, branch tips, PR states, and displayed counters can change after capture.

`summary.json` records SHA-256 hashes of the report and CSV appendices so later edits can be distinguished from the captured evidence. CSV strings beginning with spreadsheet formula prefixes are escaped with a leading apostrophe; these exports are for inspection and do not contain executable commands or API credentials. Source links and immutable commit anchors identify the original evidence.

To refresh the investigation, repeat the fork and PR enumeration, check ancestry against the new local baseline, and re-evaluate selected patches by behavior. A locally available Git object, a merged PR, a recent push, or a large ahead count is not sufficient evidence that a change is missing or safe to integrate.
