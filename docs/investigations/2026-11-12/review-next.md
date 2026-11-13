# Review after the five priority fixes

Reviewed **2026-11-12**, against local `main` at `abbb4da87c751f2797a5940317f6cb889837e0b4` plus the current uncommitted implementation. Three parallel specialists reviewed local persistence/security, parent PRs, and selected fork patches; the coordinator refreshed all readable fork/PR metadata and independently reviewed long-lived WebSocket authorization and release recovery.

The previous five fixes are present and are not outstanding findings: image fan-out is bounded, TLS/root dependencies were updated, monitor caps preserve transport, Host authorities are checked, and the one-time migration marker is durable. The new priorities are authorization across WebSocket turns, ownership of a shared profile, and safe publication of configuration/credential changes.

## Current findings, ranked

### 1. P1: open WebSockets outlive device revocation and master-key rotation

`handlers/openai.rs:7822-7844` authenticates the HTTP upgrade once and captures its headers and `UserTokenIdentity`. The socket loop at 7891-7923 creates a fresh request budget and calls `SafetyController::admit` for each later logical turn, but does not rerun authentication, device policy or the service-status gate. `handle_websocket_turn` then calls `handle_chat_completions` directly at 8005, bypassing router middleware.

A previously valid socket can therefore submit new `response.create` turns after its device key is disabled, deleted, expires or enters curfew, or after the gateway master key changes. Those policies are checked only by `user_token_db::admit_token` and the HTTP auth layer at upgrade time. Logical service stop likewise sets `is_running=false` and stops background tasks without clearing the retained token manager or reauthorizing later socket turns. `common::enforce_model_gate` checks model/catalog/quota eligibility, not current client authorization or service status.

The global emergency stop, per-device request budget and global outbound budget still apply; this finding does not bypass those controls. It concerns **new logical turns after revocation**, not recalling a request already sent upstream. LAN HTTPS protects the connection but does not refresh its cached authorization. The code path was independently checked by the local reviewer; no live socket or provider request was used to exploit it.

**Fix:** use shared current authorization/service policy before every new WebSocket turn, with the actual normalized peer address and authenticated credential identity/revision. Recheck device enablement, expiry/curfew and key rotation, then admit the turn. Fail closed when that policy cannot be read, without performing a model attempt. Keep this shared with HTTP policy rather than duplicating an adapter-specific authorization implementation. Define cancellation of an already running turn separately.

**Regression needed:** establish a socket with synthetic credentials and a fake upstream, revoke/expire/rotate or pause the service, then assert a subsequent turn is rejected with zero upstream attempts. Existing handshake and budget tests do not establish these lifecycle guarantees.

### 2. Conditional P1/P2: CLI and headless writers can race a live profile

The account file/index mutexes and OAuth refresh single-flight map are process-local (`modules/account.rs:19,30`; `modules/oauth.rs:17,801`). CLI handling precedes GUI single-instance registration (`lib.rs:541,607`), and the standalone server bypasses that GUI registration. No canonical-data-directory interprocess writer lock was found.

Two headless instances on different ports, or a mutating CLI command alongside the app, can both read account revision R0 and pass their local comparisons before either atomic publication. They can then publish competing generations or refresh the same rotating OAuth credential concurrently. Atomic rename protects JSON integrity; it does not make read/compare/replace atomic across processes. This is a conditional concurrency risk, not an observed account failure or a claim that the current profile has multiple active writers.

The same unsupported ownership model creates a stop-state inconsistency: A's successful halt sets a local memory latch; B's explicit acknowledgement updates shared SQLite but clears only B's latch. A returns early on its own latch before observing that durable acknowledgement. Preserve failed-persistence latches when repairing this; blindly clearing a local latch from a database `halted=false` would weaken the existing failure protection.

**Fix:** enforce one OS-backed owner per canonical profile across GUI, headless and offline/mutating CLI operations, forwarding supported commands to that owner or rejecting conflicting offline work clearly. If concurrent owners are intentional, serialize refresh/read/compare/publish/retirement interprocess and reconcile persisted versus unpersisted stop generations. Test with two isolated helper processes and synthetic credentials, not a real OAuth account.

### 3. P2: a rejected settings save can already change runtime safety policy

`proxy/server.rs:2120` publishes `new_config.proxy.account_safety` into the shared live controller **before** `save_app_config` validates and persists the submitted configuration at 2122. An invalid image limit or trusted host, a disk failure, or native-store failure can therefore make the HTTP save return an error after its new limits/stop-observation flags took effect.

The cached upstream controller uses that live policy. A UI reload reads the older durable configuration, so the controls can display the previous settings while effective limits differ. A later independent reconfiguration may restore the saved policy; the failing operation does not roll itself back. Existing individual account holds are not cleared by this side effect.

**Fix:** validate and durably save first, then publish safety policy alongside the other hot updates. Rejection must preserve both durable and live policy. Cover invalid-field and persistence-failure fixtures through the authenticated route.

### 4. P2: a stale authorized settings client can restore a rotated gateway key

Configuration reads return a whole hydrated snapshot. Saves serialize that snapshot under `CONFIG_IO_LOCK`, but carry no observed config/secret revision or explicit secret-change intent (`modules/config.rs:333`; `config_secrets.rs:153,195-218`). The latest native reference is used for payload reuse comparison, not stale-write rejection.

Two authorized clients can read key K0; A rotates it to K1; B saves an unrelated setting from its older snapshot. With a separate administrator password, B remains authorized and its K0 becomes the newly stored/live key. Already-authenticated concurrent requests provide another possible ordering. A latest-store merge within one frontend instance does not cover independent tabs/clients. This is an accidental stale-write/security concurrency risk requiring an authorized settings writer, **not an unauthenticated key-reset vulnerability**.

**Fix:** reject stale full replacements using observed config/secret revisions, or move secret changes into explicit operations while ordinary metadata updates preserve current secrets. Test K0-to-K1 rotation followed by an old snapshot's unrelated edit. Protected storage must retain current intent as well as protecting the bytes.

### 5. P2: deletion can report success while leaving native OAuth credentials behind

`modules/account.rs:3826-3833` converts an error reading the protected account's native reference to `None`, then unlinks the JSON at 3835 and reports success. Native deletion is skipped. An unreadable file whose directory still allows deletion, including an appropriate Windows sharing handle, can take this path.

The active metadata manifest is lost while the native entry remains. A rollback backup may retain the reference, but automatic cleanup cannot use a reference that was not recovered; no partial-cleanup result is returned. This is separate from the already-fixed index/unlink ordering and from the existing path that reports actual native-delete failures.

**Fix:** retire the index/pool as already designed, but preserve the file and return an explicit partial-cleanup error if reference recovery fails. Retry cleanup after repair; an explicit force option must disclose an unknown native orphan. Use a synthetic protected account and injected read failure or Windows handle-sharing fixture.

Exact file anchors, interleavings and test proposals for findings 2-5 are in [the local review](review-next-local.md).

## Remaining integrations worth adapting

No new parent/default-fork/PR heads arrived between the preceding inventory and this pass. The following are remaining or newly inspected objectives, not newly arrived ready-to-merge bundles.

| Candidate | Why useful | Accept/drop boundary |
| --- | --- | --- |
| SanHsien [PR 15](https://github.com/SanHsien/Antigravity-Manager/pull/15) | Keep Web admin credentials in memory; publish only verified logins; fence stale 401 responses; gate initialization. | Adapt the complete lifecycle and preserve strict backend auth/Tauri behavior. Reload/new-tab login changes are deliberate; memory does not defeat active same-origin script compromise. |
| SanHsien [PR 17](https://github.com/SanHsien/Antigravity-Manager/pull/17) | Remove the optional external-key manager's separate localStorage secret collection. | Session records disappear on reload; validate controlled legacy import and explain retention. Do not replace the whole older page. |
| cc1027114472 [aaf33fc6](https://github.com/cc1027114472/Antigravity-Manager/commit/aaf33fc63191eef0942c1aa1fab67d862bf381b9) | Make proactive OAuth refresh demand-driven, rather than refreshing every loaded expiring reserve account; also fix unchanged proxy-binding idempotency. | Keep recent-business activity distinct from quota polls, expose a policy knob and preserve reactive refresh, durable credential CAS, holds and deadlines. Drop the fork's unrelated capacity/overwrite/cache changes. First-use latency is a tradeoff, not an account-safety guarantee. |
| Parent [PR 3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928), Unix resource half only | Share bounded file-descriptor initialization across Linux/macOS and both executable entrypoints. | The migration half is already fixed. Preserve OS hard limits, never lower an existing limit, and verify platform behavior. |
| Parent [PR 3898](https://github.com/lbjlaq/Antigravity-Manager/pull/3898), four calls only | Remove discarded pure post-transit token-estimation passes in Chat, Responses, Claude and Gemini. | All four callers discard the result. Preserve real pipeline estimation/context logic. The broader 57-file draft remains blocked/conflicted; no measured speedup is claimed. |
| Parent [PR 3924](https://github.com/lbjlaq/Antigravity-Manager/pull/3924) | Explain permanently unavailable account pools with actionable errors. | Reimplement from trusted indexed metadata and typed attention states; drop arbitrary synchronous directory scanning and forbidden-means-banned assumptions. |
| Teee3047 [PR 4](https://github.com/Teee3047/Antigravity-Manager/pull/4) | Preserve caller system identity rather than broad official-persona rewriting. | Keep independent sanitization, assess exact SDK mappings and prompt-prefix/cache behavior; no provider-enforcement prevention claim. |

[Parent details](review-next-upstream.md) and [fork details](review-next-forks.md) include pinned SHAs, authors, changed files, compatibility costs and rejection reasons. The scheduler async-write patch inspected in this pass can publish stale snapshots out of order; the older auth replacements would lose existing capability, IP and transactional admission safeguards. Do not import them wholesale.

Parent #3923/#3927 still have the same broken placeholder heads; #3925 skills and #3926 EOF work are already covered. No existing fix was removed or marked missing merely because its original commit object is absent from this fork's ancestry.

## Carried-forward hardening and CI

The SSE lifecycle still appends incomplete lines without a size bound and renews activity on any nonempty transport chunk (`stream_lifecycle.rs:233,267-294`). The recent monitor capture cap does not cap this separate parser buffer. A bounded frame policy and progress-based idle timer remain worthwhile, with explicit compatibility handling for large image/tool events and existing outer deadlines.

Release preview metadata remains a separate recovery gap: `scripts/ci/release.mjs:200` publishes the immutable release before the preview updater upload at 208. If that upload fails, a normal rerun refuses the already-public release at 195. Prefer an idempotent metadata-finalization path that re-verifies the published signed artifacts and repairs only the channel, without rebuilding every native platform. Existing global publication serialization is present; this review does not claim the workflow lacks a concurrency group.

The version-bump script still writes targets incrementally, warns/continues on missing/unmatched targets, and invokes an unlocked Cargo check whose failures are swallowed. Staging every validated replacement before committing changes remains a release-tooling follow-up. These are previously recorded unresolved issues, not new discoveries in this pass. Existing opt-in cloud packaging behavior should remain.

The earlier dependency inventory has additional advisory matches outside the four patched TLS packages. This pass did not rerun the dependency audit or establish reachability for those unmatched subsystems; do not count an installed package advisory as a proved exploitable application path.

## Inventory and verification limits

Fresh pagination again returned **3,576 listed forks**, **3,475 readable**, **101 explicit NOT_FOUND**, **451 distinct default heads**, and **1,853 PR records** across parent plus all 80 readable fork repositories containing PRs. Parent: 590 PRs, eight open; forks: 1,263; origin: zero. Enumeration errors: zero. All observed default-head, PR head/state/update and push metadata deltas were empty.

The freshly reconstructed CSV bytes exactly match the previous full export, so the tables are retained once at [forks.csv](refresh/forks.csv) and [pull-requests.csv](refresh/pull-requests.csv). New timestamp, hashes, source-tree fingerprint and zero-delta evidence are in [next/summary.json](next/summary.json). Parent main/beta remain `9b82fb7e` / `3f759ae9`; all 14 parent branches and eight open PRs were refreshed by the parent reviewer.

Complete readable metadata is not exhaustive code auditing: all historical patches and nondefault fork branches were not inspected, and unavailable/private repositories cannot be audited. Static control-flow evidence is distinguished from executed reproduction. No application source/settings changes, builds, tests, live credential access, account switching, authenticated provider requests, commits, pushes, merges or remote comments occurred in this review. Only reports and public metadata artifacts were written.
