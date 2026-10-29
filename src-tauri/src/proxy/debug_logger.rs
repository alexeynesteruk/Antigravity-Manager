use futures::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::fs;

use crate::proxy::config::DebugLoggingConfig;

const DEBUG_STREAM_HEAD_BYTES: usize = 256 * 1024;
const DEBUG_STREAM_TAIL_BYTES: usize = 256 * 1024;

struct BoundedStreamCapture {
    head: Vec<u8>,
    tail: Vec<u8>,
    total_bytes: usize,
}

impl BoundedStreamCapture {
    fn new() -> Self {
        Self {
            head: Vec::with_capacity(DEBUG_STREAM_HEAD_BYTES),
            tail: Vec::with_capacity(DEBUG_STREAM_TAIL_BYTES),
            total_bytes: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        let head_len = (DEBUG_STREAM_HEAD_BYTES - self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_len]);
        let remaining = &bytes[head_len..];
        if remaining.len() >= DEBUG_STREAM_TAIL_BYTES {
            self.tail.clear();
            self.tail
                .extend_from_slice(&remaining[remaining.len() - DEBUG_STREAM_TAIL_BYTES..]);
        } else if !remaining.is_empty() {
            let overflow = self
                .tail
                .len()
                .saturating_add(remaining.len())
                .saturating_sub(DEBUG_STREAM_TAIL_BYTES);
            if overflow > 0 {
                self.tail.drain(..overflow);
            }
            self.tail.extend_from_slice(remaining);
        }
    }

    fn truncated(&self) -> bool {
        self.total_bytes > self.head.len() + self.tail.len()
    }

    fn bounded_bytes(&self) -> Vec<u8> {
        let marker = b"\n...[debug stream truncated]...\n";
        let mut bytes = Vec::with_capacity(
            self.head.len() + self.tail.len() + usize::from(self.truncated()) * marker.len(),
        );
        bytes.extend_from_slice(&self.head);
        if self.truncated() {
            bytes.extend_from_slice(marker);
        }
        bytes.extend_from_slice(&self.tail);
        bytes
    }
}

fn build_filename(prefix: &str, trace_id: Option<&str>) -> String {
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S%.3f");
    let tid = trace_id.unwrap_or("unknown");
    format!("{}_{}_{}.json", ts, tid, prefix)
}

fn resolve_output_dir(cfg: &DebugLoggingConfig) -> Option<PathBuf> {
    if let Some(dir) = cfg.output_dir.as_ref() {
        return Some(PathBuf::from(dir));
    }
    if let Ok(data_dir) = crate::modules::account::get_data_dir() {
        return Some(data_dir.join("debug_logs"));
    }
    None
}

fn resolve_exchange_output_dir(cfg: &DebugLoggingConfig) -> Option<PathBuf> {
    resolve_output_dir(cfg).map(|dir| dir.join("debug_exchanges"))
}

pub async fn write_debug_payload(
    cfg: &DebugLoggingConfig,
    trace_id: Option<&str>,
    prefix: &str,
    payload: &Value,
) {
    if cfg.metadata_only || !is_enabled(cfg) {
        return;
    }

    let output_dir = match resolve_output_dir(cfg) {
        Some(dir) => dir,
        None => {
            tracing::warn!("[Debug-Log] Enabled but output_dir is not available.");
            return;
        }
    };

    if let Err(e) = fs::create_dir_all(&output_dir).await {
        tracing::warn!("[Debug-Log] Failed to create output dir: {}", e);
        return;
    }

    let filename = build_filename(prefix, trace_id);
    let path = output_dir.join(filename);

    match serde_json::to_vec_pretty(payload) {
        Ok(bytes) => {
            if let Err(e) = fs::write(&path, bytes).await {
                tracing::warn!("[Debug-Log] Failed to write file: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("[Debug-Log] Failed to serialize payload: {}", e);
        }
    }
}

pub async fn write_exchange_payload(
    cfg: &DebugLoggingConfig,
    trace_id: Option<&str>,
    prefix: &str,
    payload: &Value,
) {
    if cfg.metadata_only || !is_enabled(cfg) {
        return;
    }

    let output_dir = match resolve_exchange_output_dir(cfg) {
        Some(dir) => dir,
        None => {
            tracing::warn!("[Debug-Exchange] Enabled but output_dir is not available.");
            return;
        }
    };

    if let Err(e) = fs::create_dir_all(&output_dir).await {
        tracing::warn!("[Debug-Exchange] Failed to create output dir: {}", e);
        return;
    }

    let filename = build_filename(prefix, trace_id);
    let path = output_dir.join(filename);

    match serde_json::to_vec_pretty(payload) {
        Ok(bytes) => {
            if let Err(e) = fs::write(&path, bytes).await {
                tracing::warn!("[Debug-Exchange] Failed to write file: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("[Debug-Exchange] Failed to serialize payload: {}", e);
        }
    }
}

pub fn is_payload_enabled(cfg: &DebugLoggingConfig) -> bool {
    !cfg.metadata_only && (cfg.enabled || crate::modules::log_bridge::is_log_bridge_enabled())
}

pub fn is_enabled(cfg: &DebugLoggingConfig) -> bool {
    cfg.enabled || cfg.metadata_only || crate::modules::log_bridge::is_log_bridge_enabled()
}

#[derive(Clone)]
pub(crate) struct UpstreamDiagnosticContext {
    pub cfg: DebugLoggingConfig,
    pub request_id: String,
    pub physical_request: u32,
}

fn upstream_diagnostic_context() -> Option<UpstreamDiagnosticContext> {
    crate::proxy::monitor::CURRENT_UPSTREAM_CAPTURE
        .try_with(|capture| capture.diagnostic_context())
        .ok()
        .flatten()
}

fn identifier_hash(kind: &str, value: Option<&str>) -> Option<String> {
    value.map(|value| {
        let mut hash = Sha256::new();
        hash.update(kind.as_bytes());
        hash.update(b"\0");
        hash.update(value.as_bytes());
        format!("{:x}", hash.finalize())
    })
}

// Origins omit userinfo, paths, queries and fragments, all of which may contain secrets.
fn endpoint_origin(endpoint: &str) -> Option<String> {
    url::Url::parse(endpoint)
        .ok()
        .map(|url| url.origin().ascii_serialization())
}

fn dispatch_diagnostic(endpoint: &str, method: &str, body: &Value, account: Option<&str>) -> Value {
    serde_json::json!({
        "endpoint_origin": endpoint_origin(endpoint),
        "method": method,
        "model": body.get("model").and_then(Value::as_str),
        "account_sha256": identifier_hash("account", account),
        "project_sha256": identifier_hash("project", body.get("project").and_then(Value::as_str)),
        "session_sha256": identifier_hash("session", body.pointer("/request/sessionId").and_then(Value::as_str)),
        "trajectory_sha256": identifier_hash("trajectory", body.pointer("/request/labels/trajectory_id").and_then(Value::as_str)),
        "wire_request_id_sha256": identifier_hash("request_id", body.get("requestId").and_then(Value::as_str)),
    })
}

// Keep the upstream spelling and field presence. Never reconstruct totals or copy arbitrary metadata.
fn diagnostic_usage(usage: &Value) -> Value {
    let Some(usage) = usage.as_object() else {
        return Value::Null;
    };
    let mut fields = serde_json::Map::new();
    for name in [
        "promptTokenCount",
        "cachedContentTokenCount",
        "cached_content_token_count",
        "candidatesTokenCount",
        "candidates_token_count",
        "thoughtsTokenCount",
        "totalThoughtTokens",
        "totalTokenCount",
        "total_input_tokens",
        "total_cached_tokens",
        "total_output_tokens",
        "total_thought_tokens",
        "total_tokens",
    ] {
        if let Some(value) = usage.get(name) {
            fields.insert(
                name.to_string(),
                value.as_u64().map_or(Value::Null, Value::from),
            );
        }
    }
    Value::Object(fields)
}

const MAX_DIAGNOSTIC_EVENT_BYTES: usize = 256 * 1024;
const MAX_DIAGNOSTIC_RECORD_BYTES: usize = 8192;

static DIAGNOSTIC_PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
type DiagnosticWrite = (PathBuf, Vec<u8>);
fn enqueue_diagnostic(cfg: &DebugLoggingConfig, request_id: &str, prefix: &str, payload: &Value) {
    static WRITER: std::sync::OnceLock<std::sync::mpsc::SyncSender<DiagnosticWrite>> =
        std::sync::OnceLock::new();
    let Some(dir) = resolve_output_dir(cfg) else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(payload) else {
        return;
    };
    if bytes.len() > MAX_DIAGNOSTIC_RECORD_BYTES {
        tracing::warn!("[Diagnostics] Metadata record exceeded size limit; skipped");
        return;
    }
    let sender = WRITER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<DiagnosticWrite>(256);
        if let Err(error) = std::thread::Builder::new()
            .name("ag-metadata-writer".into())
            .spawn(move || {
                while let Ok((path, bytes)) = rx.recv() {
                    let result = path
                        .parent()
                        .map(std::fs::create_dir_all)
                        .transpose()
                        .and_then(|_| std::fs::write(&path, bytes));
                    DIAGNOSTIC_PENDING.fetch_sub(1, std::sync::atomic::Ordering::Release);
                    if let Err(error) = result {
                        tracing::warn!("[Diagnostics] Could not persist metadata: {error}");
                    }
                }
            })
        {
            tracing::warn!("[Diagnostics] Could not start metadata writer: {error}");
        }
        tx
    });
    DIAGNOSTIC_PENDING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if sender
        .try_send((dir.join(build_filename(prefix, Some(request_id))), bytes))
        .is_err()
    {
        DIAGNOSTIC_PENDING.fetch_sub(1, std::sync::atomic::Ordering::Release);
        tracing::warn!("[Diagnostics] Metadata writer unavailable or queue full; record skipped");
    }
}

async fn write_upstream_diagnostic(
    ctx: &UpstreamDiagnosticContext,
    kind: &str,
    frame: u64,
    data: Value,
) {
    let prefix = format!("{}_{}_{}", kind, ctx.physical_request, frame);
    let payload = serde_json::json!({
        "kind": kind,
        "timestamp_ms": chrono::Utc::now().timestamp_millis(),
        "request_id": ctx.request_id,
        "physical_request": ctx.physical_request,
        "data": data,
    });
    enqueue_diagnostic(&ctx.cfg, &ctx.request_id, &prefix, &payload);
}

pub(crate) async fn log_upstream_dispatch(
    endpoint: &str,
    method: &str,
    body: &Value,
    account: Option<&str>,
) {
    if let Some(ctx) = upstream_diagnostic_context() {
        write_upstream_diagnostic(
            &ctx,
            "upstream_dispatch",
            0,
            dispatch_diagnostic(endpoint, method, body, account),
        )
        .await;
    }
}

pub(crate) async fn log_upstream_result(endpoint: Option<&str>, status: Option<u16>) {
    if let Some(ctx) = upstream_diagnostic_context() {
        write_upstream_diagnostic(
            &ctx,
            "upstream_result",
            0,
            serde_json::json!({
                "endpoint_origin": endpoint.and_then(endpoint_origin),
                "status": status,
                "network_error": status.is_none(),
            }),
        )
        .await;
    }
}

pub(crate) async fn log_upstream_json_usage(raw: &Value) {
    if let Some(ctx) = upstream_diagnostic_context() {
        let usage = raw.get("response").unwrap_or(raw).get("usageMetadata");
        write_upstream_diagnostic(
            &ctx,
            "upstream_usage_json",
            0,
            serde_json::json!({
                "usage_metadata_present": usage.is_some(),
                "usage": usage.map(diagnostic_usage),
            }),
        )
        .await;
    }
}

#[derive(Default)]
struct UpstreamUsageObserver {
    line: Vec<u8>,
    data: Vec<u8>,
    frame: u64,
    dropping_event: bool,
    truncated_events: u64,
    dropped_usage_frames: u64,
}

impl UpstreamUsageObserver {
    fn finish_event(&mut self) -> Option<(u64, Value)> {
        if self.data.is_empty() {
            if std::mem::take(&mut self.dropping_event) {
                self.truncated_events += 1;
            }
            return None;
        }
        self.frame += 1;
        let data = std::mem::take(&mut self.data);
        if std::mem::take(&mut self.dropping_event) {
            self.truncated_events += 1;
            return None;
        }
        let raw: Value = serde_json::from_slice(&data).ok()?;
        let usage = raw.get("response").unwrap_or(&raw).get("usageMetadata")?;
        Some((self.frame, diagnostic_usage(usage)))
    }

    fn finish_line(&mut self) -> Option<(u64, Value)> {
        let line = std::mem::take(&mut self.line);
        let line = line.strip_suffix(b"\n").unwrap_or(&line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return self.finish_event();
        }
        if !self.dropping_event {
            if let Some(data) = line.strip_prefix(b"data:") {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
            }
        }
        if self.data.len() > MAX_DIAGNOSTIC_EVENT_BYTES {
            self.data.clear();
            self.dropping_event = true;
        }
        None
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<(u64, Value)> {
        let mut usage = Vec::new();
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.line.len().saturating_add(part.len()) > MAX_DIAGNOSTIC_EVENT_BYTES {
                self.line.clear();
                self.line.push(b'x');
                self.data.clear();
                self.dropping_event = true;
            } else if !self.dropping_event || self.line.is_empty() {
                self.line.extend_from_slice(part);
            }
            if part.ends_with(b"\n") {
                if let Some(frame) = self.finish_line() {
                    if usage.len() < 128 {
                        usage.push(frame);
                    } else {
                        self.dropped_usage_frames += 1;
                    }
                }
            }
        }
        usage
    }

    fn finish(&mut self) -> Vec<(u64, Value)> {
        let mut usage = Vec::new();
        if !self.line.is_empty() {
            usage.extend(self.finish_line());
        }
        usage.extend(self.finish_event());
        usage
    }
}

// Observe the original transport, before auto-healing or protocol conversion. Forward every item unchanged.
pub(crate) fn wrap_stream_with_upstream_usage_diagnostics<S, E>(
    stream: std::pin::Pin<Box<S>>,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, E>> + Send>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let Some(ctx) = upstream_diagnostic_context() else {
        return stream;
    };
    Box::pin(async_stream::stream! {
        let mut observer = UpstreamUsageObserver::default();
        let mut usage_frames = 0u64;
        let mut stream_error = false;
        let mut inner = stream;
        while let Some(item) = inner.next().await {
            if let Ok(bytes) = &item {
                for (frame, usage) in observer.push(bytes) {
                    usage_frames += 1;
                    write_upstream_diagnostic(&ctx, "upstream_usage_sse", frame, serde_json::json!({
                        "sse_frame": frame, "usage": usage,
                    })).await;
                }
            } else {
                stream_error = true;
            }
            yield item;
        }
        for (frame, usage) in observer.finish() {
            usage_frames += 1;
            write_upstream_diagnostic(&ctx, "upstream_usage_sse", frame, serde_json::json!({
                "sse_frame": frame, "usage": usage,
            })).await;
        }
        write_upstream_diagnostic(&ctx, "upstream_usage_end", 0, serde_json::json!({
            "usage_frames": usage_frames, "stream_error": stream_error, "truncated_events": observer.truncated_events, "dropped_usage_frames": observer.dropped_usage_frames,
        })).await;
    })
}

/// Parse SSE streaming data, extracting thinking and body content
fn parse_sse_stream(raw: &str) -> (String, String) {
    let mut thinking_parts: Vec<String> = Vec::new();
    let mut content_parts: Vec<String> = Vec::new();

    for line in raw.lines() {
        let line = line.trim();
        if !line.starts_with("data: ") {
            continue;
        }
        let json_str = &line[6..]; // Strip the "data: " prefix
        if json_str.is_empty() || json_str == "[DONE]" {
            continue;
        }

        // Try to parse as JSON
        if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
            // Gemini/v1internal format: response.candidates[0].content.parts[0]
            if let Some(candidates) = parsed
                .get("response")
                .and_then(|r| r.get("candidates"))
                .and_then(|c| c.as_array())
            {
                for candidate in candidates {
                    if let Some(parts) = candidate
                        .get("content")
                        .and_then(|c| c.get("parts"))
                        .and_then(|p| p.as_array())
                    {
                        for part in parts {
                            let text = part.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            let is_thought = part
                                .get("thought")
                                .and_then(|t| t.as_bool())
                                .unwrap_or(false);

                            if !text.is_empty() {
                                if is_thought {
                                    thinking_parts.push(text.to_string());
                                } else {
                                    content_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
            // OpenAI format compatibility: choices[0].delta.content
            else if let Some(choices) = parsed.get("choices").and_then(|c| c.as_array()) {
                for choice in choices {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            if !content.is_empty() {
                                content_parts.push(content.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    (thinking_parts.join(""), content_parts.join(""))
}

pub fn wrap_stream_with_debug<S, E>(
    stream: std::pin::Pin<Box<S>>,
    cfg: DebugLoggingConfig,
    trace_id: String,
    prefix: &'static str,
    meta: Value,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, E>> + Send>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    if !is_enabled(&cfg) {
        return stream;
    }

    let stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, E>> + Send>> =
        if prefix == "upstream_response" {
            wrap_stream_with_upstream_usage_diagnostics(stream)
        } else {
            stream
        };

    if cfg.metadata_only {
        return stream;
    }

    let wrapped = async_stream::stream! {
        let mut capture = BoundedStreamCapture::new();
        let mut inner = stream;
        while let Some(item) = inner.next().await {
            if let Ok(bytes) = &item {
                capture.push(bytes);
            }
            yield item;
        }

        let raw_text = String::from_utf8_lossy(&capture.bounded_bytes()).to_string();
        let (thinking_content, response_content) = parse_sse_stream(&raw_text);

        let mut payload = serde_json::json!({
            "kind": prefix,
            "trace_id": trace_id,
            "meta": meta,
            "raw_stream": raw_text,
            "raw_stream_bytes": capture.total_bytes,
            "truncated": capture.truncated(),
        });

        // Only add the corresponding field when there is content
        if !thinking_content.is_empty() {
            payload["thinking_content"] = serde_json::Value::String(thinking_content);
        }
        if !response_content.is_empty() {
            payload["response_content"] = serde_json::Value::String(response_content);
        }

        write_exchange_payload(&cfg, Some(&payload["trace_id"].as_str().unwrap_or("unknown")), prefix, &payload).await;
    };

    Box::pin(wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::monitor::{UpstreamRequestBodyHolder, CURRENT_UPSTREAM_CAPTURE};
    use futures::{stream, StreamExt};

    fn diagnostic_holder(dir: &std::path::Path) -> UpstreamRequestBodyHolder {
        let holder = UpstreamRequestBodyHolder::new();
        holder.configure_diagnostics(
            DebugLoggingConfig {
                enabled: false,
                metadata_only: true,
                output_dir: Some(dir.to_string_lossy().into_owned()),
            },
            "monitor-fixture-id".to_string(),
        );
        holder.record_dispatch();
        holder
    }

    async fn diagnostic_files(dir: &std::path::Path) -> Vec<Value> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while DIAGNOSTIC_PENDING.load(std::sync::atomic::Ordering::Acquire) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("diagnostic writer drained");
        let mut files = tokio::fs::read_dir(dir).await.unwrap();
        let mut values = Vec::new();
        while let Some(file) = files.next_entry().await.unwrap() {
            values.push(
                serde_json::from_slice(&tokio::fs::read(file.path()).await.unwrap()).unwrap(),
            );
        }
        values
    }

    #[test]
    fn upstream_diagnostics_dispatch_reads_actual_fields_and_never_mutates_request() {
        let body = serde_json::json!({
            "model": "upstream-model", "project": "private-project-fixture",
            "requestId": "private-wire-id-fixture",
            "labels": {"trajectory_id": "decoy-trajectory"},
            "request": {
                "sessionId": "private-session-fixture",
                "labels": {"trajectory_id": "actual-trajectory-fixture"},
                "systemInstruction": {"parts": [{"text": "private-system-fixture"}]},
                "contents": [{"parts": [{"functionCall": {"args": {"key": "private-tool-fixture"}}}]}],
            },
            "Authorization": "private-credential-fixture",
        });
        let before = serde_json::to_vec(&body).unwrap();
        let data = dispatch_diagnostic(
            "https://private-user:private-password@upstream.example:8443/private-path?key=private-query#private-fragment",
            "streamGenerateContent", &body, Some("private-account-fixture"),
        );
        assert_eq!(data["endpoint_origin"], "https://upstream.example:8443");
        assert_eq!(data["model"], "upstream-model");
        assert_eq!(
            data["trajectory_sha256"],
            serde_json::json!(identifier_hash(
                "trajectory",
                Some("actual-trajectory-fixture")
            ))
        );
        assert_eq!(
            data["session_sha256"],
            serde_json::json!(identifier_hash("session", Some("private-session-fixture")))
        );
        assert_eq!(
            data["project_sha256"],
            serde_json::json!(identifier_hash("project", Some("private-project-fixture")))
        );
        assert_eq!(
            data["account_sha256"],
            serde_json::json!(identifier_hash("account", Some("private-account-fixture")))
        );
        assert_eq!(before, serde_json::to_vec(&body).unwrap());
        let logged = data.to_string();
        for private in [
            "private-",
            "actual-trajectory-fixture",
            "decoy-trajectory",
            "Authorization",
            "systemInstruction",
            "functionCall",
        ] {
            assert!(
                !logged.contains(private),
                "unexpected private field: {private}"
            );
        }
        let absent = dispatch_diagnostic(
            "invalid endpoint",
            "generateContent",
            &serde_json::json!({}),
            None,
        );
        assert!(absent["project_sha256"].is_null());
        assert!(absent["trajectory_sha256"].is_null());
        assert!(absent["endpoint_origin"].is_null());
    }

    #[test]
    fn upstream_diagnostics_usage_keeps_missing_zero_and_raw_output_separate() {
        let missing = diagnostic_usage(
            &serde_json::json!({"promptTokenCount":100,"candidatesTokenCount":2,"thoughtsTokenCount":3}),
        );
        assert!(missing.get("cachedContentTokenCount").is_none());
        assert_eq!(missing["candidatesTokenCount"], 2);
        assert_eq!(missing["thoughtsTokenCount"], 3);
        assert!(missing.get("totalTokenCount").is_none());
        let zero = diagnostic_usage(
            &serde_json::json!({"cachedContentTokenCount":0,"total_cached_tokens":0}),
        );
        assert_eq!(zero["cachedContentTokenCount"], 0);
        assert_eq!(zero["total_cached_tokens"], 0);
        let aliases = diagnostic_usage(
            &serde_json::json!({"cached_content_token_count":0,"candidates_token_count":2,"totalThoughtTokens":3}),
        );
        assert_eq!(aliases["cached_content_token_count"], 0);
        assert_eq!(aliases["candidates_token_count"], 2);
        assert_eq!(aliases["totalThoughtTokens"], 3);
        assert!(aliases.get("cachedContentTokenCount").is_none());
        let unknown = diagnostic_usage(
            &serde_json::json!({"cachedContentTokenCount":null,"promptTokenCount":"private-value","private_metadata":"private-value"}),
        );
        assert!(unknown
            .as_object()
            .unwrap()
            .contains_key("cachedContentTokenCount"));
        assert!(unknown["cachedContentTokenCount"].is_null());
        assert!(unknown["promptTokenCount"].is_null());
        assert!(!unknown.to_string().contains("private"));
    }

    #[tokio::test]
    async fn upstream_diagnostics_observe_original_sse_once_and_forward_items_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let holder = diagnostic_holder(dir.path());
        let raw = concat!(
            ": ping\r\n\r\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"private-body-fixture 秘密\"}]}}]}}\n\n",
            "data: {\"response\":\n",
            "data: {\"usageMetadata\":{\"promptTokenCount\":100,\"cachedContentTokenCount\":0}}}\n\n",
            "data: {\"response\":{\"usageMetadata\":{\"candidatesTokenCount\":2,\"thoughtsTokenCount\":3}}}",
        );
        let mut items = raw
            .as_bytes()
            .chunks(7)
            .map(|chunk| Ok(bytes::Bytes::copy_from_slice(chunk)))
            .collect::<Vec<Result<_, String>>>();
        items.push(Err("private-error-fixture".to_string()));
        let expected = items.clone();
        let forwarded = CURRENT_UPSTREAM_CAPTURE
            .scope(holder, async {
                wrap_stream_with_upstream_usage_diagnostics(Box::pin(stream::iter(items)))
                    .collect::<Vec<_>>()
                    .await
            })
            .await;
        assert_eq!(forwarded, expected);
        let logs = diagnostic_files(dir.path()).await;
        let mut usage = logs
            .iter()
            .filter(|log| log["kind"] == "upstream_usage_sse")
            .collect::<Vec<_>>();
        usage.sort_by_key(|log| log["data"]["sse_frame"].as_u64().unwrap());
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0]["data"]["sse_frame"], 2);
        assert_eq!(usage[0]["data"]["usage"]["cachedContentTokenCount"], 0);
        assert!(usage[1]["data"]["usage"]
            .get("cachedContentTokenCount")
            .is_none());
        assert_eq!(usage[1]["data"]["usage"]["candidatesTokenCount"], 2);
        assert_eq!(usage[1]["data"]["usage"]["thoughtsTokenCount"], 3);
        let end = logs
            .iter()
            .find(|log| log["kind"] == "upstream_usage_end")
            .unwrap();
        assert_eq!(end["data"]["usage_frames"], 2);
        assert_eq!(end["data"]["stream_error"], true);
        for log in logs {
            assert_eq!(log["request_id"], "monitor-fixture-id");
            assert_eq!(log["physical_request"], 1);
            assert!(!log.to_string().contains("private-"));
        }
    }

    #[tokio::test]
    async fn upstream_diagnostics_stream_snapshot_survives_later_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let holder = diagnostic_holder(dir.path());
        CURRENT_UPSTREAM_CAPTURE
            .scope(holder.clone(), async {
                let chunk = bytes::Bytes::from_static(
                    b"data: {\"usageMetadata\":{\"promptTokenCount\":100}}\n\n",
                );
                let first =
                    wrap_stream_with_upstream_usage_diagnostics(Box::pin(stream::iter(vec![
                        Ok::<_, String>(chunk.clone()),
                    ])));
                holder.record_dispatch();
                let second =
                    wrap_stream_with_upstream_usage_diagnostics(Box::pin(stream::iter(vec![
                        Ok::<_, String>(chunk),
                    ])));
                second.collect::<Vec<_>>().await;
                first.collect::<Vec<_>>().await;
            })
            .await;
        let logs = diagnostic_files(dir.path()).await;
        let mut attempts = logs
            .iter()
            .filter(|log| log["kind"] == "upstream_usage_sse")
            .map(|log| log["physical_request"].as_u64().unwrap())
            .collect::<Vec<_>>();
        attempts.sort();
        assert_eq!(attempts, vec![1, 2]);
    }

    #[tokio::test]
    async fn upstream_diagnostics_json_distinguishes_absent_metadata_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let holder = diagnostic_holder(dir.path());
        CURRENT_UPSTREAM_CAPTURE.scope(holder.clone(), async {
            log_upstream_json_usage(&serde_json::json!({"response":{"private":"private-response"}})).await;
            holder.record_dispatch();
            log_upstream_json_usage(&serde_json::json!({"response":{"usageMetadata":{"cachedContentTokenCount":0}}})).await;
        }).await;
        let mut logs = diagnostic_files(dir.path()).await;
        logs.sort_by_key(|log| log["physical_request"].as_u64().unwrap());
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0]["data"]["usage_metadata_present"], false);
        assert!(logs[0]["data"]["usage"].is_null());
        assert_eq!(logs[1]["data"]["usage_metadata_present"], true);
        assert_eq!(logs[1]["data"]["usage"]["cachedContentTokenCount"], 0);
        assert!(!logs.iter().any(|log| log.to_string().contains("private")));
    }

    #[tokio::test]
    async fn upstream_diagnostics_disabled_does_no_io_and_forwards_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let holder = UpstreamRequestBodyHolder::new();
        let cfg = DebugLoggingConfig {
            enabled: false,
            metadata_only: false,
            output_dir: Some(dir.path().to_string_lossy().into_owned()),
        };
        assert!(!is_enabled(&cfg));
        holder.configure_diagnostics(cfg, "disabled-monitor".to_string());
        holder.record_dispatch();
        let chunk = bytes::Bytes::from_static(
            b"data: {\"usageMetadata\":{\"cachedContentTokenCount\":0}}\n\n",
        );
        CURRENT_UPSTREAM_CAPTURE
            .scope(holder.clone(), async {
                log_upstream_dispatch(
                    "https://upstream.example/private?key=private",
                    "generateContent",
                    &serde_json::json!({}),
                    None,
                )
                .await;
                let items =
                    wrap_stream_with_upstream_usage_diagnostics(Box::pin(stream::iter(vec![
                        Ok::<_, String>(chunk.clone()),
                    ])))
                    .collect::<Vec<_>>()
                    .await;
                assert_eq!(items, vec![Ok(chunk)]);
            })
            .await;
        assert!(holder.diagnostic_context().is_none());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn debug_stream_capture_is_bounded_and_forwarding_is_unchanged() {
        let data = bytes::Bytes::from(vec![b'A'; 1024 * 1024]);
        let output_dir = std::env::temp_dir().join(format!(
            "antigravity-debug-stream-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let cfg = DebugLoggingConfig {
            enabled: true,
            metadata_only: false,
            output_dir: Some(output_dir.to_string_lossy().into_owned()),
        };
        let wrapped = wrap_stream_with_debug(
            Box::pin(stream::iter(vec![Ok::<_, String>(data.clone())])),
            cfg,
            "bounded-stream".to_string(),
            "test_stream",
            serde_json::json!({}),
        );
        let forwarded = wrapped
            .map(|item| item.expect("forwarded chunk"))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(forwarded, vec![data]);

        let exchange_dir = output_dir.join("debug_exchanges");
        let mut entries = tokio::fs::read_dir(&exchange_dir)
            .await
            .expect("debug log dir");
        let path = entries
            .next_entry()
            .await
            .expect("read debug entry")
            .expect("debug entry")
            .path();
        let payload: Value =
            serde_json::from_slice(&tokio::fs::read(path).await.expect("read debug payload"))
                .expect("debug json");
        assert_eq!(payload["raw_stream_bytes"], 1024 * 1024);
        assert_eq!(payload["truncated"], true);
        assert!(
            payload["raw_stream"].as_str().unwrap().len()
                <= DEBUG_STREAM_HEAD_BYTES + DEBUG_STREAM_TAIL_BYTES + 64
        );
        tokio::fs::remove_dir_all(output_dir)
            .await
            .expect("remove test debug dir");
    }
}

#[cfg(test)]
mod bounded_metadata_regressions {
    use super::*;
    #[test]
    fn oversized_sse_event_is_skipped_and_next_usage_still_observed() {
        let mut observer = UpstreamUsageObserver::default();
        let mut huge = b"data: ".to_vec();
        huge.extend(vec![b'x'; MAX_DIAGNOSTIC_EVENT_BYTES * 2]);
        huge.extend(b"\n\n");
        assert!(observer.push(&huge).is_empty());
        let frames =
            observer.push(b"data: {\"usageMetadata\":{\"cachedContentTokenCount\":0}}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].1["cachedContentTokenCount"], 0);
        assert!(observer.line.len() <= MAX_DIAGNOSTIC_EVENT_BYTES);
        assert!(observer.data.len() <= MAX_DIAGNOSTIC_EVENT_BYTES);
        assert_eq!(observer.truncated_events, 1);
    }
    #[tokio::test]
    async fn metadata_only_mode_never_writes_legacy_private_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DebugLoggingConfig {
            enabled: true,
            metadata_only: true,
            output_dir: Some(dir.path().to_string_lossy().into_owned()),
        };
        write_debug_payload(
            &cfg,
            Some("fixture"),
            "legacy",
            &serde_json::json!({"secret":"private"}),
        )
        .await;
        write_exchange_payload(
            &cfg,
            Some("fixture"),
            "legacy",
            &serde_json::json!({"secret":"private"}),
        )
        .await;
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
