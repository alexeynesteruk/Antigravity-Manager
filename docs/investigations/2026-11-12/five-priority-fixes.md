# Five priority fixes implemented

Implemented locally on the previously authorized `main`, preserving the existing uncommitted hardening work. No version bump, release, remote merge/comment, commit or push is part of this task.

| Fix | Result |
| --- | --- |
| Image fan-out | Generations and edits reject invalid/unbounded `n`; shared policy defaults to 10 outputs and one worker. Workers and input-payload clones are created lazily. Abort/drop prevents queued work from starting. Existing account concurrency, replay budgets and stop generations remain enforced. |
| TLS dependencies | Curated lock updates: Rustls 0.23.46, WebPKI 0.103.17, both root-list parents 1.0.10. The 0.26.11 wrapper resolves the new parent. Ring features are unchanged. A fresh OSV query found zero advisory matches for these five resolved TLS/root entries; unrelated dependency inventory findings remain separate. |
| Diagnostic transport | Request/response frame observation retains a bounded diagnostic copy while preserving original bytes, errors, trailers, status and headers. Failed/truncated capture is marked rather than replacing transport. Lazy polling/drop retains request state even on skipped logs and binary responses. |
| Trusted authorities | HTTP, native clients, preflight, WebSocket and static UI routes reject untrusted or contradictory Host/URI authorities. Real local destination, configured listener/HTTPS names and explicit trusted authorities work. Plain HTTP/2 remains supported; accepted connection tasks have bounded shutdown. |
| Migration state | The internal 32k marker survives serialization and old-client settings saves. A later deliberate 32k selection remains unchanged. Repeated reads do not remigrate or rewrite configuration. |

GUI controls are contextual: image limits beside image settings and additional trusted hosts beside LAN/HTTPS settings. Configuration fields/environment equivalents, exact authority semantics and compatibility costs are documented in [REQUEST_HARDENING.md](../../REQUEST_HARDENING.md).

## Verification

- 109 distinct focused Rust tests passed across image policy/scheduling, configuration and native-secret persistence, authority/CORS/auth/IP boundaries, actual TLS/HTTP2/SSE/WebSocket transport, diagnostic body frames/cancellation, request contexts and global stop/budget behavior.
- Ten actual-source frontend settings checks passed, including legacy per-account preservation, malformed limits, stale controls, failed-save recovery and concurrent-edit protection. Six existing automatic-refresh source checks also passed. TypeScript `--noEmit` passed.
- The updated production library and both executable test targets compiled with `cargo test --locked --bins --no-run`. The local headless executable was rebuilt successfully and passed 20 native HTTPS/authority/auth/safety smoke checks with an isolated profile containing zero provider accounts and zero model attempts. Its synthetic native device/config credential entries were cleaned up, and the real gateway remained stopped.
- `git diff --check` passed. No full test suite, release pre-flight, desktop installer or cloud platform packaging was run.

The initial failures were test fixture assumptions: summary monitor reads deliberately omit full bodies, an ephemeral loopback fixture lacked production transport metadata, rejected WebSocket upgrades may keep HTTP alive, and an older request-context fixture hardcoded obsolete replay threshold/attempt limits. The repaired fixtures use persisted detail reads, real transport metadata, HTTP response framing and effective replay policy. Production limits and auth checks were not weakened to satisfy them.

Tests used small diagnostic caps and synthetic accounts/credentials rather than allocating attack-sized bodies or issuing provider calls. Native macOS/Linux runtime behavior and manual browser/GUI interaction remain outside validation on this Windows host. The application source and controls are reviewable locally; installation of a GUI package and restarting the real gateway are separate activation steps.

## Attribution and source boundaries

The missing migration marker was identified in parent [PR #3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928), by `koeria`; this implementation adds storage-owned monotonic persistence for stale clients. The related Unix resource-limit change was not bundled into these five fixes.

TLS update discovery included SanHsien [PR #56](https://github.com/SanHsien/Antigravity-Manager/pull/56) and [PR #10](https://github.com/SanHsien/Antigravity-Manager/pull/10), authored by `dependabot[bot]`, checked against current RustSec guidance. The local lock refresh uses patched current resolutions rather than copying an older fork lockfile. Image, authority and monitor fixes address the independently reviewed current-tree defects.

Detailed test logs, resolved dependency features, OSV results and native smoke metadata are retained in `%TEMP%/antigravity-five-fixes-20261112/`. No real Google account token or account-switch operation was used for validation.
