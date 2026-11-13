# Account hardening follow-up

This follow-up implements the requested credential and account safety changes on the previously authorized local `main` branch. It preserves the earlier uncommitted review fixes. It does not publish a release or install a replacement application.

## Local settings applied

The existing user configuration now has:

| Setting | Value |
| --- | --- |
| Scheduled warmup | Disabled |
| Default concurrent requests per account | 1; image concurrency also 1; the ten configured accounts had no individual overrides |
| Listener | HTTPS `0.0.0.0:8443`; HTTP `127.0.0.1:8045`; configured, real gateway stopped |
| Private-network filter | Enabled for loopback, RFC1918, link-local and IPv6 private ranges; forwarding headers untrusted |
| Request limits | 30 requests/minute per device; 60 outbound model attempts/minute; 2,000 outbound model attempts/UTC day |
| Global stop | Confirmed account restrictions and authentication challenges require explicit acknowledgement |
| Gateway authentication | `all_except_health` |
| Debug payload logging | Disabled; metadata-only selected for later enablement |
| Scheduling | Existing Cache First, client-credential affinity |
| Quota protection | Existing 4% threshold, which triggers below 4% of reported model-group quota |
| Quota refresh | Existing enabled, one-minute frontend interval |

The user clarified that remote devices enter the LAN through their router. There is no new public tunnel. The machine reports `192.168.0.168` as a LAN address; the updated service must be started before `https://192.168.0.168:8443` is reachable. Plain HTTP remains loopback-only. Client devices must trust the private public CA certificate, and the Windows firewall must permit this LAN traffic. A client uses the gateway's local credential, separate from any Google subscription or paid API key.

The settings rollback copy is protected with Windows current-user DPAPI. The earlier plaintext settings backup was encrypted and its decryption verified before removing the plaintext original. The subsequent offline maintenance run migrated all ten managed account files, gateway configuration and the one existing device credential to Windows Credential Manager. In-memory comparison against the verified encrypted rollback archive confirmed the account token values and gateway client key were preserved. Official-client credential stores were not modified.

## Request and account safety

`proxy/pipeline/account_safety.rs` classifies provider authentication challenges and confirmed account restrictions across all four gateway protocols. HTTP failures and bounded SSE error inspection feed the same policy. Candidate text and tool output do not constitute provider restriction evidence.

A confirmed challenge/restriction stops that request with an actionable `account_attention_required` error and persists a manual recovery hold. It does not continue that request across the account pool. Existing account status and verification controls expose the hold. Numeric provider status codes and model-specific permission failures remain distinguishable; ordinary 429 responses do not become permanent bans.

An ordinary provider 401 permits one same-account authentication refresh/replay, bounded by both the refresh window and the remaining request budget. Refresh rejection stops the request. Rotated refresh tokens are retained and protected before updated credentials become available in memory. Warmup credential reuse follows the same absolute-expiry convention as the main token pool.

Pending holds deny selection immediately, including when disk persistence fails. Generation checks prevent stale disk reads and reload publication from reopening an account. Ordinary account persistence preserves a newer manual hold and its restriction evidence; only the explicit verified recovery paths can clear it. These holds do not expire after 24 hours or clear through an ordinary quota refresh.

Supplied warmup credentials cannot bypass automatic account eligibility. An explicit user availability probe retains its existing recovery purpose. The project-header 403 fallback also returns a proven challenge/restriction to the account safety policy rather than replaying it without the header.

SSE inspection is bounded to 64 KiB per line. Oversized error structures recognized within that prefix stop without inventing ban evidence. An error field placed beyond an oversized preceding metadata value remains outside that bounded inspection window. Requests already sent by another concurrent operation cannot be recalled.

## Credential persistence

Manager account access/refresh/identity tokens and configuration API/admin/tunnel/proxy credentials move into a dedicated OS credential namespace. JSON stores versioned immutable references and operational settings. Proxy credentials follow stable proxy IDs; editing or reordering public pool settings does not swap credentials between entries.

The native backends are Windows Credential Manager, macOS Keychain, and Linux Secret Service. Secret bytes are not supplied in subprocess arguments. Linux helpers use bounded stdin/stdout communication, process cleanup, and explicit errors. Missing, locked or unavailable stores do not cause a plaintext fallback. Immutable generation reads have a bounded cache to avoid a Secret Service subprocess for every configuration read.

The updated application promotes indexed legacy accounts before listing/loading them and promotes legacy configuration on load. New native entries are written and verified before atomic metadata replacement. Failed publication preserves the previous usable generation. Account holds can still update metadata while the credential store is unavailable.

Metadata-only writes adopt the latest protected credential reference and expiry/client metadata under the account file lock. The subsequent hardening wave also adds typed credential provenance and revision checks: an intentional stale credential update fails, an unchanged metadata snapshot adopts current credentials, and refresh publication retains its per-account single-flight lock through durable storage. Credential revision changes detect a value changing away and back again. Read hydration holds the account file lock so concurrent rotation cannot remove its generation during the read. Manual hold and restriction evidence remain protected.

Antigravity CLI and IDE credential writes use native named-store calls with readback verification and no automatic plaintext home-file mirror. Existing legacy files can still be read for import/compatibility. Explicit credential exports remain explicit exports.

Operational boundaries:

- An updated executable must run before managed live account/configuration migration occurs. Older executables cannot interpret the new references.
- Copying protected JSON alone does not transfer credentials to another OS user or machine. Use the existing explicit export/import workflow for an intentional transfer and protect the export.
- Linux, including headless/container use, requires `secret-tool`, a reachable user D-Bus session and an unlocked Secret Service provider. Existing bare containers need that service configured; the application does not create a weak replacement keyring. Supply a known gateway key through the existing `ABV_API_KEY` environment option when provisioning a headless instance. [Secret Service headless operation](https://github.com/git-ecosystem/git-credential-manager/blob/main/docs/secretservice-headless.md)
- Official clients can maintain their own required files or databases. IDE SQLite state/backups and Gemini/classic/ACP compatibility formats are separate from the manager vault. The optional legacy ACP macOS mirror still needs a separate native conversion; legacy permissive Keychain ACLs are not automatically tightened by updating a value.
- OS credential storage protects persistence; it does not isolate secrets from other processes running as the same OS user.

## Diagnostic privacy and internal requests

Shared diagnostic redaction runs before console/error-file output, Tauri log events, monitor buffering, SQLite writes, and debug payload writes. Monitor detail/export reads also redact older records. It covers credential fields, camelCase/escaped names, wrapped Debug values, authorization/cookies, query credentials and proxy userinfo, including supported scheme-less proxy authorities. Session identifiers, usage counts and operational numeric error codes remain available. Wire request and response payloads are not altered by diagnostic redaction.

Historical log files, old database pages and externally created backups are not erased by these hooks. Full request logging can still contain user content.

All three internal warmup HTTP callers use one no-proxy/no-redirect client factory that fails if construction fails. Existing numeric-loopback admission and internal request credentials remain in force. A 307/308 cannot cause the client to replay OAuth JSON or an internal credential to another destination.

## Fork/PR evidence

The selected source review and accept/drop details are in [account-protection-forks.md](account-protection-forks.md). The useful findings were the internal-client restriction in [SanHsien PR 41](https://github.com/SanHsien/Antigravity-Manager/pull/41), private configuration writes in [larry109 PR 1](https://github.com/larry109/Antigravity-Manager/pull/1), and credential audit redaction in [LAZYGASS PR 5](https://github.com/LAZYGASS/Antigravity-Manager/pull/5). Local changes address those problem classes through current shared boundaries; no remote PR was merged or commented on during this follow-up.

The predictable legacy machine-ID encryption fallback in [SanHsien PR 19](https://github.com/SanHsien/Antigravity-Manager/pull/19) remains a separate compatibility/migration issue. The native manager vault does not use that fallback as its protection key.

## Validation

The earlier hardening wave established the following baseline:

- **191 distinct focused Rust tests**, including an actual Windows Credential Manager smoke check with a new synthetic entry that was written, read and deleted. The remaining storage fixtures use an isolated mock store; no real account credentials were migrated or used.
- Shared account-safety tests cover real guard → Claude/Chat/Responses mapper sequences, partial content, thinking-only output, nonstream collectors, split errors, bounded large frames and the Responses lifecycle prelude.
- Existing account, model-lock, OAuth, deletion, IP-filter, stream-peek and lifecycle regression tests pass alongside the new vault, configuration, redaction, redirect and warmup admission tests.
- Desktop and headless binary test targets compiled with the ordinary production library (`cargo test --bins --no-run`), checking native code excluded from the mock-store unit configuration. This is compilation, not a packaged release or application launch.
- **25 CI helper tests** and TypeScript `--noEmit` passed. `git diff --check` passed.

The combined logs and machine-readable summary are under `%TEMP%/antigravity-account-hardening-validation-20261112/`. Initial test failures led to the retained-orphan cleanup ordering fix and the corrected Responses/partial-stream terminal handling; subsequent focused checks passed. The OpenAI collector has no separate pre-existing unit module; its attention behavior is exercised by the shared real mapper/collector fixtures.

No authenticated Google traffic or real-account switching was used. Native Linux/macOS runtime behavior and official-client account recognition remain outside validation on this Windows host. The local changes are uncommitted; a local headless executable was built for offline maintenance and isolated validation. No desktop installer, remote publication or live gateway startup was performed.

These changes harden a direct OAuth gateway. They do not replace its upstream transport with `agy`, provide provider approval for account pooling, or guarantee that provider restrictions will never occur.

## Subsequent hardening wave and local activation boundary

The completed follow-up adds typed credential revision checks, a durable global emergency stop, persistent request budgets, native LAN HTTPS and independently revocable device keys. See [ACCOUNT_SAFETY.md](../../ACCOUNT_SAFETY.md) and [HTTPS_LAN.md](../../HTTPS_LAN.md) for configuration, administrator endpoints and recovery behavior.

Automatic frontend quota reads use dedicated automatic entrypoints; manual recovery controls remain usable during a stop. Image workers retain their original stop generation during token acquisition and model resolution. The device-key database stores digests and native references, while lists and statistics return previews; administrator-only explicit reveal is noncacheable. Missing native references never make stored hashes usable as bearer credentials. Clipboard copying starts during the user gesture and clears temporary DOM values on every fallback exit.

Latest verification passed **167 distinct focused Rust tests**, **25 CI helper tests**, **37 actual-source frontend behavior checks**, TypeScript `--noEmit`, compilation of both production binary test targets, and a normal local headless build. The built executable passed **16 native HTTPS smoke checks** using an isolated profile with no provider accounts: verified CA/SAN handling, unknown CA/wrong-host rejection, loopback HTTP, required management authentication, native device-key create/list/reveal/revoke, device/admin separation, emergency stop, automatic sweep rejection and acknowledged resume. It dispatched zero model attempts. The temporary test device credential and its config-store generation were removed; redacted logs and machine-readable results remain in `%TEMP%/antigravity-account-hardening-validation-20261112/`.

The production profile now points to a newly generated private LAN identity valid through **2027-11-12**. The leaf key was verified byte-for-byte in Windows Credential Manager before removing its temporary PEM. The private CA key has a verified current-user DPAPI backup; only public certificates are distributed. No test fixture certificate is used. The local root-trust operation required an interactive Windows security confirmation and was canceled after a bounded attempt. **The public CA is not installed as a trusted root yet.** No machine-wide trust change occurred.

Remaining operator actions on this machine/device setup:

1. Trust the public certificate at `%USERPROFILE%/.antigravity_tools/tls/lan-ca.cer` on each connecting device. The local helper `tls/trust-lan-ca.ps1` targets only the current Windows user's root store and requires the Windows trust confirmation. Do not distribute a private key or disable verification.
2. If Windows Firewall blocks access, run `%USERPROFILE%/.antigravity_tools/tls/allow-lan-https.ps1` as administrator. It permits only the built server executable, TCP 8443, this PC's current LAN address, and private IPv4 source ranges. The current session cannot administer firewall rules. Nothing was forwarded through the router.
3. Start the updated `src-tauri/target/debug/antigravity-server.exe` (use `--no-web` for pure proxy operation). Older manager builds cannot hydrate the newly protected references. Keep the encrypted rollback archive and a compatible build together. Update the certificate and firewall address if DHCP changes the LAN IP.

Google passkeys, two-step verification, recovery settings, and review/removal of unused third-party grants still require the owner's Google sign-in and explicit choices. No Google account-security settings were changed. The gateway continues to use its direct OAuth transport; it does not run inference through the installed `agy` CLI or promise protection against provider enforcement.
