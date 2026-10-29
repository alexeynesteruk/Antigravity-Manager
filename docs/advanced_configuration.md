# Advanced Configuration and Experimental Features

## Remote database imports

The Web/headless endpoint `POST /api/accounts/import/db-custom` requires the
`ANTIGRAVITY_DB_IMPORT_DIR` environment variable. Set it to a directory containing
database files prepared by the server administrator, and send a filename:

```json
{"path":"state.vscdb"}
```

The endpoint returns 403 when the directory is not configured, and 400 for an
absolute path, a directory, a symbolic link, or a filename outside that directory.
Imports open SQLite databases read-only. Keep the import directory writable only
by trusted local operators. Native desktop file selection continues to accept a
full path through the desktop command.

## Browser clients and reverse proxies

The Security settings page and `proxy.security_monitor` support these fields:

```json
{
  "trusted_proxies": ["127.0.0.1/32", "::1/128"],
  "cors_allowed_origins": ["https://client.example.com"]
}
```

Both lists default to empty. Forwarded client IPs are accepted only from configured
TCP peers, resolving the trusted chain from right to left. The same verified IP
controls access lists, user-token IP limits, and access logs. List only proxies
you operate; an empty list uses the TCP peer directly.

Browser origins are exact scheme/host/port values, without paths. The hosted UI
works on its own origin. The desktop origin requires the configured API key.
Other browser clients need an explicit entry; native clients may omit `Origin`.
The same policy applies to Responses WebSocket handshakes. A literal `*` is an
explicit opt-in to unrestricted browser origins.

`ABV_TRUSTED_PROXIES` and `ABV_CORS_ALLOWED_ORIGINS` override the lists with
comma-separated values; setting either variable to an empty string clears its
list. Changes apply to subsequent requests without restarting the proxy.

OAuth callbacks require a matching, unexpired pending authorization flow. State
is reserved once and tied to its redirect URI and OAuth client before exchanging
an authorization code. Manual completion uses the same flow lifecycle.

## Account concurrency and imports

`proxy.account_max_concurrency` sets the default simultaneous requests per
account. It defaults to `0` (unlimited). An account's `max_concurrency` can override
it: `null` inherits the pool default and `0` explicitly allows unlimited requests.
The API Proxy scheduling settings and account details expose the same controls.
Streaming requests retain their slot until the stream finishes or is dropped.
When no account can accept work, the gateway returns a bounded busy response.

Account imports and quota refreshes use bounded concurrency. Legacy migrations
validate account IDs and destinations, write the backup and account files, and
replace the index only after those writes succeed. Account snapshots containing
OAuth credentials are not stored in browser local storage.

## Raw usage and diagnostics

Missing usage fields remain unknown; explicit zero remains zero. Statistics keep
upstream-reported totals separately from input/output breakdowns. Cache hit rates
use only requests with reported cache fields, and the dashboard displays coverage.
Older records without that provenance are shown as unknown rather than relabeled
as newly measured data. Model pricing remains a nominal estimate.

Set `proxy.debug_logging.enabled` to enable diagnostics. The additional
`proxy.debug_logging.metadata_only` flag defaults to `false`, preserving existing
payload-capture behavior. Enable it to record dispatch identity, endpoint origin,
status and raw usage without capturing prompt/tool bodies. The Settings page
exposes the same mode. Event parsing and queued writes are bounded; oversized
diagnostic events do not block the response stream. Existing log files are not
rewritten when this mode changes.

## Named Cloudflare tunnels

For an authenticated named tunnel, `cloudflared.custom_domain` can specify a
public URL such as `https://api.example.com`. `ABV_CLOUDFLARED_DOMAIN` overrides it.
The URL takes precedence over hostnames discovered in cloudflared logs; leaving
it unset retains automatic discovery. Only valid HTTP(S) origins are accepted.

Starting a public tunnel requires a nonempty proxy API key and authentication.
Authentication remains required while an owned tunnel process is active, even if
the stored local authentication preference is changed. Stopping the tunnel
restores that preference. Tunnel management affects only its own child process.

## Compressed AI requests

AI protocol endpoints accept `Content-Encoding: gzip`. Authentication runs before
decompression. `ABV_MAX_BODY_SIZE` limits the decoded request size in bytes
(default `104857600`, or 100 MiB); malformed gzip returns 400 and an oversized
decoded body returns 413 before request monitoring or model dispatch.

## Startup client fingerprint

Desktop and headless startup use the configured `proxy.hub_version` and
`proxy.hub_cl`, or the local/built-in fallback, immediately. Discovery runs in the
background with a bounded network timeout. It preserves settings saved while
discovery is running and retains the verified version/build pair when the updater
reports a new version without a matching build number. Explicit configuration
overrides remain authoritative.

Antigravity v3.3.35 introduces `ExperimentalConfig`, a set of experimental feature switches enabled by default, aimed at improving the system's robustness and compatibility. These settings live in `src-tauri/src/proxy/config.rs` and are not yet exposed in the UI.

## Feature List

### 1. Two-Tier Signature Cache
*   **Config key**: `enable_signature_cache`
*   **Default**: `true`
*   **Description**: when enabled, the system caches the mapping between `ToolUse ID` and `Thought Signature`.
*   **Purpose**: fixes an issue where some clients (e.g. Claude Desktop CLI, Cherry Studio) can lose the historical Tool Call signature across multiple conversation turns. When the upstream API returns "Missing signature", the system can automatically restore it from the cache, avoiding a broken conversation.

### 2. Tool Loop Auto-Recovery
*   **Config key**: `enable_tool_loop_recovery`
*   **Default**: `true`
*   **Description**: when enabled, the system monitors conversation state in real time to detect "infinite loop" patterns.
*   **Trigger condition**: a consecutive `ToolUse` -> `ToolResult` loop is detected, and the `Assistant` message is missing a `Thinking` block (usually because it was stripped after signature validation failed).
*   **Behavior**: automatically injects a synthetic message pair (`Assistant: Tool execution completed.` -> `User: Proceed.`) to break the loop and force the model into its next round of thinking.

### 3. Cross-Model Compatibility Checks
*   **Config key**: `enable_cross_model_checks`
*   **Default**: `true`
*   **Description**: prevents signature errors caused by switching between different model families (e.g. Claude -> Gemini) within the same session.
*   **Purpose**: when the system detects that a signature in the message history belongs to an incompatible model family (e.g. `claude-3-5` vs `gemini-2.0`), it automatically discards the old signature to prevent the API from rejecting the request.

## Custom Configuration

Currently these settings can be adjusted by modifying the `default_true` default values in `src-tauri/src/proxy/config.rs`, or by waiting for a future version that integrates them into the "Settings -> Advanced" UI.

```rust
// src-tauri/src/proxy/config.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentalConfig {
    #[serde(default = "default_true")]
    pub enable_signature_cache: bool,
    // ...
}
```
