# Code, parent, fork and PR review refresh

Reviewed **2026-11-12**, against local `main` commit `abbb4da87c751f2797a5940317f6cb889837e0b4` **plus all existing uncommitted changes**. Three parallel reviewers covered local correctness/security, parent branches/PRs, and selected fork patches; the coordinator refreshed the complete readable metadata inventory, audited dependency metadata and cross-checked the highest-impact code paths.

The strongest next changes are: bound image fan-out before allocating tasks; update TLS verification and bundled roots; prevent monitoring from emptying valid payloads; close the unauthenticated-localhost origin/Host gap; persist the missing migration marker; and adopt deliberate browser-secret retention. Detailed local triggers and proposed regression checks are in [the local reviewer report](review-refresh-local.md).

## Confirmed local findings

### P1 ? Image count can exhaust memory before concurrency or model-attempt limits apply

`src-tauri/src/proxy/handlers/openai.rs:6519` accepts any unsigned generation `n`, without an upper bound. The loop at 6592 clones complete image contents and allocates one Tokio task per output before joining results. Multipart edits parse `n` at 7130 and repeat the eager fan-out at 7244. An authenticated caller can submit a small, otherwise valid image request with an enormous count; inline image payloads amplify each clone. Per-account scheduling and outbound attempt budgets run inside the tasks and therefore cannot bound this earlier allocation.

This is a source-confirmed allocation path, not a claim that an OOM exploit was executed. No test request or provider call was sent. Add a shared positive bounded image-count policy before payload duplication, checked integer conversion, and bounded/lazy work creation. Preserve cancellation, request scopes, per-account concurrency and global attempt accounting. Give operator-configurable limits the same contextual settings/file/headless reachability as existing image scheduling controls. Regression validation should prove invalid counts allocate no workers and a large admitted request never has more than the configured number of active tasks.

### P2 ? Diagnostic limits destructively alter business payloads

`src-tauri/src/proxy/middleware/monitor.rs:1040-1072` consumes POST bodies using a fixed 100 MiB log limit; on any collection error it passes an empty body downstream. A configured `ABV_MAX_BODY_SIZE` greater than 100 MiB can admit a request that monitoring then destroys. The response path at 1883-1986 similarly replaces an oversized nonstream JSON body with an empty body while retaining the original success status/headers. A valid large image result can therefore be charged upstream but delivered empty. Disabled display logging does not bypass this collection path.

Use bounded diagnostic capture alongside faithful body forwarding, rather than making a log cap a transport cap. Preserve usage/request-context capture and redact only the retained diagnostic copy. An actual request-size rejection belongs at ingress with an explicit error. This defect was source-reviewed; large live bodies were not allocated or sent during this review.

### P2 ? Same-origin fallback trusts attacker-chosen Host in default unauthenticated localhost mode

`src-tauri/src/proxy/middleware/cors.rs:99-110` accepts an Origin when it matches the request's Host, without checking that authority against listener-approved hostnames. `auth.rs:192` allows generation when effective auth is Off, which is the default local-only Auto behavior. A DNS-rebinding browser reaching loopback with a matching attacker-controlled Host/Origin is not rejected by this policy. This is distinct from a conventional cross-origin request to a literal localhost URL, which the existing tests reject correctly.

Pin accepted authorities to the actual listener/local addresses and an explicit trusted-host configuration for intentional reverse proxies; keep Origin validation and strict administration auth. Requiring model auth even on loopback is another explicit policy option, with compatibility costs. Modern browser local-network restrictions vary and a browser exploit was not executed. **The user's currently configured LAN HTTPS/auth-required profile does not have the unauthenticated condition.** Native clients without Origin still need the appropriate host/auth policy.

## Integrations worth taking, with adaptation

| Priority | Source | Accept/adapt | Material cost or limitation |
| --- | --- | --- | --- |
| First | SanHsien [PR 56](https://github.com/SanHsien/Antigravity-Manager/pull/56) and [PR 10](https://github.com/SanHsien/Antigravity-Manager/pull/10), current RustSec advisories | Refresh the specific TLS/root dependencies to currently patched compatible versions; do not copy an old fork lockfile. | Older proposed versions are insufficient; intentionally distrusted chains must stop working. Preserve Ring features and check both TLS listeners and outbound clients. |
| High, small | Parent [PR 3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928) | Persist `thinking_budget_32k_legacy_migrated` in the typed configuration and normalization path. | Today the unknown marker is dropped, forcing repeated atomic configuration writes. Unchanged secrets reuse their native reference; this is not unconditional vault-generation churn. |
| High, deliberate UX | SanHsien [PR 15](https://github.com/SanHsien/Antigravity-Manager/pull/15) | Memory-only Web admin key, successful-login publication, request epochs, and protected initialization after authentication. | Reload/new-tab login changes; redesign/remove Remember Me explicitly. Backend authorization remains authoritative. |
| Useful for optional feature | SanHsien [PR 17](https://github.com/SanHsien/Antigravity-Manager/pull/17) | Stop persisting managed APIKEY.FUN keys in browser localStorage; remove the legacy entry after controlled migration. | Session data disappears on reload/exit. The manager's native vault does not currently cover this separate browser store. |
| Useful prompt integrity | Teee3047 [PR 4](https://github.com/Teee3047/Antigravity-Manager/pull/4) | Preserve caller system identity in the current shared sanitizer; retain independent billing, media, thinking and tool guards. | Existing exact SDK identity mapping is separate. Prompt/cache prefixes change; this is not a proven restriction/429 remedy. |
| Medium | Parent [PR 3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928), Unix resource half | Move bounded file-descriptor initialization to a shared path serving Linux/macOS and both executables. | Applying only the parent cfg substitutions misses the standalone server; platform behavior remains unverified. |
| Medium, concept only | Parent [PR 3924](https://github.com/lbjlaq/Antigravity-Manager/pull/3924) | Typed empty-pool diagnostics from trusted indexed raw metadata. | Its submitted directory scan and broad forbidden-means-banned rule conflict with current file boundaries and stronger manual/global stops. |
| Optional | DNAlec [PR 4](https://github.com/DNAlec/Antigravity-Manager/pull/4) | Validated instance QoS policy with state-preserving hot updates and claim-time snapshots. | Nontrivial config integration; retain automatic jailing disabled and do not add unsolicited probes. |

Pinned SHAs, file-by-file accept/drop details, authors and proposed validation are in [the parent report](review-refresh-upstream.md) and [selected-fork report](review-refresh-forks.md). Keep each independent change revertible and retain original contribution attribution when integrating.

## Dependency hardening evidence

The locked graph contains `rustls 0.23.36`, `rustls-webpki 0.103.9`, `webpki-roots 1.0.6`, and `webpki-root-certs 1.0.6`. Current published guidance requires at least WebPKI **0.103.16** on its stable line and both root-list families **1.0.10** for the emSign distrust corrections. Rustls **0.23.46** includes additional fixes. See [RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html), [roots advisory](https://rustsec.org/advisories/RUSTSEC-2026-0381.html), and [root-certs advisory](https://rustsec.org/advisories/RUSTSEC-2026-0383.html).

This is relevant to the primary outbound transport as well: Rquest uses BoringSSL verification but gets roots through `webpki-root-certs 0.26.11 -> 1.0.6`. The compatibility wrapper does not fix the old resolved parent. Direct Reqwest 0.12 with `rustls-tls` uses WebPKI/bundled roots; the separate Reqwest 0.13 updater-plugin path uses platform verification. Incoming HTTPS does not request client certificates, so this is not an incoming client-authentication-bypass claim. No malicious-certificate experiment was performed.

Two fresh read-only scans were performed:

- `npm audit --json --package-lock-only --ignore-scripts` reports **32 package findings: 19 high, 7 moderate, 6 low**, including build/docs dependencies. Twenty affected package entries have production dependency nodes, but that does not prove their vulnerable functions enter the shipped bundle. The current Vite 7.3.6 and the separate Vitepress/Vite 5 dependency branch need deliberate updates/removal, not blind `npm audit fix --force`.
- OSV batch matching checked **685 registry packages** from Cargo.lock and matched **36 packages** to advisory records. Matches include informational/unmaintained notices, OS-specific paths and disabled optional features, plus overlapping aliases. These are **not 36 confirmed remotely exploitable app bugs**. Runtime Rustls enables Ring/std/TLS1.2, not Brotli certificate decompression; Quinn is not in the inspected Windows normal-dependency tree. Conversely, root lists, WebPKI, H2, bundled SQLite and Zstd are in that tree and warrant targeted review. For example, the SQLite advisory does not prove caller-controlled SQL reaches its vulnerable path.

`cargo audit` is not installed, so the Rust scan used the primary OSV API rather than installing a tool. Windows dependency reachability was checked with `cargo tree --offline --locked --edges normal`; the Linux offline tree could not resolve an uncached `async-broadcast` package. No cross-platform runtime result is claimed. Raw scan data and exact dependency versions are retained in [refresh/](refresh/).

There is also avoidable dependency surface: no direct source imports of `@lobehub/ui`, Fluent Emoji or Vitepress were found, while the icon package has UI/Ant Design peer requirements inconsistent with current direct versions. Confirm the icon integration's peer needs before removing dependencies. A focused dependency cleanup can reduce install time and advisory noise without changing the app's feature scope.

For CI, add a small scheduled/manual lockfile security audit and explicit exception tracking, and pin privileged third-party actions to reviewed immutable SHAs. This does not require building native platform applications in the cloud. Keep routine Linux validation and opt-in packaging behavior from the prior work. Release metadata resumability, atomic version bumping and fork updater trust/channel selection remain separate documented follow-ups; none justifies a bulk upstream merge.

## Do not import these submissions as they stand

Parent [PR 3923](https://github.com/lbjlaq/Antigravity-Manager/pull/3923) replaces OAuth/module source with literal `@src-tauri/...` placeholder lines. [PR 3927](https://github.com/lbjlaq/Antigravity-Manager/pull/3927) replaces the model catalog and four source files with placeholders. Pinned raw blobs independently confirm the destructive contents. Their descriptions are not working implementations, and no public checks/reviews were observed at those heads. Request corrected original source before reconsidering; no malicious intent is inferred.

Skills pass-through in [3925](https://github.com/lbjlaq/Antigravity-Manager/pull/3925), EOF-tail flushing in [3926](https://github.com/lbjlaq/Antigravity-Manager/pull/3926), earlier sticky/QoS/transport work, directory identity guards and warmup redirect restrictions are already covered locally. Reapplying old patches could lose current credential and account-stop protections. Avoid bulk merging parent beta or old whole-module fork replacements.

## Coverage and limits

| Item | Fresh result |
| --- | --- |
| Listed public forks, fully paginated | 3,576 |
| Readable fork repositories | 3,475 |
| Unavailable fork repositories | 101 explicit NOT_FOUND |
| Distinct readable default heads | 451 |
| Fork repositories with PRs, fully paginated | 80 |
| Parent PRs | 590; 8 open |
| Fork PRs | 1,263; 62 open |
| Origin PRs | 0 |
| Total PR metadata records | 1,853; zero enumeration errors |
| Parent main / beta | Unchanged: `9b82fb7e` / `3f759ae9` |
| New/changed default heads vs previous same-day snapshot | 10; mostly synchronization, CI or keepalive |

The connected GitHub fetch tool rejected the fork-list endpoint; authenticated GitHub CLI REST supplied all pages. Batched GraphQL supplied default heads/availability/PR counts and complete PR pagination. Selected patches were inspected through connected GitHub tools and pinned source. Inventories, deltas, hashes and limits are in [refresh/summary.json](refresh/summary.json), [forks.csv](refresh/forks.csv), and [pull-requests.csv](refresh/pull-requests.csv).

Complete metadata enumeration is not exhaustive source auditing: private/unavailable forks, all nondefault fork branches and every unique historical patch were not reviewed. Dependency matching and static control-flow confirmation are separate from executed exploit reproduction. No app tests/builds, source fixes, live configuration changes, user credential access, authenticated Google requests, commits, pushes, merges or remote comments occurred during this review. Reports and public metadata artifacts are the only new repository files.
