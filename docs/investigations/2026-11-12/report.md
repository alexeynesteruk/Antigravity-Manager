# Antigravity Manager integration review

Reviewed November 12, 2026, against local `main` at `1e6e90b6a613e26cb2b4b06a6ebd29588fa92b85`.

The best additions are upstream's skills pass-through, sticky-session and transport fixes, DNAlec's probe ownership and versioned session leases, and SanHsien's directory-identity guards. Integrate them as separate functional changes on a staging branch, preserving this fork's existing safeguards. Several local security and persistence defects deserve attention before importing more features.

## Baseline and coverage

| Item | Result |
| --- | --- |
| Local repository | `alexeynesteruk/Antigravity-Manager`, manifests `5.0.4-beta.5` |
| Parent and source | [lbjlaq/Antigravity-Manager](https://github.com/lbjlaq/Antigravity-Manager) |
| Upstream main | `9b82fb7ef51c42e9d1662a14bd5b718da0525cd8`; already an ancestor of local HEAD |
| Upstream beta | [`3f759ae9a36fb0b33427fe4f8a4acded4a862372`](https://github.com/lbjlaq/Antigravity-Manager/commit/3f759ae9a36fb0b33427fe4f8a4acded4a862372), `5.0.4-beta.7` |
| Divergence from upstream main | 139 local-only commits, zero upstream-only commits |
| Divergence from upstream beta | 74 local-only commits, 26 upstream-only commits |
| Beta changes since the common ancestor | 89 files, 7,208 insertions, 1,304 deletions; this is the upstream side, not a proposed merge's final diff |
| Listed public forks | 3,574 unique records across 36 API pages; displayed upstream counter was 3,536 |
| Readable fork repositories | 3,473; the other 101 returned explicit GraphQL `NOT_FOUND` |
| Distinct readable default heads | 452; 1,619 forks have heads in local ancestry, 1,854 do not |
| Forks with PRs | 81; all their PR collections were paginated |
| Upstream PRs | 583: 386 merged, 196 closed without merge, one open |
| Fork PRs | 1,265: 1,180 merged, 23 closed without merge, 62 open |
| Origin PRs | Zero |
| Total PR metadata records | 1,848; no PR enumeration errors |

Three parallel agents reviewed local correctness, upstream changes, and selected fork patches; the primary agent cross-checked findings and refreshed the full metadata inventory. Compared with the existing November 5 snapshot, 54 readable fork default heads changed or appeared, representing 16 distinct SHAs. Most were upstream synchronization. Selection also revisited previously identified unique fork work and nondefault upstream branches.

This is comprehensive metadata screening with selected code review. It does not establish that every line, nondefault branch, private fork, or detached fork was inspected. `NOT_FOUND` cannot distinguish deletion, access restrictions, or suspension. An ahead count or recent push does not establish useful missing functionality.

The appendices preserve the reviewable inventory: [forks.csv](forks.csv), [pull-requests.csv](pull-requests.csv), and [summary.json](summary.json). The November 5 investigation is retained unchanged as historical evidence.

## Current defects to address first

Priorities describe consequences in the affected deployment. These are static findings; application behavior and exploitability were not exercised.

### Untrusted media sources can read server files

**P1.** [OpenAI image conversion](../../../src-tauri/src/proxy/mappers/openai/request.rs), lines 537–551, accepts filesystem references and reads them before image validation. [Audio normalization](../../../src-tauri/src/proxy/audio/mod.rs), lines 90–107, accepts `file://` and existing paths, reads the entire file, and encodes arbitrary bytes under an audio MIME type. Oversized local audio produces a warning rather than rejection. A caller with gateway access can cause server-local file bytes to enter an upstream payload through the audio path; image validation does not prevent image-side filesystem I/O.

[LAZYGASS PR 3](https://github.com/LAZYGASS/Antigravity-Manager/pull/3), author `LAZYGASS`, addresses the image-side objective. Accept the restriction in `mappers/openai/request.rs`, broaden it to shared image/audio source admission, and exclude `.jules/sentinel.md`. Return a clear validation error rather than silently omitting media. Reject filesystem references before conversion performs I/O; a canonical Gemini pipeline check after conversion is too late. Retain supported upload/data/remote URL behavior, with any trusted local import exposed separately. Server-local-path clients would need to upload content instead.

### Disabled user tokens remain authorized and IP admission races

**P1 for disabled-token admission; P2 for capacity races.** [user_token_db.rs](../../../src-tauri/src/modules/user_token_db.rs), line 592 onward, validates expiry, IP restrictions, and curfew without checking `enabled`. Lookup at line 343 includes disabled records. [Authentication](../../../src-tauri/src/proxy/middleware/auth.rs), line 273, accepts successful validation.

IP validation at `user_token_db.rs:612` checks binding counts without reserving a slot. Binding insertion occurs later during response accounting. Two different IPs can both pass a one-IP limit while the binding table is empty, then become permanently bound. Some count/lookup errors also default permissively.

Use one serialized admission operation for enabled/expiry/restriction checks and atomic IP reservation, returning the admitted identity. Keep usage counts in response accounting and unrestricted lookup available to administration screens. Preserve primary-key, administrator, internal-request, and auth-off semantics. No external patch was established as a ready fix for this combined defect.

### Headless host selection expands the listening boundary

**P1.** [server.rs](../../../src-tauri/src/bin/server.rs), line 50, accepts an arbitrary `--host` value. [lib.rs](../../../src-tauri/src/lib.rs), line 171, recognizes only exact `127.0.0.1` or `localhost`; every other value sets `allow_lan_access=true`. [ProxyConfig](../../../src-tauri/src/proxy/config.rs), line 1440, then returns `0.0.0.0`, which the listener expands to wildcard IPv4/IPv6 where supported.

Consequently, `--host ::1`, `--host 127.0.0.2`, or a specific interface address does not honor the requested boundary. Authentication remains enforced; this is unintended network exposure, not an authentication bypass. Preserve and validate the actual host through startup and binding, or reject unsupported values. Reject missing and unknown CLI arguments as well. This headless feature is already imported; the November 5 report's advice to defer it is no longer its current status.

### Malformed configuration is overwritten during headless startup

**P1/P2 depending on the stored policy.** [lib.rs](../../../src-tauri/src/lib.rs), line 163, uses `load_app_config().unwrap_or_default()`. A malformed existing JSON file produces defaults; default auth normalization sets `modified=true`, and line 278 saves those defaults over the repairable original. [The loader](../../../src-tauri/src/modules/config.rs), lines 36 and 70, propagates errors and creates no recovery backup; atomic replacement does not retain the original contents.

This is a regression introduced by headless extraction in `104cea6f`: the earlier headless startup explicitly exited on configuration-load failure. Restore that failure behavior. Missing or intentionally empty initialization should remain a distinct loader decision. This can otherwise discard credentials, mappings, and security policy while starting with defaults.

### Stream activity does not prove generation progress

**P2.** [stream_lifecycle.rs](../../../src-tauri/src/proxy/pipeline/stream_lifecycle.rs), lines 230–293, renews `last_activity` for every nonempty transport chunk, including comments, blank lines, and incomplete SSE lines. The unfinished-line buffer has no explicit bound. A stalled generation can outlive the configured inactivity limit and hold resources until a broader deadline; the upstream client's overall cap is 7,200 seconds, so the exposure is not necessarily unlimited.

Track transport liveness separately from complete-event generation progress in the shared pipeline. Adapters should report progress for valid thinking, text, and tool events. Bound unfinished frames, preserve the existing empty-chunk guard, and retain trailing-buffer processing before EOF validation. Also bound persistence acknowledgement: `stream_lifecycle.rs:541` awaits `finalize_barrier()`, whose Codex implementation waits on `ack_rx` without a deadline at `mappers/openai/streaming.rs:1747`. The shared lifecycle migration is already integrated; these are remaining lifecycle defects.

### Batch deletion performs irreversible work before saving the index

**P2.** [account.rs](../../../src-tauri/src/modules/account.rs), lines 3356–3393, removes credential files and triggers runtime eviction before saving the changed index. File deletion errors are ignored. An index-write failure leaves references to removed credentials; locked Windows files can survive silently while deletion reports success.

Apply the safer single-account ordering already present around line 3337: validate paths, persist membership changes first, then clean files with explicit partial-failure reporting and retry. Keep source recovery and managed-path protections. This is a residual batch-path defect, not evidence that the upstream orphan-account fix is entirely missing.

### Pedal mode restoration depends on one browser

**P2.** [usePedalModeStore.ts](../../../src/stores/usePedalModeStore.ts), lines 175–225, persists boosted backend priorities before storing their original values in browser `localStorage`. Automatic restoration runs only in [Layout.tsx](../../../src/components/layout/Layout.tsx), lines 43 and 56. Closing the originating UI, clearing its storage, switching browsers, or crashing before the snapshot is saved leaves the server without durable restoration metadata.

Recovery and manual exit also remove the snapshot before restoring all accounts, swallow write failures, and display success. Persist the mode and original priorities in the backend, retain restoration state until writes succeed, and expose equivalent headless controls. UI polling can display that backend state. The feature is already integrated, so the appropriate next step is lifecycle repair rather than another import.

## Recommended upstream integrations

### Skills should pass through unchanged

**High value, small scope.** [Commit fb987bde](https://github.com/lbjlaq/Antigravity-Manager/commit/fb987bde479a3a4d179747533d1d3033852d246e), author `lbjlaq`, fixes [issue 3914](https://github.com/lbjlaq/Antigravity-Manager/issues/3914).

Local `mappers/common_utils.rs:1666` injects instructions claiming the client has a `shell_command` tool and must use Windows `Get-Content` whenever a skills closing tag is present. `mappers/openai/context_blocks.rs:20` calls it. This changes client instructions and can induce unavailable tool calls or platform-inappropriate commands.

Accept removal of the helper and call, plus the verbatim-preservation regression. This directly supports cross-platform compatibility and prompt preservation. Issue 3914 is not a PR.

### Preserve sticky sessions and streaming QoS context

**High value.** [PR 3893](https://github.com/lbjlaq/Antigravity-Manager/pull/3893), author `mclinyeshi`, and [PR 3895](https://github.com/lbjlaq/Antigravity-Manager/pull/3895), author `cubelikeplayDaniel`.

Local `pipeline/stream_peek.rs:218` stores a callback without retaining its request task-local budget context. Streaming TTFT processing after the handler returns can lose estimated input tokens, removing prefill allowances for large cold prompts. Local `token_manager.rs:4637` also migrates healthy sticky sessions solely because another account has more quota.

Accept context capture/restore, Responses estimation and affinity corrections, and removal of quota-disparity migration. Both source PR heads passed all eight checks. Review these together with DNAlec's cooldown-wait companions below: the newer upstream waiting path can otherwise clear a concurrently renewed cooldown or conflict with the existing acquisition watchdog.

### Adopt the final transport and upload policy together

**High value, substantial integration.** [PR 3892](https://github.com/lbjlaq/Antigravity-Manager/pull/3892), `cubelikeplayDaniel`; [PR 3899](https://github.com/lbjlaq/Antigravity-Manager/pull/3899), `mclinyeshi`; [PR 3906](https://github.com/lbjlaq/Antigravity-Manager/pull/3906), `jeikl`.

The final chain distinguishes in-budget transport resets from actual timeout, retries suitable sticky transports once in place, avoids cross-account amplification of failed uploads, detects upload stalls/progress, and avoids penalizing model breakers for correctable client-version errors. Local `pipeline/terminal.rs:113` still stops rotation for all terminal failures, without the final mode-aware classification.

Accept the final shared policy across `pipeline/{terminal,rotation,budget,policy}.rs`, `upstream/client.rs`, handler outcome reporting, and matching configuration/settings surfaces. Do not cherry-pick the first patch alone: subsequent changes intentionally revise its replay policy. Preserve fixed global deadlines, local tenant isolation, cancellation, streaming completion, and bounded body replay. Include slow-upload and concurrent-account acceptance checks on the resulting combination.

### Make thinking controls and dynamic specifications effective

**Medium to high value.** [Commit f4a83aca](https://github.com/lbjlaq/Antigravity-Manager/commit/f4a83aca1701d232d53a35375e3c36e140f68214), author `jeikl`.

Accept three functional fixes: honor explicit client thinking disable in `pipeline/inbound.rs` (local line 1668 ignores `_client_switch`); use `claude_mode` for Claude custom budgets in `model_specs.rs` (local line 587 uses Gemini mode); and use dynamic tier resolution in `common/variant_mapping.rs`. Include relevant regressions.

Retain local `build.rs`. Its target-OS/environment checks, `new_without_app_manifest`, and `/MANIFEST:EMBED` handling already cover more cases than the source commit's Windows addition.

### Review prompt normalization and structured 429 handling as separate concerns

**Medium to high value, broader review needed.** [PR 3913](https://github.com/lbjlaq/Antigravity-Manager/pull/3913), author `jeikl`, with subsequent changes by `sinderson123-afk`.

Useful pieces include cross-part singleton identity normalization, version parsing, punctuation repair, structured 429 classification, a first-attempt grace retry, account-and-model failure counters, and distinguishing gateway errors from upstream errors. Config/API/environment/UI surfaces are included, including `request_429_grace_ms` and `ABV_REQUEST_429_GRACE_MS`.

Review the new prompt rewrite defaults separately. Broad deletion patterns such as `*WorkBuddy*` must not remove user queries, supplied skills, or pipeline-owned instructions. Preserve this fork's narrower sanitization safeguards. Identity-prefix changes can invalidate cached prefixes; use the repository's one-time cache guidance only if the final implementation requires it.

### Adapt data-plane account health probing

**Medium value.** Beta.6 commits [f6d5ef1f](https://github.com/lbjlaq/Antigravity-Manager/commit/f6d5ef1fc6b689724e76cf3ce3a888edd5c339db), [e454cd80](https://github.com/lbjlaq/Antigravity-Manager/commit/e454cd80ab1c8fbae8217d58b95418ad58774fc0), and [6e95f47d](https://github.com/lbjlaq/Antigravity-Manager/commit/6e95f47d22f7ef43b4bf255b529c6631d7fba379), author `jeikl`.

Control-plane success does not prove inference access. The source moves ban checks from `loadCodeAssist` to `generateContent`, preserves bans on unknown results, adds probe cooldowns and configurable refresh concurrency/delay, and smooths account updates.

Adapt the mechanism before integration: it hardcodes `gemini-3.8-flash-low`, requests `maxOutputTokens=32000` despite minimal-probe comments, and tests successful JSON by substring matching `"candidates"`. Choose an authorized model from the catalog/configuration, bound probe cost, and parse the response structurally. Keep settings reachable without the GUI.

## Recommended fork integrations

### DNAlec probe claims and recovery

**High value.** [DNAlec PR 3](https://github.com/DNAlec/Antigravity-Manager/pull/3), author `DNAlec`, merged in that fork November 1; final head `af05eaa1d5283beabd110342119bafc280a8401b`.

Local `qos_guardian.rs:727/739` unconditionally applies late probe success/failure. `token_manager.rs:7475` claims expired jails before finding credentials; probe requests use a separate client, and first arbitrary response bytes count as successful TTFT. Late completion can overwrite reset, removal, or newer penalties; missing prerequisites can leave an unowned Probing state.

Accept claim IDs and atomic transitions in `qos_guardian.rs`, matching completions, prerequisite checks, account-proxy/shared-client use, actual non-thinking text validation, and one bounded deadline across acquisition/send/read. Wire the shared client in `server.rs`. Preserve local dynamic model discovery, endpoint fallback/envelopes, sample expiry, and HalfOpen probation. Separate Docker healthchecks from the state-machine change.

### DNAlec versioned session leases

**High value.** [Commit 42c9a34a](https://github.com/DNAlec/Antigravity-Manager/commit/42c9a34ab95d5a50169d91b8e0533314a311da14), November 12, author `DNAlec`.

Local `token_manager.rs:7206` checks a binding and removes it in separate operations; a newer migration can be removed in between. Line 7221 unconditionally commits successful bindings, so an older response can restore account A after a newer request moved to B. String-only identities cannot distinguish A-to-B-to-A generations or account removal.

Accept `proxy/session_bindings.rs`, binding revisions/account epochs, selection snapshots, and atomic claim/commit/abandon. Propagate leases through shared QoS/retry processing and all four handlers. Preserve tenant-scoped identities, retry exclusions, lifecycle safeguards, and terminal policy. Split the commit's optional raw-usage diagnostics from lease correctness.

### DNAlec cooldown and acquisition budgets

**High value when adopting the newer upstream waiting behavior.** [Commit 381736c5](https://github.com/DNAlec/Antigravity-Manager/commit/381736c50cb20f47dd6e16cd1eaf1e786534a4dd) and [commit 47a07f52](https://github.com/DNAlec/Antigravity-Manager/commit/47a07f52808835e68120871a85d36b8991735b47), author `DNAlec`.

Revalidate current cooldowns after sleeping, preserve active probe ownership, and never clear a newer lock merely because an earlier wait ended. Separate intentional cooldown waits from the existing 15-second token-acquisition watchdog through shared `pipeline/token_budget.rs`, keeping the fixed request deadline authoritative. A legitimate 16-second wait otherwise becomes an acquisition timeout.

These are companions for upstream beta.6/.7 integration, not a claim that the reviewed checkout already contains those new waiting-path regressions. Source-reported focused test counts are not independent verification of a rebased combination.

### SanHsien directory identity guards

**Medium value, cross-platform validation needed.** [Commit 40176234](https://github.com/SanHsien/Antigravity-Manager/commit/40176234f4df542df830c9aa3bb624fb9666c98d) and [follow-up dee4b84c](https://github.com/SanHsien/Antigravity-Manager/commit/dee4b84c2727f6a6215053c8fa248fd636948889), author `SanHsien`.

Local `modules/account.rs:2340` turns canonicalization errors into textual fallback, line 2366 starts nesting checks using normalized strings, and line 2402 recursively copies without directory-identity checks. Existing post-creation canonicalization handles ordinary missing paths and symlinks, but Windows drive and accessible UNC aliases can identify the same tree with different strings. A target inside the source through an alias can make copying recurse into its own new destination.

Accept fail-closed resolution except for genuinely missing paths, filesystem identity checks on existing ancestors, source-chain loop detection, and the follow-up's Windows attribute-only handles. Keep the local writer lock, empty-destination requirement, copy-before-pointer-update, atomic pointer write, and source retention. Do not replace the account module with the source's older migration flow. The direct `same-file` dependency already exists transitively, but Windows/macOS/Linux behavior still needs verification on the final implementation.

### Optional configuration and diagnostics

[DNAlec PR 4](https://github.com/DNAlec/Antigravity-Manager/pull/4), merged November 2, adds validated per-instance `proxy.qos`, partial defaults, hot updates, and disk/API/desktop parity. Consider it after probe ownership. Preserve current cold-start behavior and in-flight policy snapshots; expose contextual frontend controls if these become essential knobs.

The diagnostics portion of `42c9a34a` adds bounded per-attempt raw-usage status: absent versus explicit-zero counters, cumulative values, EOF/error/dropped outcomes, and malformed/oversized events. Local raw usage already exists. Treat this as an optional observability extension, preserving numeric-only bounded capture and authoritative billing/wire usage.

`proxy_pool.rs:367` still implements `WeightedRoundRobin` through strict priority selection. [jul-ienfr's candidate](https://github.com/jul-ienfr/Antigravity-Manager/commit/8392da55d39955d88839687771f679a1d1929ae6) identifies a real gap but implements weighted randomness and needs arithmetic review. Define scheduling semantics before adapting only that policy; exclude its unrelated performance bundle.

## Changes to defer or exclude

| Source | File-level decision and path forward |
| --- | --- |
| [Upstream PR 3898](https://github.com/lbjlaq/Antigravity-Manager/pull/3898) | Do not import the 57-file removal bundle. It is the only open upstream PR, explicitly draft/blocked, conflicted, and awaiting long-session/multi-client validation. Request a focused original PR for redundant estimation in `context_manager.rs` or discarded marker allocation in `handlers/openai.rs`; green builds do not resolve behavioral uncertainty. |
| SanHsien signature/tenant bundle | Do not replace `signature_cache.rs`, `http_session_store.rs`, `thinking_store.rs`, monitor backfill, or Responses handling. Local code already has session-scoped tool signatures, exact session lookup, tenant-bound Responses history, trusted stored-parent restoration, and shared credential parsing, plus stronger persistence/retention safeguards. |
| [Issue 3911 EOF fix](https://github.com/lbjlaq/Antigravity-Manager/issues/3911) | Local `stream_lifecycle.rs:353` already flushes a final line without a newline before EOF validation. Review the source's remaining post-loop ordering/regression coverage, without presenting the entire issue as missing. |
| `f4a83aca` Windows build change | Keep local `build.rs`; accept only the distinct model/pipeline fixes. |
| Lyhiving Cursor relay/unary fallback | Separate product/transport work; do not replace shared gateway behavior without an explicit client requirement and protocol review. |
| Older LAZYGASS async scheduler work | Do not simply move snapshots outside serialization. Preserve newest-write ordering with an ordered writer if this performance work is revisited. |
| Broad dependency PRs | Do not equate stale-fork upgrade titles with needed local upgrades. Compare current lockfiles and specific advisories separately. |

Recent histories screened also included Ahmed-Refaat, KiylinG, HotOpenSourcing, KK129, cychenhaibin/gemini2api, ThermalEng, Colin-nwk, DeanCen, schlambos, d3mbun, xb18, modded-soldier-9, vecyang1, and cc1027114472. Much of their recent activity was inherited upstream code, synchronization, workflows, badges, or keepalives. No additional ready integration was established from those histories.

The previous report's quota-depletion/eligibility/retry exclusions, Responses media estimation/logging, endpoint policy, shared stream migration, orphan exclusion, safe single-account deletion, commentary folding, injection-prefix stability, ACP support, headless extraction, and object-union preservation are already integrated. Do not re-import their old patches as fresh fixes.

## Integration order and verification

1. Repair media admission, token admission, and headless host/configuration handling as separate functional units. Bring batch deletion and pedal restoration into durable backend lifecycles.
2. Integrate the small skills pass-through fix and thinking/specification corrections.
3. Adapt DNAlec probe claims and session leases; integrate upstream sticky/QoS context corrections with their cooldown-budget companions.
4. Adopt the final transport/upload policy chain with all matching configuration surfaces.
5. Add directory-identity guards. Review structured 429 handling, prompt rewrite defaults, and account health probes independently before broader adoption.

Source CI is encouraging but bounded evidence. Upstream beta.7 had 20 successful checks across [ordinary CI](https://github.com/lbjlaq/Antigravity-Manager/actions/runs/43068468540), [release](https://github.com/lbjlaq/Antigravity-Manager/actions/runs/43068475908), and [installer](https://github.com/lbjlaq/Antigravity-Manager/actions/runs/43068468592) runs, including Windows/macOS/Ubuntu Rust/Tauri and Docker amd64/arm64. PR 3913's earlier Ubuntu failures were package-mirror errors before compilation. Upstream candidates 3893, 3895, 3899, 3906, 3913, and 3898 had no formal reviews returned by the API. DNAlec's exact latest head `cdec9c3458a214246d8e2fa16566b1c114ad9707` had no check/status contexts; its latest listed successful CI was an older head. [SanHsien's current CI](https://github.com/SanHsien/Antigravity-Manager/actions/runs/43032785230) succeeded. None of these runs validates this fork's eventual adapted combination.

Focused acceptance should cover filesystem media references on Windows/Unix/UNC paths; disabled tokens and competing-IP admission; exact host binding and malformed-config preservation; stale probe/lease completions after reset/delete/migration; renewed cooldowns and waits beyond 15 seconds; SSE keepalives/fragmentation/EOF and delayed persistence/cancellation; upload stalls and replay bounds; batch-delete write failures; pedal recovery without a browser; and aliased directory migration. Preserve configuration parity and all four adapters' shared-pipeline behavior.

No application tests, builds, release pre-flight checks, exploits, or authenticated Google API/inference probes were run for this review. No source changes, commits, merges, remote comments, or releases were made; only the dated report/inventories were added, and upstream refs/objects were refreshed. Repository guidelines require the maintainer's beta-staging decision when nontrivial implementation is requested. Integrate original contributor work with its authorship and source PR/issue references, and keep each functional unit independently revertible.
