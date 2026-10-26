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
