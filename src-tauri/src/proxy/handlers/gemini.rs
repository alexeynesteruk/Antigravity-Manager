// Gemini Handler
use axum::{
    extract::State,
    extract::{Json, Path},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use tracing::{debug, error, info};

use crate::proxy::common::client_adapter::CLIENT_ADAPTERS;
use crate::proxy::debug_logger;
use crate::proxy::handlers::common::{
    apply_retry_strategy, next_rotation_attempt, should_rotate_account, FailureStatusTracker,
    RequestRetryState, RetryStrategy,
};
use crate::proxy::mappers::gemini::{unwrap_response, wrap_request, wrap_request_v2};
use crate::proxy::server::AppState;
use crate::proxy::session_manager::SessionManager;
use crate::proxy::upstream::client::mask_email;
use axum::http::HeaderMap;

const MAX_RETRY_ATTEMPTS: usize = 3;

fn response_has_inline_image_data(value: &Value) -> bool {
    let response = value.get("response").unwrap_or(value);
    response
        .get("candidates")
        .and_then(Value::as_array)
        .is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .get("content")
                    .and_then(|content| content.get("parts"))
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part.get("inlineData")
                                .or_else(|| part.get("inline_data"))
                                .and_then(|image| image.get("data"))
                                .and_then(Value::as_str)
                                .is_some_and(|data| !data.is_empty())
                        })
                    })
            })
        })
}

#[cfg(test)]
mod image_success_tests {
    use super::response_has_inline_image_data;
    use serde_json::json;

    #[test]
    fn task_gemini_image_success_requires_nonempty_payload() {
        let empty = json!({
            "response": {"candidates": [{"content": {"parts": [{"inlineData": {"data": ""}}]}}]}
        });
        let image = json!({
            "response": {"candidates": [{"content": {"parts": [{"inlineData": {"data": "AQ=="}}]}}]}
        });
        assert!(!response_has_inline_image_data(&empty));
        assert!(response_has_inline_image_data(&image));
    }
}

/// Handle generateContent and streamGenerateContent
/// Path params: model_name, method (e.g. "gemini-pro", "generateContent")
pub async fn handle_generate(
    State(state): State<AppState>,
    Path(model_action): Path<String>,
    headers: HeaderMap,          // [NEW] Extract headers for adapter detection
    Json(mut body): Json<Value>, // mut so we can inject the fix-up prompt
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Parse model:method
    let (model_name, method) = if let Some((m, action)) = model_action.rsplit_once(':') {
        (m.to_string(), action.to_string())
    } else {
        (model_action, "generateContent".to_string())
    };

    crate::modules::logger::log_info(&format!(
        "Received Gemini request: {}/{}",
        model_name, method
    ));
    let trace_id = format!("req_{}", chrono::Utc::now().timestamp_subsec_millis());
    let debug_cfg = state.debug_logging.read().await.clone();

    // [NEW] Detect Client Adapter
    let client_adapter = CLIENT_ADAPTERS
        .iter()
        .find(|a| a.matches(&headers))
        .cloned();
    if client_adapter.is_some() {
        debug!("[{}] Client Adapter detected", trace_id);
    }

    // 1. Validate the method
    // [NEW] :countTokens colon syntax, proxy directly to upstream v1internal:countTokens
    if method == "countTokens" {
        return execute_count_tokens(state, model_name, body).await;
    }

    if method != "generateContent" && method != "streamGenerateContent" {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Unsupported method: {}", method),
        ));
    }
    if debug_logger::is_enabled(&debug_cfg) {
        let original_payload = json!({
            "kind": "original_request",
            "protocol": "gemini",
            "trace_id": trace_id,
            "original_model": model_name,
            "method": method,
            "request": body.clone(),
        });
        debug_logger::write_debug_payload(
            &debug_cfg,
            Some(&trace_id),
            "original_request",
            &original_payload,
        )
        .await;
    }
    let client_wants_stream = method == "streamGenerateContent";
    // [AUTO-CONVERSION] Force internal streaming
    let force_stream_internally = !client_wants_stream;
    let is_stream = client_wants_stream || force_stream_internally;

    if force_stream_internally {
        // debug!("[AutoConverter] Converting non-stream request to stream");
    }

    // 2. Obtain the UpstreamClient and TokenManager
    let upstream = state.upstream.clone();
    let image_scheduler = state.image_scheduler.clone();
    let request_timeout = state.request_timeout;
    let token_manager = state.token_manager;
    let pool_size = token_manager.len();
    let max_attempts = MAX_RETRY_ATTEMPTS.min(pool_size).max(1);

    let mut last_error = String::new();
    let mut last_email: Option<String> = None;
    let mut force_rotate = false;
    let mut retry_state = RequestRetryState::default();
    let mut retry_credentials: Option<(String, String, String, String, u64)> = None;
    let mut image_permit = None;
    let mut failure_statuses = FailureStatusTracker::default();
    let mut used_attempts = 0;

    while let Some(attempt) = next_rotation_attempt(
        &mut used_attempts,
        max_attempts,
        retry_credentials.is_some(),
    ) {
        // 3. Model route resolution
        let mapped_model = crate::proxy::common::model_mapping::resolve_model_route(
            &model_name,
            &*state.custom_mapping.read().await,
        );
        // Extract the tools list for web-search probing (Gemini style may be nested)
        let tools_val: Option<Vec<Value>> =
            body.get("tools").and_then(|t| t.as_array()).map(|arr| {
                let mut flattened = Vec::new();
                for tool_entry in arr {
                    if let Some(decls) = tool_entry
                        .get("functionDeclarations")
                        .and_then(|v| v.as_array())
                    {
                        flattened.extend(decls.iter().cloned());
                    } else {
                        flattened.push(tool_entry.clone());
                    }
                }
                flattened
            });

        let config = crate::proxy::mappers::common_utils::resolve_request_config(
            &model_name,
            &mapped_model,
            &tools_val,
            None,        // size (not applicable for Gemini native protocol)
            None,        // quality
            None,        // [NEW] image_size
            Some(&body), // [NEW] Pass request body for imageConfig parsing
        );

        // 4. Obtain a token (using the accurate request_type)
        // Extract the SessionId (sticky fingerprint)
        let session_id = SessionManager::extract_gemini_session_id(&body, &model_name);

        // Key: decide whether to rotate accounts based on the force_rotate flag (supports Grace Retry in-place retry)
        let (access_token, project_id, email, account_id, _wait_ms) =
            if let Some(credentials) = retry_credentials.take() {
                credentials
            } else if config.request_type == "image_gen" {
                drop(image_permit.take());
                match token_manager
                    .get_image_token(
                        force_rotate,
                        Some(&session_id),
                        &config.final_model,
                        &image_scheduler,
                        request_timeout,
                    )
                    .await
                {
                    Ok((access_token, project_id, email, account_id, wait_ms, permit)) => {
                        image_permit = Some(permit);
                        (access_token, project_id, email, account_id, wait_ms)
                    }
                    Err((status, message)) => {
                        failure_statuses.record(status);
                        last_error = message;
                        break;
                    }
                }
            } else {
                match token_manager
                    .get_token(
                        &config.request_type,
                        force_rotate,
                        Some(&session_id),
                        &config.final_model,
                    )
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        return Err((
                            StatusCode::SERVICE_UNAVAILABLE,
                            format!("Token error: {}", e),
                        ));
                    }
                }
            };

        let mapped_model = token_manager
            .resolve_dynamic_model_for_account(&account_id, &mapped_model)
            .await;

        last_email = Some(email.clone());
        info!("✓ Using account: {} (type: {})", email, config.request_type);

        // 5. Wrap the request (project injection)
        // [FIX #765] Pass session_id to wrap_request for signature injection
        // [NEW] Fetch the full Token object to inject dynamic spec limits (dynamic > static default > 65535)
        let token_obj = token_manager.get_token_by_id(&account_id);
        let wrapped_body = wrap_request_v2(
            &body,
            &project_id,
            &mapped_model,
            Some(account_id.as_str()),
            Some(&session_id),
            token_obj.as_ref(),
            Some(&token_manager),
        );

        if debug_logger::is_enabled(&debug_cfg) {
            let payload = json!({
                "kind": "v1internal_request",
                "protocol": "gemini",
                "trace_id": trace_id,
                "original_model": model_name,
                "mapped_model": mapped_model,
                "request_type": config.request_type,
                "attempt": attempt,
                "v1internal_request": wrapped_body.clone(),
            });
            debug_logger::write_debug_payload(
                &debug_cfg,
                Some(&trace_id),
                "v1internal_request",
                &payload,
            )
            .await;
        }

        // 5. Upstream call
        let query_string = if is_stream { Some("alt=sse") } else { None };
        let upstream_method = if is_stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };

        // [FIX #1522] Inject Anthropic Beta Headers for Claude models
        let mut extra_headers = std::collections::HashMap::new();
        if mapped_model.to_lowercase().contains("claude") {
            extra_headers.insert("anthropic-beta".to_string(), "claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14".to_string());
            tracing::debug!(
                "[Gemini] Injected Anthropic beta headers for Claude model: {}",
                mapped_model
            );
        }

        let call_result = match upstream
            .call_v1_internal_with_headers(
                upstream_method,
                &access_token,
                wrapped_body,
                query_string,
                extra_headers.clone(),
                Some(account_id.as_str()),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = e.clone();
                failure_statuses.record(StatusCode::BAD_GATEWAY);
                drop(image_permit.take());
                debug!(
                    "Gemini Request failed on attempt {}/{}: {}",
                    attempt + 1,
                    max_attempts,
                    e
                );
                continue;
            }
        };

        // [NEW] Log endpoint fallback to the debug file
        if !call_result.fallback_attempts.is_empty() && debug_logger::is_enabled(&debug_cfg) {
            let fallback_entries: Vec<serde_json::Value> = call_result
                .fallback_attempts
                .iter()
                .map(|a| {
                    json!({
                        "endpoint_url": a.endpoint_url,
                        "status": a.status,
                        "error": a.error,
                    })
                })
                .collect();
            let payload = json!({
                "kind": "endpoint_fallback",
                "protocol": "gemini",
                "trace_id": trace_id,
                "original_model": model_name,
                "mapped_model": mapped_model,
                "attempt": attempt,
                "account": mask_email(&email),
                "fallback_attempts": fallback_entries,
            });
            debug_logger::write_debug_payload(
                &debug_cfg,
                Some(&trace_id),
                "endpoint_fallback",
                &payload,
            )
            .await;
        }

        let response = call_result.response;
        // [NEW] Extract the actual upstream endpoint URL that was called, for logging and diagnostics
        let upstream_url = response.url().to_string();
        let status = response.status();

        // [NEW] Extract the official TraceID
        let cloud_code_trace_id = response
            .headers()
            .get("x-cloudaicompanion-trace-id")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());

        if status.is_success() {
            // 6. Response processing
            if is_stream {
                use axum::body::Body;
                use axum::response::Response;
                use bytes::{Bytes, BytesMut};
                use futures::StreamExt;

                let meta = json!({
                    "protocol": "gemini",
                    "trace_id": trace_id,
                    "original_model": model_name,
                    "mapped_model": mapped_model,
                    "request_type": config.request_type,
                    "attempt": attempt,
                    "status": status.as_u16(),
                    "upstream_url": upstream_url,
                });
                let mut response_stream = debug_logger::wrap_stream_with_debug(
                    Box::pin(response.bytes_stream()),
                    debug_cfg.clone(),
                    trace_id.clone(),
                    "upstream_response",
                    meta,
                );
                let mut buffer = BytesMut::new();
                let s_id = session_id.clone(); // Clone for stream closure

                // [FIX #859] Implement peek logic for Gemini stream to prevent 0-token 200 OK
                let mut first_chunk = None;
                let mut retry_gemini = false;

                // [NEW] Implement a two-phase timeout: phase one is FirstChunkTimeout (300s / 5min)
                // This precisely matches the official Worker's extreme patience during the model's
                // cold-start (Initialization) phase
                match tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    response_stream.next(),
                )
                .await
                {
                    Ok(Some(Ok(bytes))) => {
                        if bytes.is_empty() {
                            tracing::warn!("[Gemini] Empty first chunk received, retrying...");
                            retry_gemini = true;
                        } else {
                            first_chunk = Some(bytes);
                        }
                    }
                    Ok(Some(Err(e))) => {
                        tracing::warn!("[Gemini] Stream error during peek: {}, retrying...", e);
                        last_error = format!("Stream error: {}", e);
                        retry_gemini = true;
                    }
                    Ok(None) => {
                        tracing::warn!("[Gemini] Stream ended immediately, retrying...");
                        last_error = "Empty response".to_string();
                        retry_gemini = true;
                    }
                    Err(_) => {
                        tracing::warn!("[Gemini] First chunk timeout after 300s, retrying...");
                        last_error = "First chunk timeout".to_string();
                        retry_gemini = true;
                    }
                }

                if retry_gemini {
                    failure_statuses.record(StatusCode::BAD_GATEWAY);
                    continue;
                }
                let s_id_for_stream = s_id.clone();
                let model_name_for_stream = mapped_model.clone();
                let image_permit_for_stream = image_permit.take();
                let track_image_success = config.request_type == "image_gen";
                let image_success_manager = token_manager.clone();
                let image_success_account = account_id.clone();
                let image_success_model = mapped_model.clone();
                let stream = async_stream::stream! {
                    let _image_permit = image_permit_for_stream;
                    let mut first_data = first_chunk;
                    let mut meta_sent = false;
                    let mut saw_image_data = false;
                    let mut stream_failed = false;

                    loop {
                        // [NEW] Phase 6.2: forward the __cloudCodeMeta response metadata
                        // The official Worker sends the TraceID as the 0th packet in the SSE stream
                        if !meta_sent {
                            if let Some(tid) = &cloud_code_trace_id {
                                let meta_pkg = serde_json::json!({
                                    "__cloudCodeMeta": {
                                        "traceId": tid
                                    }
                                });
                                yield Ok::<Bytes, String>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&meta_pkg).unwrap())));
                            }
                            meta_sent = true;
                        }

                        let item = if let Some(fd) = first_data.take() {
                            Some(Ok(fd))
                        } else {
                            // [NEW] Phase two is StreamIdleTimeout (300s / 5min)
                            match tokio::time::timeout(std::time::Duration::from_secs(300), response_stream.next()).await {
                                Ok(next_item) => next_item,
                                Err(_) => {
                                    error!("[Gemini-SSE] Idle timeout after 300s, terminating stream");
                                    stream_failed = true;
                                    None
                                }
                            }
                        };

                        let bytes = match item {
                            Some(Ok(b)) => b,
                            Some(Err(e)) => {
                                error!("[Gemini-SSE] Stream error: {}", e);
                                stream_failed = true;
                                let error_json = serde_json::json!({
                                    "id": &s_id_for_stream,
                                    "object": "chat.completion.chunk",
                                    "model": &model_name_for_stream,
                                    "choices": [
                                        {
                                            "index": 0,
                                            "delta": {
                                                "content": format!("\n[Stream Error] {}", e)
                                            },
                                            "finish_reason": "error"
                                        }
                                    ]
                                });
                                yield Ok::<Bytes, String>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&error_json).unwrap_or_default())));
                                yield Ok::<Bytes, String>(Bytes::from("data: [DONE]\n\n"));
                                break;
                            }
                            None => break,
                        };

                        debug!("[Gemini-SSE] Received chunk: {} bytes", bytes.len());
                        buffer.extend_from_slice(&bytes);
                        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line_raw = buffer.split_to(pos + 1);
                            if let Ok(line_str) = std::str::from_utf8(&line_raw) {
                                let line = line_str.trim();
                                if line.is_empty() { continue; }

                                if line.starts_with("data: ") {
                                    let json_part = line.trim_start_matches("data: ").trim();
                                    if json_part == "[DONE]" {
                                        yield Ok::<Bytes, String>(Bytes::from("data: [DONE]\n\n"));
                                        continue;
                                    }

                                    match serde_json::from_str::<Value>(json_part) {
                                        Ok(mut json) => {
                                            if track_image_success && response_has_inline_image_data(&json) {
                                                saw_image_data = true;
                                            }
                                            // [FIX #765] Extract thoughtSignature from stream
                                            let inner_val = if json.get("response").is_some() {
                                                json.get("response")
                                            } else {
                                                Some(&json)
                                            };

                                            if let Some(resp) = inner_val {
                                                if let Some(candidates) = resp.get("candidates").and_then(|c| c.as_array()) {
                                                    for cand in candidates {
                                                        if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                                                            for part in parts {
                                                                if let Some(sig) = part.get("thoughtSignature").and_then(|s| s.as_str()) {
                                                                    crate::proxy::SignatureCache::global()
                                                                        .cache_session_signature(&s_id_for_stream, sig.to_string(), 1);
                                                                    debug!("[Gemini-SSE] Cached signature (len: {}) for session: {}", sig.len(), s_id_for_stream);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            // [FIX #1522] Inject Tool ID into Stream Response
                                            crate::proxy::mappers::gemini::wrapper::inject_ids_to_response(&mut json, &model_name_for_stream);

                                            // Unwrap v1internal response wrapper
                                            if let Some(inner) = json.get_mut("response").map(|v| v.take()) {
                                                let new_line = format!("data: {}\n\n", serde_json::to_string(&inner).unwrap_or_default());
                                                yield Ok::<Bytes, String>(Bytes::from(new_line));
                                            } else {
                                                yield Ok::<Bytes, String>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&json).unwrap_or_default())));
                                            }
                                        }
                                        Err(e) => {
                                            debug!("[Gemini-SSE] JSON parse error: {}, passing raw line", e);
                                            stream_failed = true;
                                            yield Ok::<Bytes, String>(Bytes::from(format!("{}\n\n", line)));
                                        }
                                    }
                                } else {
                                    // Non-data lines (comments, etc.)
                                    yield Ok::<Bytes, String>(Bytes::from(format!("{}\n\n", line)));
                                }
                            } else {
                                // Non-UTF8 data? Just pass it through or skip
                                debug!("[Gemini-SSE] Non-UTF8 line encountered");
                                yield Ok::<Bytes, String>(line_raw.freeze());
                            }
                        }
                    }

                    if track_image_success && saw_image_data && !stream_failed {
                        image_success_manager.mark_account_success(&image_success_account);
                        image_success_manager
                            .clear_persisted_live_limit(
                                &image_success_account,
                                Some(&image_success_model),
                            );
                    }
                };

                if client_wants_stream {
                    let body = Body::from_stream(stream);
                    return Ok(Response::builder()
                        .header("Content-Type", "text/event-stream")
                        .header("Cache-Control", "no-cache")
                        .header("Connection", "keep-alive")
                        .header("X-Accel-Buffering", "no")
                        .header("X-Account-Email", &email)
                        .header("X-Mapped-Model", &mapped_model)
                        .body(body)
                        .unwrap()
                        .into_response());
                } else {
                    // Collect to JSON
                    use crate::proxy::mappers::gemini::collector::collect_stream_to_json;
                    match collect_stream_to_json(Box::pin(stream), &s_id).await {
                        Ok(gemini_resp) => {
                            info!(
                                "[{}] ✓ Stream collected and converted to JSON (Gemini)",
                                session_id
                            );
                            let unwrapped = unwrap_response(&gemini_resp);
                            return Ok((
                                StatusCode::OK,
                                [
                                    ("X-Account-Email", email.as_str()),
                                    ("X-Mapped-Model", mapped_model.as_str()),
                                ],
                                Json(unwrapped),
                            )
                                .into_response());
                        }
                        Err(e) => {
                            error!("Stream collection error: {}", e);
                            return Ok((
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Stream collection error: {}", e),
                            )
                                .into_response());
                        }
                    }
                }
            }

            let mut gemini_resp: Value = response
                .json()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Parse error: {}", e)))?;

            // [FIX #1522] Inject Tool ID into Non-streaming Response
            crate::proxy::mappers::gemini::wrapper::inject_ids_to_response(
                &mut gemini_resp,
                &mapped_model,
            );

            // [FIX #765] Extract thoughtSignature from non-streaming response
            let inner_val = if gemini_resp.get("response").is_some() {
                gemini_resp.get("response")
            } else {
                Some(&gemini_resp)
            };

            if let Some(resp) = inner_val {
                if let Some(candidates) = resp.get("candidates").and_then(|c| c.as_array()) {
                    for cand in candidates {
                        if let Some(parts) = cand
                            .get("content")
                            .and_then(|c| c.get("parts"))
                            .and_then(|p| p.as_array())
                        {
                            for part in parts {
                                if let Some(sig) =
                                    part.get("thoughtSignature").and_then(|s| s.as_str())
                                {
                                    crate::proxy::SignatureCache::global().cache_session_signature(
                                        &session_id,
                                        sig.to_string(),
                                        1,
                                    );
                                    debug!("[Gemini-Response] Cached signature (len: {}) for session: {}", sig.len(), session_id);
                                }
                            }
                        }
                    }
                }
            }

            let unwrapped = unwrap_response(&gemini_resp);
            return Ok((
                StatusCode::OK,
                [
                    ("X-Account-Email", email.as_str()),
                    ("X-Mapped-Model", mapped_model.as_str()),
                ],
                Json(unwrapped),
            )
                .into_response());
        }

        // Handle the error and retry
        failure_statuses.record(status);
        let status_code = status.as_u16();
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|header| header.to_str().ok())
            .map(str::to_string);
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| format!("HTTP {}", status_code));
        last_error = format!("HTTP {}: {}", status_code, error_text);
        if debug_logger::is_enabled(&debug_cfg) {
            let payload = json!({
                "kind": "upstream_response_error",
                "protocol": "gemini",
                "trace_id": trace_id,
                "original_model": model_name,
                "mapped_model": mapped_model,
                "request_type": config.request_type,
                "attempt": attempt,
                "status": status_code,
                "upstream_url": upstream_url,
                "account": mask_email(&email),
                "error_text": error_text,
            });
            debug_logger::write_debug_payload(
                &debug_cfg,
                Some(&trace_id),
                "upstream_response_error",
                &payload,
            )
            .await;
        }

        // [FIX] On 403, check for VALIDATION_REQUIRED first and set the is_forbidden / validation_block
        // state, to ensure the URL is extracted and the UI updated promptly
        if status_code == 403 {
            if let Some(acc_id) = token_manager.get_account_id_by_email(&email) {
                if error_text.contains("VALIDATION_REQUIRED")
                    || error_text.contains("verify your account")
                    || error_text.contains("Verify your account")
                    || error_text.contains("validation_url")
                {
                    tracing::warn!(
                        "[Gemini] VALIDATION_REQUIRED detected on account {}, temporarily blocking",
                        email
                    );
                    let block_minutes = 10i64;
                    let block_until = chrono::Utc::now().timestamp() + (block_minutes * 60);

                    if let Err(e) = token_manager
                        .set_validation_block_public(&acc_id, block_until, &error_text)
                        .await
                    {
                        tracing::error!("Failed to set validation block: {}", e);
                    }
                }

                // Set the is_forbidden status and persist it
                if let Err(e) = token_manager.set_forbidden(&acc_id, &error_text).await {
                    tracing::error!("Failed to set forbidden status: {}", e);
                }
            }
        }

        // Determine the retry strategy
        let strategy = retry_state.determine_strategy(
            &account_id,
            status_code,
            &error_text,
            retry_after.as_deref(),
            false,
        );
        let needs_quota_refresh = if config.request_type == "image_gen" && status_code == 429 {
            token_manager
                .mark_rate_limited_fast(
                    &email,
                    status_code,
                    retry_after.as_deref(),
                    &error_text,
                    Some(&mapped_model),
                )
                .await
        } else {
            false
        };
        if !matches!(&strategy, RetryStrategy::GraceRetry(_)) {
            drop(image_permit.take());
        }
        if needs_quota_refresh {
            token_manager
                .refresh_quota_lock_after_fast_mark(&email, Some(&mapped_model))
                .await;
        }
        let trace_id = format!("gemini_{}", session_id);

        // Execute the backoff
        if apply_retry_strategy(
            strategy.clone(),
            attempt,
            max_attempts,
            status_code,
            &trace_id,
        )
        .await
        {
            if matches!(strategy, RetryStrategy::GraceRetry(_)) {
                retry_credentials = Some((
                    access_token.clone(),
                    project_id.clone(),
                    email.clone(),
                    account_id.clone(),
                    0,
                ));
            }
            // [NEW] Apply Client Adapter "let_it_crash" strategy
            if let Some(adapter) = &client_adapter {
                if adapter.let_it_crash() && attempt > 0 {
                    tracing::warn!(
                        "[Gemini] let_it_crash active: Aborting retries after attempt {}",
                        attempt
                    );
                    break;
                }
            }

            // Determine whether an account rotation is needed
            if !should_rotate_account(status_code, Some(&strategy)) {
                debug!(
                "[{}] Keeping same account for status {} (Gemini server-side issue or Grace Retry)",
                trace_id, status_code
            );
            }

            continue;
        }

        // [NEW] Handle a 400 error (Thinking signature invalidated)
        if status_code == 400
            && (error_text.contains("Invalid `signature`")
                || error_text.contains("thinking.signature")
                || error_text.contains("Invalid signature")
                || error_text.contains("Corrupted thought signature"))
        {
            tracing::warn!(
                "[Gemini] Signature error detected on account {}, retrying without thinking",
                email
            );

            // Append the repair prompt to the last content entry in the request body
            if let Some(contents) = body.get_mut("contents").and_then(|v| v.as_array_mut()) {
                if let Some(last_content) = contents.last_mut() {
                    if let Some(parts) =
                        last_content.get_mut("parts").and_then(|v| v.as_array_mut())
                    {
                        parts.push(json!({
                            "text": "\n\n[System Recovery] Your previous output contained an invalid signature. Please regenerate the response without the corrupted signature block."
                        }));
                        tracing::debug!("[Gemini] Appended repair prompt to last content");
                    }
                }
            }

            continue; // Retry
        }

        // HTTP exceptions like 404 caused by model config or path errors are reported directly,
        // without a pointless account rotation
        error!(
            "Gemini Upstream non-retryable error {}: {}",
            status_code, error_text
        );
        return Ok((
            status,
            [
                ("X-Account-Email", email.as_str()),
                ("X-Mapped-Model", mapped_model.as_str()),
            ],
            // [FIX] Return JSON error
            Json(json!({
                "error": {
                    "code": status_code,
                    "message": error_text,
                    "status": "UPSTREAM_ERROR"
                }
            })),
        )
            .into_response());
    }

    // All attempts failed: return 429 only if every structured failure status was 429
    let final_status = failure_statuses.final_status();

    if let Some(email) = last_email {
        Ok((
            final_status,
            [("X-Account-Email", email)],
            format!("All accounts exhausted. Last error: {}", last_error),
        )
            .into_response())
    } else {
        Ok((
            final_status,
            format!("All accounts exhausted. Last error: {}", last_error),
        )
            .into_response())
    }
}

pub async fn handle_list_models(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    use crate::proxy::common::model_mapping::get_all_dynamic_models;

    // Fetch the full dynamic model list (consistent with /v1/models)
    let only_raw = *state.only_raw_quota_models.read().await;
    let model_ids =
        get_all_dynamic_models(&state.custom_mapping, Some(&state.token_manager), only_raw).await;

    // Convert to Gemini API format
    let models: Vec<_> = model_ids
        .into_iter()
        .map(|id| {
            json!({
                "name": format!("models/{}", id),
                "version": "001",
                "displayName": id.clone(),
                "description": "",
                "inputTokenLimit": 128000,
                "outputTokenLimit": 8192,
                "supportedGenerationMethods": ["generateContent", "countTokens"],
                "temperature": 1.0,
                "topP": 0.95,
                "topK": 64
            })
        })
        .collect();

    Ok(Json(json!({ "models": models })))
}

pub async fn handle_get_model(Path(model_name): Path<String>) -> impl IntoResponse {
    Json(json!({
        "name": format!("models/{}", model_name),
        "displayName": model_name
    }))
}

/// Handle the /countTokens slash-syntax route
/// Delegates to execute_count_tokens, sharing the same implementation with the :countTokens colon syntax
pub async fn handle_count_tokens(
    State(state): State<AppState>,
    Path(model_name): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    match execute_count_tokens(state, model_name, body).await {
        Ok(resp) => resp,
        Err((status, msg)) => (status, Json(json!({ "error": msg }))).into_response(),
    }
}

/// Core countTokens implementation: transparently proxies to upstream v1internal:countTokens
///
/// Obtains a valid OAuth token, wraps the standard Gemini request body into v1internal format
/// before forwarding, and returns the real token count instead of a hardcoded 0
pub async fn execute_count_tokens(
    state: AppState,
    model_name: String,
    body: Value,
) -> Result<Response, (StatusCode, String)> {
    // 1. Model route resolution
    let mapped_model = crate::proxy::common::model_mapping::resolve_model_route(
        &model_name,
        &*state.custom_mapping.read().await,
    );

    // 2. Resolve the request config and obtain a token
    let config = crate::proxy::mappers::common_utils::resolve_request_config(
        &model_name,
        &mapped_model,
        &None,
        None,
        None,
        None,
        Some(&body),
    );

    let session_id = SessionManager::extract_gemini_session_id(&body, &model_name);

    let (access_token, _project_id, email, account_id, _wait_ms) = state
        .token_manager
        .get_token(
            &config.request_type,
            false,
            Some(&session_id),
            &config.final_model,
        )
        .await
        .map_err(|e| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Token error: {}", e),
            )
        })?;

    // 3. Wrap into v1internal format
    // [Verified] countTokens differs from generateContent: only the "request" key is allowed at
    // the top level; carrying model/project gets rejected upstream with a 400 (Unknown name
    // "model"/"project"); safetySettings inside request is likewise not accepted (matching
    // CLIProxyAPI's handling)
    let mut inner_body = body;
    if let Some(obj) = inner_body.as_object_mut() {
        obj.remove("safetySettings");
    }
    let wrapped_body = json!({
        "request": inner_body,
    });

    // 4. Call upstream v1internal:countTokens
    let call_result = state
        .upstream
        .call_v1_internal_with_headers(
            "countTokens",
            &access_token,
            wrapped_body,
            None,
            std::collections::HashMap::new(),
            Some(account_id.as_str()),
        )
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("Upstream call error: {}", e),
            )
        })?;

    let response = call_result.response;
    let status = response.status();

    if !status.is_success() {
        let err_text = response.text().await.unwrap_or_default();
        return Err((status, format!("Upstream countTokens error: {}", err_text)));
    }

    let gemini_resp: Value = response
        .json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Parse error: {}", e)))?;

    // 5. Extract totalTokens (compatible with both wrapped and unwrapped response formats)
    let total_tokens = gemini_resp
        .get("response")
        .and_then(|r| r.get("totalTokens"))
        .or_else(|| gemini_resp.get("totalTokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // 6. Return the standard Gemini REST response
    Ok((
        StatusCode::OK,
        [
            ("X-Account-Email", email.as_str()),
            ("X-Mapped-Model", mapped_model.as_str()),
        ],
        Json(json!({ "totalTokens": total_tokens })),
    )
        .into_response())
}
