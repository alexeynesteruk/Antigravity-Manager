# Fork follow-up after the five local fixes

Read-only review on November 12, 2026, against `HEAD abbb4da8` plus the current working tree. The TLS/root updates, bounded image fan-out, non-destructive monitoring, listener Host protection and migration-marker changes are already present and are not recommended again here. Live connected GitHub reads supplied selected PR metadata and actual patches; existing complete metadata inventories supplied discovery. No application/source/configuration edits, builds, tests, provider calls or remote comments were made. This document is the only new repository file from this review.

The selected live SanHsien head remains `1ece0165a9447feb6627fd9abca1d629f2929e86`; Teee3047 remains `f0e2b8677cbc4cf12099ed3a2b24a3c15084a96a`. Unchanged heads do not establish that every nondefault branch is unchanged. The coordinator's separate refreshed census governs complete inventory counts.

## Decisions

No newly inspected patch is a safe wholesale merge into the current gateway. The strongest remaining changes are deliberate browser-secret retention and prompt-integrity decisions from the previous shortlist. This pass additionally inspected idle OAuth refresh/binding idempotency, scheduler I/O, alternate auth implementations and mixed-tool injection instead of treating older descriptions as fresh fixes.

### 1. Browser administrator credentials: adopt the complete authentication lifecycle, not just storage replacement

Source: SanHsien [PR 15](https://github.com/SanHsien/Antigravity-Manager/pull/15), author `SanHsien`, merged; [`fd4ced5eacdef05e1d3606e48c2c3b8172024b3f`](https://github.com/SanHsien/Antigravity-Manager/commit/fd4ced5eacdef05e1d3606e48c2c3b8172024b3f). Files: `src/utils/adminAuth.ts`, `request.ts`, `App.tsx`, `components/common/AdminAuthGuard.tsx`, both navbar logout controls.

Still applicable: the current guard restores and writes `abv_admin_api_key` in session/local storage, accepts non-401 failed verification responses as authenticated, and runs below `App`'s initialization effects. The source's in-memory key is only one part of the change; its authentication epoch prevents an old request's 401 from removing a newer login.

**Accept/adapt:** successful verification before publishing credentials, authenticated mounting of configuration/account initialization, request-generation checks, in-memory retention, and removal of old storage entries. Preserve current Tauri/MiniView/cross-window behavior and strict backend authorization. **Drop:** importing the old App/guard files wholesale or claiming that memory storage prevents active same-origin script compromise.

Cost: page reload/new tab requires login; the existing Remember Me setting must change explicitly. Persistent login without retaining the full administrator credential would require a separate server-session design, not a misleading storage toggle. This remains higher value than a cosmetic timing-comparison patch.

### 2. Managed external API keys: stop the separate localStorage persistence

Source: SanHsien [PR 17](https://github.com/SanHsien/Antigravity-Manager/pull/17), author `SanHsien`, merged; [`dab936ab62720f626d3644b2c8b9eed3a52de097`](https://github.com/SanHsien/Antigravity-Manager/commit/dab936ab62720f626d3644b2c8b9eed3a52de097). Files: `src/pages/ApiKeyFun.tsx` and retention notices in locales.

Current `ApiKeyFun.tsx` still saves full managed key records under `apikey_fun_managed_keys_local`. The new native account/config/device-key store does not cover this frontend-only collection.

**Accept/adapt:** memory retention across navigation, controlled import/removal of old records, validation of imported record shapes, and an accurate retention notice. Keep local query sequence guards and managed-provider ownership restrictions. **Drop:** the older page as a whole, or silently changing persistence while leaving “saved keys” promises unchanged.

Cost: full reload/exit loses session records and their labels/balance metadata. A desktop native-vault profile store could preserve persistence separately, with Web-only memory behavior; the source itself implements only memory retention.

### 3. Caller system identity: preserve integrity through the current shared pipeline

Source: Teee3047 [PR 4](https://github.com/Teee3047/Antigravity-Manager/pull/4), author `Teee3047`, merged; PR head [`6fa9af518245a0096010fa014af1e9e31d1c5355`](https://github.com/Teee3047/Antigravity-Manager/commit/6fa9af518245a0096010fa014af1e9e31d1c5355), main merge [`f0e2b8677cbc4cf12099ed3a2b24a3c15084a96a`](https://github.com/Teee3047/Antigravity-Manager/commit/f0e2b8677cbc4cf12099ed3a2b24a3c15084a96a). Files: `proxy/mappers/prompt_sanitizer.rs`, `mappers/openai/request.rs`.

The local broad normalizer still rewrites caller identity, including brand/XML cases beyond this older patch baseline. The source changes `normalize_system_identity` to pass-through and covers a DSH/Gemini-version fragment regression. It deliberately keeps Header cleaning and exact SDK-string mapping; therefore it does not establish complete identity pass-through.

**Accept/adapt:** caller-owned system-text integrity and its multi-part/version/user/tool regressions at the shared sanitizer. Keep billing metadata removal, media policy, schema bounds, signed thinking and tool guards independent. Decide and document the exact SDK mapping separately. **Drop:** copying the old file, unused regex/dead-code baggage, header fingerprint changes, or claims of proven restriction/429 prevention.

Cost: removes existing rewrite behavior and changes prompt prefixes. All four adapters need output-level regression coverage; a one-time cache prompt is warranted only if the final implementation actually invalidates persisted assumptions.

### 4. Idle OAuth refresh: adapt the traffic-policy objective without the fork's account machinery

Newly inspected source: cc1027114472 [`aaf33fc63191eef0942c1aa1fab67d862bf381b9`](https://github.com/cc1027114472/Antigravity-Manager/commit/aaf33fc63191eef0942c1aa1fab67d862bf381b9), author `cc1027114472`. Files: `proxy/token_manager.rs`, `modules/account.rs`, `proxy/server.rs`.

The relevant hunk restricts proactive refresh to accounts with recent real business requests instead of refreshing idle reserve accounts. Current `token_manager.rs:695-716` still selects every loaded token approaching its ten-minute refresh buffer; its worker runs every thirty seconds. The new global safety permit, eligibility checks, bounded refresh and credential-revision publication remain authoritative and must be retained. Their correctness does not itself make idle refresh demand-driven.

**Accept/adapt:** an explicit proactive-refresh policy, such as current behavior versus recently used accounts versus on-demand, driven by genuine accepted business activity. Preserve first-use refresh, single-flight locks through durable CAS publication, rotated refresh tokens, manual/global holds and bounded operation deadlines. Make the policy reachable in contextual settings and headless configuration. **Drop:** copying the fork's unrelated capacity tiers, warm reserve sizing, older token-merge/persistence logic, or its “anti-ban” claim.

Cost: reduced background traffic trades against the first request's refresh latency. The source's one-hour usage test and hardcoded reserve sizes are not universal policy. Current import refreshes already resolve managed identities and retain credential revisions through `oauth::refresh_access_token_for_import` / `token_data_for_import`; do not replace that with the fork's older in-memory timestamp synchronization.

### 5. Proxy binding idempotency: useful small objective, separate from egress policy

Same source commit [`aaf33fc63191eef0942c1aa1fab67d862bf381b9`](https://github.com/cc1027114472/Antigravity-Manager/commit/aaf33fc63191eef0942c1aa1fab67d862bf381b9). The batch-import hunk skips assigning an account to the proxy it already uses.

Current `proxy/proxy_pool.rs:438-480` checks maximum account capacity before considering an unchanged binding. Consequently, assigning the same account again to a full one-account proxy can incorrectly fail; otherwise it rewrites the same binding/configuration. The fork's bulk-import endpoint and egress-settle machinery are not identical to the current API, so this is a concept-level adaptation to the central binding operation.

**Accept/adapt:** recognize an unchanged validated account/proxy assignment inside the binding operation's serialization boundary; retain capacity checks for genuine new assignments and durable error reporting. Cover same-binding-at-capacity, genuine moves, concurrent capacity reservations and explicit unbind semantics. **Drop:** the source's refusal to honor an explicit overwrite merely because the old proxy is healthy, and its broad removal of client-cache invalidation. Those change operator intent and can preserve stale transport configuration.

Cost: modest binding/persistence concurrency work. No claim is made that reassigning the same proxy currently triggers the source fork's egress cooldown, or that this prevents provider restrictions.

## Newly screened submissions to reject or rework

| Source | Actual patch / current comparison | Decision |
| --- | --- | --- |
| LAZYGASS [PR 7](https://github.com/LAZYGASS/Antigravity-Manager/pull/7), open; [`1290aaf56878045cee006c0a9c5ecd28513020eb`](https://github.com/LAZYGASS/Antigravity-Manager/commit/1290aaf56878045cee006c0a9c5ecd28513020eb) | Makes scheduler history writes asynchronous, but releases the history mutex before awaiting independent whole-file writes. Two snapshots can finish out of order and restore stale cooldown state. Current `modules/scheduler.rs:30-42` instead blocks under the mutex and ignores write errors; neither is a complete durable async design. | Do not merge as-is. A serialized/coalesced atomic writer with visible failure and ordered acknowledgement is the useful follow-up. Preserve scheduler opt-in and global/account holds; do not add warmup traffic. |
| LAZYGASS [PR 2](https://github.com/LAZYGASS/Antigravity-Manager/pull/2), open; [`10af9e2d6fec45e0909e1fecc300eb6dc0c34db4`](https://github.com/LAZYGASS/Antigravity-Manager/commit/10af9e2d6fec45e0909e1fecc300eb6dc0c34db4) | Replaces one old auth comparison with a hand-written XOR loop. It does not establish compiler-level constant-time behavior or cover current administrator/internal/token-admission paths. | Do not replace current auth. If pursued, use an audited comparison primitive at current secret comparisons, preserving credential precedence and capability checks. No network timing exploit was reproduced. |
| taiab0x1 [PR 1](https://github.com/taiab0x1/Antigravity-Manager/pull/1), open; [`87a6e5f6e3fb73bdfa0c35bda26096fea4b99aff`](https://github.com/taiab0x1/Antigravity-Manager/commit/87a6e5f6e3fb73bdfa0c35bda26096fea4b99aff) | Extracts old auth helpers and injects identities into requests, but still bypasses `/internal/*` without the current process capability, trusts its old forwarded-IP path, and separates admission from identity lookup. | Its useful identity injection and strict admin behavior are already local. Reject the old auth file; it would lose current stronger checks and transactional device-key admission. |
| dwninfinity [PR 1](https://github.com/dwninfinity/Antigravity-Manager/pull/1), merged; [`711a08ab5733d5941b4466905b3e458155de2cf3`](https://github.com/dwninfinity/Antigravity-Manager/commit/711a08ab5733d5941b4466905b3e458155de2cf3) | Stops injecting Google Search alongside function declarations. Current `mappers/common_utils.rs:526-535` already does this for both camelCase and snake_case declarations. | Already covered more broadly; no new import. |
| vecyang1 [`7f025e6ab0394fb9ba71e469862635bbf93479ec`](https://github.com/vecyang1/Antigravity-Manager/commit/7f025e6ab0394fb9ba71e469862635bbf93479ec) | Adds an environment template and replaces documented credential paths with `internal-docs` in changelogs. No runtime code changes in that commit. | Not runtime privacy hardening; reject documentation erasure as a security integration. |

DNAlec's configurable QoS policy remains an optional existing backlog item, not a newly discovered hardening. The crypto shared-fallback issue is likewise a separate compatibility-codec migration concern after the native vault. Earlier TLS dependency advice is now implemented and should not be counted as outstanding fork work.

These findings are static patch/current-source comparisons. They do not claim that author-reported tests validate a local adaptation, that every historic branch was audited, or that any patch can prevent provider account restrictions.
