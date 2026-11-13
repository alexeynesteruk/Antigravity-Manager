# Selected fork patch refresh

Reviewed November 12, 2026, against local `HEAD abbb4da8` **plus the current uncommitted implementation**, including native credential storage, restriction stops, request budgets, log redaction and native LAN HTTPS. This is a bounded selected-patch review, not a claim to have inspected every fork branch. The earlier dated inventories provide discovery; live connected GitHub reads refreshed the selected default heads, PR states and actual patches. No application/configuration changes, builds, tests, real credentials or provider requests were made in this review.

The coordinator's fresh complete metadata census lists 3,576 forks: 3,475 readable, 101 unavailable, and 451 distinct readable default heads. Ten repositories have new/changed heads relative to the previous inventory; most are upstream synchronization, keepalive or CI changes. The new substantive selected patch is Teee3047 PR 4. Those metadata counts do not represent deep patch review of every fork.

The four priority default heads match the earlier inventory; the additional newly changed head is:

| Repository | Live default head |
| --- | --- |
| SanHsien/Antigravity-Manager | `1ece0165a9447feb6627fd9abca1d629f2929e86` |
| DNAlec/Antigravity-Manager | `cdec9c3458a214246d8e2fa16566b1c114ad9707` |
| larry109/Antigravity-Manager | `344795ff5e130d7476391ffbec4379e7d7368f7f` |
| LAZYGASS/Antigravity-Manager | `4e2cb4f85d6d809799d5df6656e8ce440081b632` |
| Teee3047/Antigravity-Manager | `f0e2b8677cbc4cf12099ed3a2b24a3c15084a96a` (changed) |

## Remaining candidates, ranked

### 1. Update the TLS verifier and bundled root certificates beyond the older fork patches

**High priority; a targeted dependency update, not a wholesale fork import.** Discovery sources: SanHsien [PR 56](https://github.com/SanHsien/Antigravity-Manager/pull/56), merged, head [`7b6a9bab80275e239548d94c1de6b9b4d9cb2b18`](https://github.com/SanHsien/Antigravity-Manager/commit/7b6a9bab80275e239548d94c1de6b9b4d9cb2b18), and [PR 10](https://github.com/SanHsien/Antigravity-Manager/pull/10), merged, head [`858774b1fe9022fcbc33fdd8e8fab3b1430ef242`](https://github.com/SanHsien/Antigravity-Manager/commit/858774b1fe9022fcbc33fdd8e8fab3b1430ef242). Both PR authors are `dependabot[bot]`.

Actual patch 56 changes only the `rustls` lock entry from 0.23.45 to 0.23.46. The current local lock still resolves `rustls 0.23.36`, `rustls-webpki 0.103.9`, `webpki-roots 1.0.6`, and `webpki-root-certs 1.0.6`. The older fork's WebPKI update to 0.103.15 is no longer sufficient:

- [RUSTSEC-2026-0382](https://github.com/rustsec/advisory-db/blob/main/crates/rustls-webpki/RUSTSEC-2026-0382.md) requires `rustls-webpki >=0.103.16` on the stable 0.103 line to reject paths through compromised emSign intermediate public keys.
- [RUSTSEC-2026-0383](https://rustsec.org/advisories/RUSTSEC-2026-0383.html) requires `webpki-root-certs >=1.0.10`. Its 0.26.11 compatibility wrapper reexports its resolved 1.x dependency; the wrapper's presence does not make the currently resolved 1.0.6 root list safe. The [1.0.10 release](https://github.com/rustls/webpki-roots/releases/tag/v/1.0.10) removes the affected root.
- [Rustls 0.23.46](https://github.com/rustls/rustls/releases/tag/v/0.23.46) includes session-secret Debug redaction and other TLS fixes. [RUSTSEC-2026-0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html) explicitly excludes ordinary connection and built-in session-cache Debug output; a local secret-logging exploit has not been established.

Reachability matters: local `rquest 5.1.0` uses BoringSSL, **and** depends on `webpki-root-certs 0.26.11 -> 1.0.6`. Thus the root-list update is relevant to the main outbound client even though that client's verifier is not Rustls. Direct `reqwest 0.12` enables `rustls-tls` in `src-tauri/Cargo.toml:39` and uses `rustls-webpki`/`webpki-roots`; examples include update/version fetching and the optional cloudflared downloader. The updater plugin's separate Reqwest 0.13 path uses platform verification. The new incoming TLS listener uses no client-certificate authentication, so the WebPKI advisory is not evidence of an incoming client-authentication bypass.

**Accept:** refresh the specific compatible lock entries to patched versions, including the 1.x parent behind the compatibility wrapper, and verify the resulting resolved graph and TLS behavior. Preserve the current Ring feature selection and avoid introducing AWS-LC accidentally. **Drop:** copying an older fork lockfile, stopping at WebPKI 0.103.15, or claiming every TLS advisory is reachable in every client. Costs include intentional rejection of distrusted certificate chains, changed TLS behavior, and required cross-platform compilation/transport checks. This source review did not run an audit or update dependencies.

### 2. Keep Web administrator credentials in memory and fence stale unauthorized responses

**High value, with an explicit login-behavior change.** SanHsien [PR 15](https://github.com/SanHsien/Antigravity-Manager/pull/15), merged; author `SanHsien`; head [`fd4ced5eacdef05e1d3606e48c2c3b8172024b3f`](https://github.com/SanHsien/Antigravity-Manager/commit/fd4ced5eacdef05e1d3606e48c2c3b8172024b3f).

The patch adds `src/utils/adminAuth.ts` with an in-memory key and authentication epoch, uses that epoch in `request.ts` so an old request's 401 cannot clear a newer login, clears old browser storage, and moves initialization below the authenticated component boundary. It also treats only a successful verification response as a successful login.

Current `src/components/common/AdminAuthGuard.tsx:24-35` restores session/local storage; lines 62, 77 and 91 still write the administrator key. The non-401 error branch accepts authentication even after a failed verification request. `src/utils/request.ts:250` still reads both stores. `src/App.tsx:88-94` runs initialization in the component whose rendered children include the guard, so hiding children alone does not gate that parent's effects.

**Accept:** memory-only key handling, successful-login publication, removal of legacy storage, request epochs, and mounting protected initialization after authentication. Adapt all current consumers: guard, request helper and both logout controls. **Drop:** replacing the entire older App/guard files, losing local MiniView/cross-window behavior, or presenting a frontend guard as backend authorization. The backend must retain its strict administrator route checks.

Costs: page reloads and new tabs require authentication; the existing Remember Me control must be removed or redesigned deliberately. Do not silently promise persistent login. In-memory storage reduces persisted browser-profile/backup exposure; it does not defeat active same-origin script compromise. Desktop Tauri behavior must remain independent of Web login.

### 3. Remove persistent browser storage of managed external API keys

**Useful where the optional APIKEY.FUN manager is used.** SanHsien [PR 17](https://github.com/SanHsien/Antigravity-Manager/pull/17), merged; author `SanHsien`; head [`dab936ab62720f626d3644b2c8b9eed3a52de097`](https://github.com/SanHsien/Antigravity-Manager/commit/dab936ab62720f626d3644b2c8b9eed3a52de097).

The actual `src/pages/ApiKeyFun.tsx` patch replaces persistent `apikey_fun_managed_keys_local` writes with module-session memory, removes the old storage item during import, and adds a clear retention notice. Current local lines 116 and 130 still read/write the full managed-key list in localStorage. The new manager account/config/device-key vault does not automatically cover this separate frontend store.

**Accept:** stop new browser persistence, remove the old item after a controlled import, retain session navigation behavior, and explain retention in context. Validate imported entries rather than accepting every JSON array blindly. **Drop:** the older page file as a whole; preserve local query-generation guards, provider ownership checks and current controls. A desktop-specific native-vault alternative can be designed separately if persistent profiles are required.

Costs: a reload/full exit loses session key records, labels and cached balance data; closing the desktop window to its tray does not necessarily end its JavaScript session. This is a confidentiality improvement for that optional feature, not a change to Google-account eligibility or egress routing.

### 4. Preserve caller system identity rather than rewriting it into an official persona

**Fresh substantive fork change; useful integrity correction with behavior costs.** Teee3047 [PR 4](https://github.com/Teee3047/Antigravity-Manager/pull/4), merged; author `Teee3047`; PR head [`6fa9af518245a0096010fa014af1e9e31d1c5355`](https://github.com/Teee3047/Antigravity-Manager/commit/6fa9af518245a0096010fa014af1e9e31d1c5355); current main merge [`f0e2b8677cbc4cf12099ed3a2b24a3c15084a96a`](https://github.com/Teee3047/Antigravity-Manager/commit/f0e2b8677cbc4cf12099ed3a2b24a3c15084a96a).

The two-file patch replaces `PromptSanitizer::normalize_system_identity` with original-text pass-through and changes corresponding OpenAI adapter assertions. Its DSH fixture covers a harness identity followed by `gemini-3.8-flash-high/low`: the old sentence matcher could replace/truncate at the version decimal and leave `8-flash-* model.` glued to a repeated official introduction. It restores unrelated tool/schema/framework tests rather than deleting those guards.

The objective applies to the current tree: `prompt_sanitizer.rs:530-591` still rewrites system identity through `normalize_text_identity`, including additional brand/XML rewriting beyond the older source baseline. Both direct system parts and anchored identity regions reach this helper. The source nevertheless retains `clean_text` and exact-string `normalize_client_identity` mapping; local lines 600-605 and 1064-1065 likewise map certain SDK introductions before the broad normalizer. Therefore this PR is **not** evidence that every identity or all caller text becomes byte-for-byte pass-through.

**Accept:** preserve caller-authored system identity in the shared sanitizer and adapt the DSH/version, multi-part system, user/tool preservation regressions to the current pipeline. Keep billing-metadata, media-source, schema-size, signed-thinking and tool-argument guards independent. Explicitly decide whether the existing exact SDK mapping remains desired, documenting it if retained. **Drop:** replacing the older sanitizer/request files wholesale, carrying unused identity regexes solely under `allow(dead_code)`, changing outbound headers/fingerprints, or presenting this as a proven 429/account-restriction remedy.

Costs: removes an intentional existing identity-rewrite behavior, changes cached prompt prefixes, and requires all four adapters' shared output checks. Assess whether persisted cache assumptions change; use the repository's one-time cache-upgrade guidance only if required. The source reports focused tests; this review did not reproduce its production observation or run its tests.

### 5. Make QoS thresholds an instance policy while preserving current ownership guards

**Optional operational hardening, medium priority.** DNAlec [PR 4](https://github.com/DNAlec/Antigravity-Manager/pull/4), merged; author `DNAlec`; head [`7a0babd6abdc496aea754c027faf7657716c854c`](https://github.com/DNAlec/Antigravity-Manager/commit/7a0babd6abdc496aea754c027faf7657716c854c).

The patch adds validated `proxy.qos` configuration, instance snapshots, policy readback, and disk/desktop/HTTP hot-update wiring. An already claimed probe keeps its policy snapshot; updating thresholds does not release an existing jail. The current tree has probe claims and versioned leases, but no `ProxyConfig.qos`; thresholds remain fixed at 20/35 seconds and recovery 18.5 seconds in `qos_guardian.rs:449-453`, a fixed ladder at line 9, and an eight-second probe deadline in `token_manager.rs:8037`.

**Accept:** validated partial defaults, instance ownership, claim-time snapshots, state-preserving hot updates and authoritative runtime readback. Add contextual GUI controls if exposing these knobs, with file/API parity. **Drop:** old whole-module replacements, any removal of current account-attention holds or request budgets, and changes that enable automatic QoS jailing by default. Current `qos_auto_jail` defaults false; retain that preference and separate it from global restriction stops.

Costs: nontrivial integration across config/save/startup/guardian paths; full-save serialization must coexist with current config/vault locking. Thresholds describe local observations, not proof that a provider account is throttled or safe. This is an optional availability control, not a reason to add probes or weaken provider restriction handling.

## Deferred legacy-codec follow-up

SanHsien [PR 19](https://github.com/SanHsien/Antigravity-Manager/pull/19), merged; author `SanHsien`; head [`74f1084c1fe8d0bf4dbc7306ffb061d374652d8c`](https://github.com/SanHsien/Antigravity-Manager/commit/74f1084c1fe8d0bf4dbc7306ffb061d374652d8c), remains a separate migration concern below the five candidates above.

The source rejects failed/empty machine identities instead of deriving an encryption key from `default`; it retains legacy decoding fixtures and adds tamper/identity-error coverage. Local `src-tauri/src/utils/crypto.rs:16` still falls back to `default`, and line 41 has an Android shared fallback. Its serializer/deserializer remain attached to `ProxyAuth.password`.

**Accept:** fail-closed identity resolution for new legacy-codec encryption and focused compatibility/tamper coverage, or eliminate new use of the codec at explicit serialization boundaries. **Drop:** treating machine-ID-derived encryption as a native vault, blindly rejecting old weak ciphertext without a recovery plan, and broad crypto changes bundled into unrelated account work. The native vault now protects managed persisted credentials; this candidate addresses the remaining compatibility serializer, not evidence that the new vault uses a shared encryption key.

Costs: machines without a usable identifier can no longer serialize/decrypt through this path, and historical ciphertext created with the shared fallback requires deliberate recovery/re-entry. Keep migration error reporting explicit so ciphertext cannot silently become the effective password.

## Rechecked and excluded from this new integration list

- DNAlec probe claims, versioned session leases and cooldown acquisition budgets are already present in `qos_guardian.rs`, `session_bindings.rs`, `token_manager.rs` and `pipeline/token_budget.rs`. Their older recommendations are not outstanding new imports.
- SanHsien directory identity/Windows attribute-handle guards and internal warmup capability/no-redirect goals have been adapted locally. The newer managed-account model-reader path from PR 54 is also present; do not replace it with an older cached-path reader.
- Larry109's bounded single-account grace retry [`cbc30ef003ec65064a79f70e8ce2f437f023f65e`](https://github.com/larry109/Antigravity-Manager/commit/cbc30ef003ec65064a79f70e8ce2f437f023f65e) and SanHsien's companion [`b6cca13be58608563cc1a07e16055147349e3e8d`](https://github.com/SanHsien/Antigravity-Manager/commit/b6cca13be58608563cc1a07e16055147349e3e8d) identify the same class already bounded in local `pipeline/rotation.rs:304-315`. Preserve the newer shared policy instead of importing their old handler files.
- LAZYGASS's redaction objective is now covered by shared future-write redaction hooks. Its old substring matcher is less precise and is not a new missing protection. Historical logs/backups were not audited or erased by this review.
- SanHsien tenant/signature patches and OpenCode profile ownership checks duplicate current scoped caches, tenant-bound Responses history and managed-provider removal restrictions.
- CC1027114472's fixed-model recovery scheduler remains unsuitable: a successful probe of another model must not clear a newer/model-specific restriction or add unsolicited traffic. No fingerprint/evasion claim was accepted as an account-safety mechanism.

Recommended sequence: targeted TLS/root trust update, then a deliberate Web credential-retention decision. Managed external-key retention, system-prompt integrity and configurable QoS are separate changes with explicit compatibility costs. Upstream CI and author-reported tests are source evidence only; no candidate has been rebased, built or runtime-verified by this refresh.
