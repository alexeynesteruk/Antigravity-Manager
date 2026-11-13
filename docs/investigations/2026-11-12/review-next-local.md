# Next local hardening review — 2026-11-12

Read-only review of the latest uncommitted tree after the previous five fixes. The bounded image workers, faithful monitor forwarding, Host guard, migration marker and TLS dependency updates are not reported as open findings. No application, build, test, live configuration, credential-store operation or provider request was run. Findings below are source-confirmed control flows; interleavings and OS error conditions were not executed.

## 1. Conditional P1/P2: profile ownership does not cover CLI/headless writers

**Sources:** [process-local account locks](D:/development/Antigravity-Manager/src-tauri/src/modules/account.rs:19), [account writer wrapper](D:/development/Antigravity-Manager/src-tauri/src/modules/account.rs:30), [process-local OAuth refresh locks](D:/development/Antigravity-Manager/src-tauri/src/modules/oauth.rs:17), [refresh lock acquisition](D:/development/Antigravity-Manager/src-tauri/src/modules/oauth.rs:801), [credential revision check](D:/development/Antigravity-Manager/src-tauri/src/modules/credential_store/mod.rs:646), [latest-file read](D:/development/Antigravity-Manager/src-tauri/src/modules/credential_store/mod.rs:764), [atomic publication](D:/development/Antigravity-Manager/src-tauri/src/modules/credential_store/mod.rs:916), [CLI before GUI single-instance registration](D:/development/Antigravity-Manager/src-tauri/src/lib.rs:541), [GUI plugin registration](D:/development/Antigravity-Manager/src-tauri/src/lib.rs:607), [standalone entrypoint](D:/development/Antigravity-Manager/src-tauri/src/bin/server.rs:66).

**Trigger:** Run a credential-mutating CLI command alongside the GUI/headless process using the same data directory, or run two headless instances on different ports. The GUI single-instance plugin does not protect these entrypoints. Both per-account mutexes and OAuth single-flight maps are independent process memory; no interprocess account/profile writer lock was found.

**Credential impact:** The new CAS correctly prevents stale writes within one process, but its revision check and file replacement are not an atomic cross-process operation. A permitted interleaving is: both processes read revision R0; both pass their checks; process A publishes R1; process B publishes a different result derived from R0. Atomic rename keeps each JSON document intact but does not reject B's stale publication. The independent OAuth locks also permit simultaneous use of a rotating refresh token. Depending on provider rotation and publication order, the surviving account may retain an unusable refresh token. No provider rotation was simulated or claimed observed.

**Related global-stop inconsistency:** [halt raises a local latch](D:/development/Antigravity-Manager/src-tauri/src/proxy/pipeline/admission.rs:275); [resume clears only its own process latch](D:/development/Antigravity-Manager/src-tauri/src/proxy/pipeline/admission.rs:324); [check returns before consulting SQLite](D:/development/Antigravity-Manager/src-tauri/src/proxy/pipeline/admission.rs:329); [outbound charging does likewise](D:/development/Antigravity-Manager/src-tauri/src/proxy/pipeline/admission.rs:380). If process A halts and process B explicitly resumes the shared durable generation, A continues rejecting dispatch because its memory latch remains true. [Status also ORs that latch with durable state](D:/development/Antigravity-Manager/src-tauri/src/proxy/pipeline/admission.rs:262). The acknowledged stop therefore is not consistently resumed across processes.

**Fix:** Enforce one owner for each canonical data directory across GUI, headless and mutating CLI entrypoints using an OS-backed profile lock; route concurrent commands to that owner or reject them clearly. If multiple writers are intentionally supported, hold interprocess locks across refresh/read/compare/publish/retirement, and reconcile a persisted stop with a newer durable acknowledgement. Preserve the separate fail-closed latch for a stop that failed to persist; simply clearing every memory latch when a database read says `halted=false` would reintroduce that failure race.

**Verification needed:** Two isolated helper processes synchronized at the revision-check/publication boundary, using synthetic storage/transport only. Separately exercise halt in process A and acknowledgement in B, including a failed durable-halt write. The offline migration command's documented closed-app requirement remains necessary, but documentation does not serialize other mutating CLI commands.

## 2. P2: rejected configuration saves can still change live account-safety policy

**Sources:** [runtime policy publication](D:/development/Antigravity-Manager/src-tauri/src/proxy/server.rs:2120), [subsequent durable save](D:/development/Antigravity-Manager/src-tauri/src/proxy/server.rs:2122), [validation before persistence](D:/development/Antigravity-Manager/src-tauri/src/modules/config.rs:333).

**Trigger:** An administrator saves changed account-safety limits or stop-observation flags together with an invalid setting, for example `image_scheduler.max_outputs_per_request=0`, or the subsequent config/native-store write fails.

**Impact:** `admin_save_config` configures the live `SafetyController` before validating/persisting the submitted configuration. The request then returns an error while the controller has already accepted the rejected policy. For example, local attempt limits or future authentication-challenge stop observation can be disabled despite the save failing. A frontend reload reads the older saved configuration, not this live controller state. An eventual independent reconfiguration may correct it, but it is not rolled back by the failing save path.

**Fix:** Validate and durably save before publishing the new runtime safety policy, alongside the other existing hot updates. A failed save must leave the previous live policy intact.

**Verification needed:** An authenticated synthetic configuration route test using an invalid image limit and changed safety flags, then a persistence-failure fixture. Assert both file and controller policy remain unchanged after rejection.

## 3. Conditional P1/P2: unrelated stale config saves can restore a revoked gateway key

**Sources:** [full hydrated config response](D:/development/Antigravity-Manager/src-tauri/src/proxy/server.rs:2099), [whole-config save contract](D:/development/Antigravity-Manager/src-tauri/src/proxy/server.rs:2115), [whole-snapshot persistence](D:/development/Antigravity-Manager/src-tauri/src/modules/config.rs:333), [secret extraction from the submitted snapshot](D:/development/Antigravity-Manager/src-tauri/src/modules/config_secrets.rs:153), [latest reference used for reuse comparison](D:/development/Antigravity-Manager/src-tauri/src/modules/config_secrets.rs:195), [new secret-generation publication](D:/development/Antigravity-Manager/src-tauri/src/modules/config_secrets.rs:212), [live authentication update](D:/development/Antigravity-Manager/src-tauri/src/proxy/server.rs:2163).

**Trigger:** Two admin clients/tabs load configuration C0 containing master key K0. Client A deliberately rotates it to K1. Client B still has C0 and saves an unrelated setting. With an independently configured administrator password, B remains authorized; alternatively both saves can already have authenticated before rotation and reach persistence in the opposite order.

**Impact:** Configuration writes have a process mutex, but no observed configuration/secret revision or credential-change intent. B's serialized snapshot therefore supplies K0 as the new desired secret. Native storage creates/publishes a generation containing K0, then the live security configuration accepts K0 again. The revoked key becomes usable without B intentionally rotating it back. The recent latest-store frontend merge protects edits within one store instance, not independent browser tabs/clients. The account-level credential CAS does not cover config credentials; the monotonic thinking-migration marker protects only its own flag.

**Fix:** Carry an observed config/secret revision through read and write and reject stale replacements, or use explicit secret-change operations while ordinary configuration patches preserve current secrets. Keep rotation and unrelated metadata updates distinct at the persistence boundary; storing the stale value in a new protected generation does not make it current.

**Verification needed:** Synthetic native-store config fixture: read C0 twice, rotate K0→K1, save unrelated metadata from C0, and prove K1 remains authoritative or the stale save fails explicitly. Cover the administrator-password and already-authenticated concurrent-request cases without real client keys.

## 4. P2: account deletion can lose the native cleanup reference after a read failure

**Sources:** [deletion helper](D:/development/Antigravity-Manager/src-tauri/src/modules/account.rs:3817), [reference-read error converted to no reference](D:/development/Antigravity-Manager/src-tauri/src/modules/account.rs:3826), [unconditional file unlink afterward](D:/development/Antigravity-Manager/src-tauri/src/modules/account.rs:3835).

**Trigger:** The managed JSON file is unreadable or unparsable while its directory still permits deletion. A concrete Windows condition is an open handle allowing `FILE_SHARE_DELETE` but not `FILE_SHARE_READ`; on Unix an unreadable file in a writable directory can have the same read-fails/unlink-succeeds behavior. The account was already stored with a valid native credential generation before the read restriction.

**Impact:** `account_reference_at` returns an error; the helper logs a warning and substitutes `None`, then deletes the JSON file and returns success. Native credential deletion is skipped, and the active account metadata manifest/reference is lost. Encrypted rollback backups may retain that reference; those backups were not inspected in this pass. The user receives no partial-cleanup error/reference even though the OS store retains the OAuth payload. This is different from the already-fixed unlink ordering: failed unlink correctly retains the generation, and an actual native-delete failure correctly reports its reference. The problematic branch is failure to obtain the reference before unlinking.

**Fix:** Return a partial-cleanup error before unlinking when reference recovery fails. The already-retired index entry and pool invalidation can remain committed, while the file survives for repair/review and a later safe retry. An explicit force-cleanup operation, if needed, should disclose the unknown native orphan rather than silently treating it as complete deletion.

**Verification needed:** Synthetic protected account plus a Windows read-denying/delete-sharing handle, or an injected reference-read error. Assert the index is retired, the JSON/reference remains recoverable, and native cleanup can complete on retry. No user credential deletion was attempted in this review.

## Cross-review and exclusions

- Independently confirmed the coordinator's separate WebSocket finding: later turns reuse cached identity/headers and call the chat handler directly after only local request-budget admission. Current device/master policy and logical service status are not reauthorized per turn. TLS protects transport but does not revalidate a revoked credential cached by an already-open socket. That finding is not duplicated above.
- Native device-key migration/admission remains fail-closed when a protected reference is absent; no plaintext/hash bearer fallback was found.
- Historical/legacy archive backup protection remains a previously recorded boundary, not a new finding here.
- These are conditional concurrency/failure-path findings, not claims that the current single running profile is already compromised or that a provider restriction was observed.
