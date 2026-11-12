# Request hardening

This change implements the five priority fixes from the November 12 review: bounded image output creation, updated TLS verification/root lists, non-destructive diagnostic capture, trusted request authorities, and durable thinking-budget migration state. It preserves the existing account stops, request budgets, native credential storage and LAN access policy.

## Image output limits

Both image generations and multipart edits validate `n` before creating output workers. Missing `n` means one output. A supplied count must be a positive integer within the configured request limit; malformed, zero, fractional and overflowing counts return a validation error. Counts are never silently reduced.

At most the configured number of workers, including completed results awaiting consumption, occupy scheduler slots. Payloads are cloned for active dispatches, rather than once per requested output before scheduling. The final response still contains all admitted outputs, so its size grows with the accepted count. Cancellation aborts active workers and never starts remaining outputs. Existing per-account scheduling, request deadlines, global stops and actual-dispatch budgets still apply.

The contextual **Image request limits** control is beside image settings in **API Proxy → Prompt & Multimodal**. The same public configuration is available headlessly:

```json
{
  "proxy": {
    "image_scheduler": {
      "per_account_concurrency": 1,
      "max_outputs_per_request": 10,
      "max_parallel_outputs": 1
    }
  }
}
```

`max_outputs_per_request` defaults to 10 and accepts 1–100. `max_parallel_outputs` defaults to 1 and accepts 1–16, never exceeding the output limit. These operational ceilings remain enforced for configuration and environment overrides. Existing per-account concurrency settings are preserved; the example's value of one is the user's selected setting, not a new universal default.

`ABV_IMAGE_MAX_OUTPUTS_PER_REQUEST` and `ABV_IMAGE_MAX_PARALLEL_OUTPUTS` override the corresponding saved values. Invalid overrides fail closed. Each request snapshots only public scheduling metadata, without reading native-store secrets. Saved limits apply to new requests without restarting the gateway.

## Diagnostic capture preserves transport

Monitoring now observes HTTP body frames lazily and retains only a bounded diagnostic copy. The original request and response bytes, stream errors, trailers, status, headers and size hints continue to the original consumer. A logging limit never empties a body or turns an oversized response into a successful empty response.

The diagnostic limits remain 100 MiB for requests and responses. Partial, failed or oversized captures produce an explicit omission marker instead of logging an incomplete credential-bearing payload. Request-size admission remains governed independently by the existing ingress limit, including `ABV_MAX_BODY_SIZE`.

Nonstream response logs finalize when the client consumes or drops the body, so their appearance may follow delivery of response headers. Usage and physical-dispatch accounting retain the captured request context during body polling and cancellation. Disabled diagnostic display and skipped health/probe logging do not remove transport context. No background body drainer reads ahead of the client. Historical logs and backups are not erased by this change.

## Trusted request authorities

Every incoming HTTP request, preflight, WebSocket upgrade and static UI request validates its Host/URI authority before Origin handling. Host must agree with URI authority when both are supplied. Malformed, duplicate and untrusted authorities are rejected. Forwarded-Host headers do not grant authority.

The running listener's exact IP destination and port, localhost addresses, explicit bind address and configured HTTPS advertised hostname are accepted automatically. Wildcard binding does not trust arbitrary hostnames or every private address: the live socket destination supplies the actual local address. Listener addresses and ports remain pinned until restart when configuration edits are pending.

The existing LAN configuration therefore continues using `https://192.168.0.168:8443`, with local HTTP on `127.0.0.1:8045`. TLS verification, private-network IP filtering and authentication remain independent requirements. This change does not create a tunnel, modify a router or open firewall ports.

For an intentional reverse proxy or another local hostname, add exact authorities under **Additional trusted hosts**, beside LAN/HTTPS settings, or in configuration:

```json
{
  "proxy": {
    "security_monitor": {
      "trusted_hosts": ["gateway.home:8443", "[::1]:18045"]
    }
  }
}
```

The entries supplement automatic listener authorities. Use the port your client sends in Host. A bare hostname/IP permits only ports 80 and 443; it does not authorize arbitrary ports. Bracket IPv6 addresses when supplying a port. Schemes, paths, userinfo, wildcards and invalid ports are rejected. Existing IP whitelist, blacklist and trusted-proxy settings are preserved.

`ABV_TRUSTED_HOSTS` supplies a comma-separated replacement for saved additional hosts; an empty value removes the extras. Invalid environment values fail startup/request validation closed. This is separate from `ABV_CORS_ALLOWED_ORIGINS`: a cross-origin browser client may need an explicitly allowed Origin too. A reverse proxy should preserve the intended Host, or configure the two policies deliberately. Broad CORS permission never bypasses authority validation.

Native clients continue to send their normal Host header. Plaintext HTTP retains HTTP/1.1 and HTTP/2 support; HTTPS retains its existing HTTP/1.1 ALPN behavior, SSE and WebSocket upgrades. Listener shutdown drains accepted connections for a bounded interval and aborts remaining tasks.

## TLS dependencies

The lockfile updates only the affected compatible package resolutions: Rustls **0.23.46**, Rustls-WebPKI **0.103.17**, WebPKI roots **1.0.10** and WebPKI root certificates **1.0.10**. The root-certs 0.26.11 compatibility wrapper resolves to the updated 1.x parent. Ring remains selected; AWS-LC and Brotli certificate decompression were not enabled.

The root update applies to the primary Rquest/BoringSSL client as well as the relevant Reqwest path. Direct Reqwest 0.12 uses bundled WebPKI roots; the separate Reqwest 0.13 updater path retains platform verification. Incoming HTTPS does not request client certificates. Previously trusted distrusted certificate paths are intentionally rejected. No private LAN certificate/key or account credential is regenerated by updating these dependencies.

Primary guidance: [WebPKI distrust correction](https://rustsec.org/advisories/RUSTSEC-2026-0382.html), [root-certificate correction](https://rustsec.org/advisories/RUSTSEC-2026-0383.html), and [Rustls release](https://github.com/rustls/rustls/releases/tag/v/0.23.46). This targeted update does not claim to resolve unrelated advisory matches in the earlier dependency inventory.

## One-time migration state

The typed configuration retains `thinking_budget_32k_legacy_migrated`. Once the stored marker is true, a GUI/API save cannot clear it through an older snapshot that omitted the internal field. The first applicable migration writes its result; later reads do not repeat the migration or rewrite a deliberate subsequent 32k budget selection. The marker is internal and has no user-facing switch.

The original proposal is parent [PR #3928](https://github.com/lbjlaq/Antigravity-Manager/pull/3928), by `koeria`. This local implementation additionally preserves the storage-owned marker against stale client saves. The PR's separate Unix file-limit change is outside these five fixes. No remote PR was merged or commented on.
