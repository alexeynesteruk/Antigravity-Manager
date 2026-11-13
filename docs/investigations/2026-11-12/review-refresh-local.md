# Fresh local security and reliability review — 2026-11-12

Reviewed the current uncommitted tree over `abbb4da8`, focusing on network/media inputs, admin/auth boundaries, secrets and migration behavior. This report contains three newly identified current defects. Source files were not changed. No application, Cargo tests/builds, provider calls, live configuration or real credentials were accessed. Only source inspection and small static/in-memory proof scripts were used.

## 1. P1: unbounded image counts eagerly allocate tasks and copies before dispatch limits

**Evidence:** [OpenAI generation count](D:/development/Antigravity-Manager/src-tauri/src/proxy/handlers/openai.rs:6519), [generation fan-out](D:/development/Antigravity-Manager/src-tauri/src/proxy/handlers/openai.rs:6592), [payload copy](D:/development/Antigravity-Manager/src-tauri/src/proxy/handlers/openai.rs:6596), [task spawn](D:/development/Antigravity-Manager/src-tauri/src/proxy/handlers/openai.rs:6605), [edit count parsing](D:/development/Antigravity-Manager/src-tauri/src/proxy/handlers/openai.rs:7130), [edit fan-out](D:/development/Antigravity-Manager/src-tauri/src/proxy/handlers/openai.rs:7244).

**Trigger:** With a valid image model and an enabled gateway, submit an authorized `/v1/images/generations` request containing a small prompt and `"n":10000000`; the multipart edits endpoint accepts the same unbounded count. A lower count combined with a large admitted inline image also multiplies memory sharply. These are illustrative payloads only; they were not submitted.

**Consequence:** Both handlers loop over the complete count and eagerly spawn a `JoinSet` task per image, cloning `contents_parts` for each task. The loop does not await a bounded worker slot before allocating the next task. Account/image scheduler permits and outbound budgets are checked inside the spawned work, after these allocations; completed tasks also remain in the set until collected. Thus one admitted request can exhaust memory or occupy a worker allocating millions of futures, even when provider dispatch is tightly limited. No provider response is necessary for the allocation problem. Zero and invalid counts also lack consistent client validation.

**Fix:** Use a shared positive, bounded image-count policy for JSON generations and multipart edits, reject invalid/overflowing counts before payload copying, and produce work lazily with a bounded number of outstanding futures. Share immutable image parts where practical. A semaphore acquired inside already-spawned tasks does not bound queued task memory.

**Verification needed:** Pure count-boundary checks plus a fake-dispatch regression that requests many outputs and measures a fixed maximum number of live workers/payload copies; exercise both endpoints without provider calls.

## 2. P2: diagnostic body limits replace legitimate request/response data with empty bodies

**Evidence:** [100 MiB diagnostic caps](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/monitor.rs:15), [request collection](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/monitor.rs:1040), [empty request fallback](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/monitor.rs:1072), [response collection](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/monitor.rs:1887), [empty response fallback](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/monitor.rs:1986), [configurable transport limit](D:/development/Antigravity-Manager/src-tauri/src/proxy/server.rs:1161), [decoded-body validation](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/request_body.rs:20).

**Trigger:** Set `ABV_MAX_BODY_SIZE` above 100 MiB and send a valid POST between 100 MiB and that configured limit. The decoded-body layer accepts it, then the monitor's independent cap consumes and replaces it with an empty body. Separately, a non-stream JSON response above 100 MiB, such as an aggregated batch of base64 images, reaches the monitor's response cap.

**Consequence:** A request accepted by the configured gateway limit arrives at its handler empty and receives a misleading parse failure. On the response path, the monitor preserves the original status and headers but replaces the body with `Body::empty()`: a successful, potentially billable result becomes an empty HTTP success. Monitoring is not conditional on log persistence being enabled at this point, so disabling the traffic log does not provide a bypass. SSE forwarding already retains its original chunks while bounding its diagnostic copy; the non-stream path lacks that behavior.

**Fix:** Bound the diagnostic copy, not the forwarded business payload. Tee/capture a bounded prefix and retain the original stream and errors. If request size must be rejected, use the declared transport limit and an explicit 413 before handler execution. Never return an empty success response merely because its log copy is too large.

**Verification needed:** Synthetic request/response bodies around both cap boundaries, with a reduced injected test cap to avoid large allocations. Assert that diagnostics truncate while forwarded bytes/status remain intact, including when log persistence is disabled.

## 3. Conditional P1/P2 hardening gap: arbitrary Host and Origin pairs pass the local browser policy

**Evidence:** [origin parsing](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/cors.rs:46), [same-origin Host fallback](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/cors.rs:101), [comparison accepting it](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/cors.rs:105), [loopback Auto resolves to Off](D:/development/Antigravity-Manager/src-tauri/src/proxy/security.rs:50), [Off bypasses authentication](D:/development/Antigravity-Manager/src-tauri/src/proxy/middleware/auth.rs:192), [default loopback configuration](D:/development/Antigravity-Manager/src-tauri/src/proxy/config.rs:1419), [shared origin middleware](D:/development/Antigravity-Manager/src-tauri/src/proxy/server.rs:1176).

**Trigger:** A local HTTP gateway uses the default loopback `Auto` policy, or explicit auth `Off`. In a browser permitting the relevant DNS-rebinding/local-network access, an attacker-controlled hostname is rebound to the gateway's loopback address. The request keeps matching headers such as `Origin: http://rebind.attacker.invalid:8045` and `Host: rebind.attacker.invalid:8045`. The policy compares those two attacker-domain values and accepts them without checking the authority against a configured listener/public hostname.

**Consequence:** A website can potentially issue model requests through a gateway that treats the resulting loopback traffic as unauthenticated local use. Incoming/outbound budgets reduce volume but do not authenticate the requester. This is **not** an unauthenticated compromise of the active LAN profile: its TLS listener and forced authentication protect that profile. HTTPS certificate validation also prevents treating arbitrary attacker DNS names as the configured secure identity. Browser DNS pinning and local-network access restrictions vary; a browser exploit was not executed.

**Static proof:** With an empty configured origin allowlist, the predicate used by the fallback accepts `Origin=http://rebind.attacker.invalid:8045`, `Host=rebind.attacker.invalid:8045`; the actual TCP peer does not participate in that decision. No other Host/authority validation was found in the reviewed middleware.

**Fix:** Restrict accepted request authorities to actual local listener names/addresses and explicit configured public hostnames; preserve intentional reverse-proxy origins through explicit configuration. Require authentication for a request with an unrecognized authority rather than granting auth-off local trust merely because Origin and Host match. Keep the existing Tauri credential requirement and TLS authentication safeguards.

**Verification needed:** A pure middleware/router fixture with a loopback peer, empty origin allowlist and attacker Host/Origin must reject before a model handler runs; retain legitimate localhost, configured LAN/TLS hostname and explicitly permitted reverse-proxy behavior. Full browser testing would separately characterize browser-specific mitigations.

## Scope notes and discarded hypotheses

- Server-local media file reads remain removed; declared media URI validation precedes the relevant conversions. No new filesystem-read bypass was found in this pass.
- Device-key admission requires a digest match and a non-null native reference; failed legacy migration does not restore plaintext authentication. Protected configuration failures propagate on direct headless startup. Native-store portability/availability and historical plaintext backups were already recorded and are not presented as new findings here.
- A suspected missing SQLite foreign-key enablement was discarded: this repository uses bundled `libsqlite3-sys 0.30.1`, whose build enables `SQLITE_DEFAULT_FOREIGN_KEYS=1`. A system-Python SQLite default cannot establish the application's behavior.
- OAuth refresh-lock entries have no removal path, and unregistered no-account imports can retain raw refresh tokens as map keys. This merits bounded-lifecycle cleanup, but import/admin control and the absence of a remote unauthenticated trigger make it lower priority than the three findings above; it is not counted as an additional high-confidence release blocker.
- TLS dependency advisories are being investigated in the separate fork/dependency review; they are not duplicated here.
