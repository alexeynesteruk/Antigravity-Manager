//! Provider authentication and account restrictions stop before account rotation.
//! Rate limits and model-specific permissions deliberately stay outside this policy.

use super::rotation::RequestRetryState;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAttention {
    AuthenticationChallenge,
    AccountRestricted,
    CredentialsRejected,
}

impl AccountAttention {
    pub fn classify_stream_event(line: &[u8]) -> Option<(Self, String)> {
        let (status, body) = Self::stream_account_error(line)?;
        Self::classify(status, &body).map(|attention| (attention, body))
    }

    /// Inspect only provider error objects, never generated candidates or text.
    fn stream_account_error(line: &[u8]) -> Option<(u16, String)> {
        let text = std::str::from_utf8(line).ok()?.trim();
        let text = text.strip_prefix("data:").unwrap_or(text).trim();
        let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
        fn inspect(value: &serde_json::Value, depth: usize) -> Option<(u16, String)> {
            if depth > 4 {
                return None;
            }
            if let Some(items) = value.as_array() {
                return items.iter().find_map(|item| inspect(item, depth + 1));
            }
            let error = value
                .get("error")
                .or_else(|| value.pointer("/response/error"))?;
            let status = error
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .and_then(|code| u16::try_from(code).ok())
                .unwrap_or_else(|| {
                    match error
                        .get("status")
                        .or_else(|| error.get("type"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                    {
                        "UNAUTHENTICATED" | "authentication_error" => 401,
                        "INVALID_ARGUMENT" | "invalid_request_error" => 400,
                        "RESOURCE_EXHAUSTED" | "rate_limit_error" => 429,
                        "INTERNAL" | "api_error" => 500,
                        "UNAVAILABLE" => 503,
                        "overloaded_error" => 529,
                        _ => 403,
                    }
                });
            let body = serde_json::json!({ "error": error }).to_string();
            if status == 401 || AccountAttention::classify(status, &body).is_some() {
                Some((status, body))
            } else {
                None
            }
        }
        inspect(&value, 0)
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::AuthenticationChallenge => "AUTHENTICATION_CHALLENGE",
            Self::AccountRestricted => "ACCOUNT_RESTRICTED",
            Self::CredentialsRejected => "CREDENTIALS_REJECTED",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::AuthenticationChallenge => "The provider requires account verification. This request stopped and the account is paused. Open Accounts, complete verification with the provider, then explicitly recover the account before retrying.",
            Self::AccountRestricted => "The provider explicitly restricted this account. This request stopped and the account is paused. Review the restriction in Accounts and resolve it with the provider before explicitly recovering the account.",
            Self::CredentialsRejected => "The provider rejected this account's credentials. This request stopped and the account is paused. Reauthorize the account in Accounts, then explicitly recover it before retrying.",
        }
    }

    pub fn classify(status: u16, body: &str) -> Option<Self> {
        if !matches!(status, 401 | 403) {
            return None;
        }
        if crate::models::account::AccountBanInfo::classify_upstream(status, body).is_some() {
            return Some(Self::AccountRestricted);
        }

        let (_, parsed) = super::errors::parse_raw_upstream_error(body);
        let error = parsed.get("error").unwrap_or(&parsed);
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or(body)
            .to_ascii_lowercase();
        let has_validation_url = error
            .get("details")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .any(|detail| {
                detail
                    .pointer("/metadata/validation_url")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|url| url.starts_with("https://"))
            });
        let reasons = error
            .get("details")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|detail| detail.get("reason").and_then(serde_json::Value::as_str));
        let explicit_challenge = reasons
            .chain(
                ["reason", "error", "code"]
                    .into_iter()
                    .filter_map(|key| error.get(key).and_then(serde_json::Value::as_str)),
            )
            .any(|reason| {
                matches!(
                    reason.to_ascii_uppercase().as_str(),
                    "VALIDATION_REQUIRED"
                        | "REAUTH_REQUIRED"
                        | "REAUTHENTICATION_REQUIRED"
                        | "INTERACTION_REQUIRED"
                        | "ACCOUNT_VERIFICATION_REQUIRED"
                        | "CAPTCHA_REQUIRED"
                        | "SUSPICIOUS_ACTIVITY"
                )
            });
        if explicit_challenge
            || has_validation_url
            || [
                "validation_required",
                "validation required",
                "verify your account",
                "verify it's you",
                "additional verification is required",
                "reauthentication is required",
                "further action is required",
                "confirm you are not a robot",
            ]
            .iter()
            .any(|cue| message.contains(cue))
        {
            return Some(Self::AuthenticationChallenge);
        }
        None
    }

    pub fn classify_refresh_failure(body: &str) -> Option<Self> {
        let lower = body.to_ascii_lowercase();
        if [
            "invalid_grant",
            "invalid_rapt",
            "reauth_required",
            "interaction_required",
            "consent_required",
        ]
        .iter()
        .any(|code| lower.contains(code))
        {
            Some(Self::CredentialsRejected)
        } else {
            Self::classify(401, body)
        }
    }
}

pub enum AccountSafetyAction {
    Continue,
    RetrySame(String),
    Stop(AccountSafetyStop),
}

pub struct AccountSafetyStop {
    pub account_id: String,
    pub upstream_status: u16,
    pub attention: Option<crate::proxy::pipeline::account_safety::AccountAttention>,
    pub hold_persisted: bool,
    pub inspection_incomplete: bool,
}

impl AccountSafetyStop {
    pub fn body(&self) -> Value {
        let message = if self.inspection_incomplete {
            "The provider returned an error larger than the bounded account-safety inspection limit. This request stopped without trying another account. Review account status before retrying explicitly."
        } else {
            self.attention.map(|attention| attention.message()).unwrap_or(
            "Provider authentication could not be refreshed within this request. The request stopped without trying other accounts. Check account authorization and retry explicitly.",
        )
        };
        json!({"error": {
            "type": "account_attention_required",
            "code": if self.inspection_incomplete { "UPSTREAM_ACCOUNT_ERROR_TOO_LARGE" } else { self.attention.map(|attention| attention.code()).unwrap_or("AUTHENTICATION_REFRESH_FAILED") },
            "message": message,
            "account_id": self.account_id,
            "upstream_status": self.upstream_status,
            "manual_recovery_required": self.attention.is_some(),
            "account_hold_persisted": self.hold_persisted,
        }})
    }

    pub fn response(self) -> Response {
        // 400 is intentionally non-retryable and does not confuse a provider
        // challenge with authentication to this local gateway (notably Claude).
        (
            StatusCode::BAD_REQUEST,
            [
                ("X-Account-Attention", "required"),
                ("Cache-Control", "no-store"),
            ],
            Json(self.body()),
        )
            .into_response()
    }

    pub fn encoded(self) -> String {
        self.body().to_string()
    }
}

pub fn is_account_attention_error(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|body| {
            body.pointer("/error/type")
                .and_then(Value::as_str)
                .map(|kind| kind == "account_attention_required")
        })
        .unwrap_or(false)
}

pub fn account_attention_payload(body: &Value) -> Option<Value> {
    let error = body
        .get("error")
        .or_else(|| body.pointer("/response/error"))
        .unwrap_or(body);
    if let Some(admission) =
        super::admission::error_from_encoded(&json!({"error":error}).to_string())
    {
        return Some(admission.body());
    }
    if [error.get("type"), error.get("code")]
        .into_iter()
        .flatten()
        .any(|value| value.as_str() == Some("gateway_request_limit"))
    {
        let retry_after = error
            .get("retry_after")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1);
        let code =
            if error.get("gateway_code").and_then(Value::as_str) == Some("DAILY_REQUEST_BUDGET") {
                "DAILY_REQUEST_BUDGET"
            } else {
                "REQUEST_RATE_LIMIT"
            };
        return Some(super::admission::AdmissionError { code, retry_after }.body());
    }
    if [error.get("type"), error.get("code")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .all(|kind| kind != "account_attention_required")
    {
        return None;
    }
    Some(json!({"error": {
        "type": "account_attention_required", "code": "account_attention_required",
        "message": error.get("message").and_then(Value::as_str).unwrap_or("Provider account requires attention. Review Accounts before retrying explicitly."),
    }}))
}

pub fn account_attention_error_response(body: &str) -> Option<Response> {
    let body = serde_json::from_str::<Value>(body).ok()?;
    let normalized = account_attention_payload(&body)?;
    if let Some(error) = super::admission::error_from_encoded(&normalized.to_string()) {
        return Some(error.response());
    }
    Some(
        (
            StatusCode::BAD_REQUEST,
            [
                ("X-Account-Attention", "required"),
                ("Cache-Control", "no-store"),
            ],
            Json(normalized),
        )
            .into_response(),
    )
}

pub fn account_attention_stream_response(bytes: &[u8]) -> Option<Response> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("data:"))
        .find_map(|data| account_attention_error_response(data.trim()))
}

/// Raw Gemini stream guard shared by all adapters, including Gemini's immediate
/// response headers. Inspect bounded complete lines so split error events work.
pub fn guard_upstream_account_stream<S, E>(
    stream: std::pin::Pin<Box<S>>,
    token_manager: std::sync::Arc<crate::proxy::token_manager::TokenManager>,
    account_id: String,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, String>> + Send>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Send + 'static + ?Sized,
    E: std::fmt::Display + Send + 'static,
{
    use futures::StreamExt;
    let guarded = async_stream::stream! {
        const MAX_ERROR_LINE: usize = 64 * 1024;
        let mut stream = stream;
        let mut pending = bytes::BytesMut::new();
        let mut oversized_line = false;
        while let Some(item) = stream.next().await {
            let bytes = match item { Ok(bytes) => bytes, Err(error) => { yield Err(error.to_string()); return; } };
            let mut offset = 0;
            while offset < bytes.len() {
                let end = bytes[offset..].iter().position(|byte| *byte == b'\n').map(|end| offset + end + 1).unwrap_or(bytes.len());
                let segment = bytes.slice(offset..end);
                offset = end;
                let complete = segment.last() == Some(&b'\n');
                if oversized_line || pending.len() + segment.len() > MAX_ERROR_LINE {
                    if !oversized_line {
                        let mut prefix = Vec::with_capacity(MAX_ERROR_LINE);
                        prefix.extend_from_slice(&pending[..pending.len().min(MAX_ERROR_LINE)]);
                        let remaining = MAX_ERROR_LINE.saturating_sub(prefix.len());
                        prefix.extend_from_slice(&segment[..segment.len().min(remaining)]);
                        if looks_like_provider_error_line(&prefix) {
                            yield Err(AccountSafetyStop { account_id: account_id.clone(), upstream_status: 400, attention: None, hold_persisted: false, inspection_incomplete: true }.encoded());
                            return;
                        }
                    }
                    if !pending.is_empty() { yield Ok(pending.split().freeze()); }
                    yield Ok(segment);
                    oversized_line = !complete;
                    continue;
                }
                pending.extend_from_slice(&segment);
                if complete {
                    let line = pending.split().freeze();
                    if let Some((status, body)) = AccountAttention::stream_account_error(&line) {
                        let attention = AccountAttention::classify(status, &body);
                        let saved = if let Some(attention) = attention { token_manager.hold_account_for_attention(&account_id, attention, &body).await.is_ok() } else { false };
                        yield Err(AccountSafetyStop { account_id: account_id.clone(), upstream_status: status, attention, hold_persisted: saved, inspection_incomplete: false }.encoded());
                        return;
                    }
                    yield Ok(line);
                }
            }
        }
        if !pending.is_empty() {
            if let Some((status, body)) = AccountAttention::stream_account_error(&pending) {
                let attention = AccountAttention::classify(status, &body);
                let saved = if let Some(attention) = attention { token_manager.hold_account_for_attention(&account_id, attention, &body).await.is_ok() } else { false };
                yield Err(AccountSafetyStop { account_id, upstream_status: status, attention, hold_persisted: saved, inspection_incomplete: false }.encoded());
            } else { yield Ok(pending.freeze()); }
        }
    };
    Box::pin(guarded)
}

fn looks_like_provider_error_line(prefix: &[u8]) -> bool {
    // Inspect structural keys, not substrings: metadata may precede an error,
    // while model text and tool arguments may themselves mention "error".
    // An error after a truncated/oversized preceding metadata value is outside
    // this bounded inspection; never grow the buffer to search indefinitely.
    const MAX_PREFIX: usize = 64 * 1024;
    fn whitespace(bytes: &[u8], position: &mut usize) {
        while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
            *position += 1;
        }
    }
    fn string(bytes: &[u8], position: &mut usize) -> Option<()> {
        if bytes.get(*position) != Some(&b'"') {
            return None;
        }
        *position += 1;
        while let Some(byte) = bytes.get(*position) {
            *position += 1;
            match byte {
                b'"' => return Some(()),
                b'\\' => {
                    bytes.get(*position)?;
                    *position += 1;
                }
                _ => {}
            }
        }
        None
    }
    fn skip_value(bytes: &[u8], position: &mut usize) -> Option<()> {
        whitespace(bytes, position);
        match bytes.get(*position)? {
            b'"' => string(bytes, position),
            b'{' | b'[' => {
                let mut stack = Vec::new();
                while let Some(byte) = bytes.get(*position) {
                    match byte {
                        b'"' => {
                            string(bytes, position)?;
                            continue;
                        }
                        b'{' => stack.push(b'}'),
                        b'[' => stack.push(b']'),
                        b'}' | b']' => {
                            if stack.pop() != Some(*byte) {
                                return None;
                            }
                            *position += 1;
                            if stack.is_empty() {
                                return Some(());
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if stack.len() > 64 {
                        return None;
                    }
                    *position += 1;
                }
                None
            }
            _ => {
                let start = *position;
                while bytes.get(*position).is_some_and(|byte| {
                    !byte.is_ascii_whitespace() && !matches!(byte, b',' | b'}' | b']')
                }) {
                    *position += 1;
                }
                (*position > start).then_some(())
            }
        }
    }
    fn object(bytes: &[u8], position: &mut usize, root: bool) -> Option<bool> {
        if bytes.get(*position) != Some(&b'{') {
            return None;
        }
        *position += 1;
        whitespace(bytes, position);
        if bytes.get(*position) == Some(&b'}') {
            *position += 1;
            return Some(false);
        }
        loop {
            whitespace(bytes, position);
            let start = *position;
            string(bytes, position)?;
            // Decode complete JSON keys, including valid Unicode escapes.
            let key = serde_json::from_slice::<String>(&bytes[start..*position]).ok()?;
            whitespace(bytes, position);
            if bytes.get(*position) != Some(&b':') {
                return None;
            }
            *position += 1;
            whitespace(bytes, position);
            if key == "error" && matches!(bytes.get(*position), Some(b'{' | b'"' | b'[')) {
                return Some(true);
            }
            if root && key == "response" && bytes.get(*position) == Some(&b'{') {
                if object(bytes, position, false)? {
                    return Some(true);
                }
            } else {
                skip_value(bytes, position)?;
            }
            whitespace(bytes, position);
            match bytes.get(*position)? {
                b',' => *position += 1,
                b'}' => {
                    *position += 1;
                    return Some(false);
                }
                _ => return None,
            }
        }
    }
    let bytes = &prefix[..prefix.len().min(MAX_PREFIX)];
    let mut position = 0;
    whitespace(bytes, &mut position);
    if bytes
        .get(position..)
        .is_some_and(|bytes| bytes.starts_with(b"data:"))
    {
        position += 5;
        whitespace(bytes, &mut position);
    }
    object(bytes, &mut position, true).unwrap_or(false)
}

/// Shared execution hook; adapters only supply the real provider status/body.
pub async fn handle_upstream_account_safety(
    token_manager: &crate::proxy::token_manager::TokenManager,
    retry_state: &mut RequestRetryState,
    account_id: &str,
    rejected_access_token: &str,
    status: u16,
    body: &str,
) -> AccountSafetyAction {
    use crate::proxy::pipeline::account_safety::AccountAttention;
    let mut attention = AccountAttention::classify(status, body);
    if attention.is_none() && status != 401 {
        return AccountSafetyAction::Continue;
    }
    if attention.is_none() && retry_state.begin_auth_refresh() {
        let remaining = crate::proxy::pipeline::budget::remaining_secs()
            .unwrap_or(15)
            .min(15);
        if remaining > 0 {
            match token_manager
                .refresh_account_token_after_rejection(
                    account_id,
                    rejected_access_token,
                    std::time::Duration::from_secs(remaining),
                )
                .await
            {
                Ok(token) if token.access_token != rejected_access_token => {
                    return AccountSafetyAction::RetrySame(token.access_token);
                }
                Ok(_) => {
                    attention = Some(AccountAttention::CredentialsRejected);
                }
                Err(error) => {
                    attention = AccountAttention::classify_refresh_failure(&error);
                }
            }
        }
    } else if attention.is_none() {
        attention = Some(AccountAttention::CredentialsRejected);
    }
    let hold_persisted = if let Some(attention) = attention {
        let saved = token_manager
            .hold_account_for_attention(account_id, attention, body)
            .await
            .is_ok();
        tracing::warn!(
            account_id,
            code = attention.code(),
            hold_persisted = saved,
            "Provider account requires manual recovery; request stopped"
        );
        saved
    } else {
        false
    };
    AccountSafetyAction::Stop(AccountSafetyStop {
        account_id: account_id.to_string(),
        upstream_status: status,
        attention,
        hold_persisted,
        inspection_incomplete: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::pipeline::rotation::{
        determine_retry_strategy_adaptive, should_rotate_account, RetryStrategy,
    };

    #[tokio::test]
    async fn attention_survives_real_claude_chat_and_responses_conversion_and_peek() {
        use crate::proxy::pipeline::stream_peek::{peek_first_data, PeekOutcome, StreamDialect};
        use futures::StreamExt;
        for protocol in ["claude", "chat", "responses"] {
            let root = tempfile::tempdir().unwrap();
            let manager = std::sync::Arc::new(crate::proxy::token_manager::TokenManager::new(
                root.path().to_path_buf(),
            ));
            let upstream = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from_static(
                b"data: {\"error\":{\"code\":403,\"message\":\"Verify your account\"}}\n\n",
            ))]);
            let guarded = guard_upstream_account_stream(
                Box::pin(upstream),
                manager,
                "attention-test".to_string(),
            );
            let mut converted = match protocol {
                "claude" => crate::proxy::mappers::claude::create_claude_sse_stream(
                    guarded,
                    "attention-test".to_string(),
                    "account@example.invalid".to_string(),
                    None,
                    false,
                    200_000,
                    Some(10),
                    0,
                    None,
                    vec![],
                ),
                "chat" => {
                    crate::proxy::mappers::openai::streaming::create_openai_sse_stream_with_anchor(
                        guarded,
                        "test-model".to_string(),
                        "attention-test".to_string(),
                        0,
                        None,
                        false,
                        None,
                    )
                }
                _ => crate::proxy::mappers::openai::streaming::create_codex_sse_stream(
                    guarded,
                    "test-model".to_string(),
                    "attention-test".to_string(),
                    0,
                    0,
                    "resp-attention-test".to_string(),
                    None,
                    false,
                    false,
                ),
            };
            let dialect = if protocol == "claude" {
                StreamDialect::Anthropic
            } else {
                StreamDialect::OpenAi
            };
            let outcome = peek_first_data(
                &mut converted,
                2,
                |bytes| crate::proxy::pipeline::stream_peek::is_heartbeat(bytes, dialect),
                |bytes| {
                    if protocol == "claude" {
                        crate::proxy::pipeline::stream_peek::has_anthropic_error_event(bytes)
                    } else {
                        crate::proxy::pipeline::stream_peek::has_openai_error_event(bytes)
                    }
                },
            )
            .await;
            if protocol == "responses" {
                // Streaming Responses returns its legitimate prelude before it
                // reads the provider body. Production returns headers here;
                // subsequent failure must remain an error + response.failed.
                let PeekOutcome::Data { bytes, .. } = outcome else {
                    panic!("Responses prelude missing: {outcome:?}");
                };
                assert!(String::from_utf8_lossy(&bytes).contains("response.created"));
                let mut wire = bytes.to_vec();
                while let Some(item) = converted.next().await {
                    wire.extend_from_slice(&item.unwrap());
                }
                let text = String::from_utf8(wire).unwrap();
                let events: Vec<Value> = text
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .filter_map(|line| serde_json::from_str(line.trim()).ok())
                    .collect();
                assert!(events.iter().any(|event| event["type"] == "error"
                    && event["code"] == "account_attention_required"));
                assert!(events.iter().any(|event| event["type"] == "response.failed"
                    && account_attention_payload(event).is_some()));
                assert!(!events
                    .iter()
                    .any(|event| event["type"] == "response.completed"));
                assert!(account_attention_stream_response(text.as_bytes()).is_some());
                continue;
            }
            let PeekOutcome::ErrorEvent { bytes, .. } = outcome else {
                panic!("{protocol}: {outcome:?}");
            };
            let response = account_attention_stream_response(&bytes)
                .expect("converted error must stop before account rotation");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{protocol}");
        }
    }

    #[tokio::test]
    async fn attention_after_partial_content_survives_real_nonstream_collectors() {
        use futures::StreamExt;
        for (protocol, thinking_only) in [
            ("claude", false),
            ("claude", true),
            ("chat", false),
            ("chat", true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let manager = std::sync::Arc::new(crate::proxy::token_manager::TokenManager::new(
                root.path().to_path_buf(),
            ));
            let first = if thinking_only {
                b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\"Thinking only\"}]}}]}\n\n".as_slice()
            } else {
                b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Partial content\"}]}}]}\n\n".as_slice()
            };
            let upstream = futures::stream::iter(vec![
                Ok::<_, String>(bytes::Bytes::copy_from_slice(first)),
                Ok(bytes::Bytes::from_static(
                    b"data: {\"error\":{\"code\":403,\"message\":\"Verify your account\"}}\n\n",
                )),
            ]);
            let guarded = guard_upstream_account_stream(
                Box::pin(upstream),
                manager,
                "attention-test".to_string(),
            );
            let result = if protocol == "claude" {
                let converted = crate::proxy::mappers::claude::create_claude_sse_stream(
                    guarded,
                    "attention-test".into(),
                    "account@example.invalid".into(),
                    None,
                    false,
                    200_000,
                    Some(10),
                    0,
                    None,
                    vec![],
                );
                let converted = converted.map(|item| item.map_err(std::io::Error::other));
                crate::proxy::mappers::claude::collect_stream_to_json(converted)
                    .await
                    .map(|_| ())
            } else {
                let converted =
                    crate::proxy::mappers::openai::streaming::create_openai_sse_stream_with_anchor(
                        guarded,
                        "test-model".into(),
                        "attention-test".into(),
                        0,
                        None,
                        false,
                        None,
                    );
                crate::proxy::mappers::openai::collector::collect_stream_to_json(converted)
                    .await
                    .map(|_| ())
            };
            let error = result.expect_err("a provider challenge must never become partial success");
            assert!(is_account_attention_error(&error), "{protocol}: {error}");
            assert_eq!(
                account_attention_error_response(&error).unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[tokio::test]
    async fn oversized_provider_error_stops_but_is_not_an_account_ban() {
        use futures::StreamExt;
        let root = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(crate::proxy::token_manager::TokenManager::new(
            root.path().to_path_buf(),
        ));
        let input = format!(
            "data: {{\"error\":{{\"message\":\"{}\"}}}}\n\n",
            "x".repeat(70_000)
        );
        let upstream = futures::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(input))]);
        let output: Vec<_> = guard_upstream_account_stream(
            Box::pin(upstream),
            manager,
            "attention-test".to_string(),
        )
        .collect()
        .await;
        let error = output[0].as_ref().unwrap_err();
        let body: Value = serde_json::from_str(error).unwrap();
        assert_eq!(body["error"]["code"], "UPSTREAM_ACCOUNT_ERROR_TOO_LARGE");
        assert_eq!(body["error"]["account_hold_persisted"], false);
    }

    #[test]
    fn authentication_refresh_is_once_per_request_and_attention_is_terminal() {
        let mut state = RequestRetryState::default();
        assert!(state.begin_auth_refresh());
        assert!(!state.begin_auth_refresh());
        assert!(!state.begin_auth_refresh());
        assert!(RequestRetryState::default().begin_auth_refresh());
        for pool in [1, 3, 50] {
            let strategy = determine_retry_strategy_adaptive(
                403,
                "VALIDATION_REQUIRED",
                None,
                false,
                true,
                0,
                pool,
            );
            assert!(matches!(strategy, RetryStrategy::NoRetry));
            assert!(!should_rotate_account(403, Some(&strategy)));
        }
    }

    #[tokio::test]
    async fn split_stream_challenge_stops_before_following_data_even_when_hold_storage_fails() {
        use futures::StreamExt;
        let root = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(crate::proxy::token_manager::TokenManager::new(
            root.path().to_path_buf(),
        ));
        let stream = futures::stream::iter(vec![
            Ok::<_, String>(bytes::Bytes::from_static(
                b": heartbeat\n\ndata: {\"error\":{\"code\":403,\"message\":\"Verify ",
            )),
            Ok(bytes::Bytes::from_static(
                b"your account fake-sensitive-value\"}}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"data: {\"candidates\":[{\"text\":\"must not continue\"}]}\n\n",
            )),
        ]);
        let output: Vec<_> =
            guard_upstream_account_stream(Box::pin(stream), manager, "attention-test".to_string())
                .collect()
                .await;
        let error = output.last().unwrap().as_ref().unwrap_err();
        assert!(is_account_attention_error(error));
        assert!(!error.contains("fake-sensitive-value"));
        assert!(!format!("{output:?}").contains("must not continue"));
        assert_eq!(output.iter().filter(|item| item.is_err()).count(), 1);
    }

    #[tokio::test]
    async fn stream_guard_passes_large_normal_lines_and_heartbeats_without_false_holds() {
        use futures::StreamExt;
        let root = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(crate::proxy::token_manager::TokenManager::new(
            root.path().to_path_buf(),
        ));
        let input = format!(
            ": heartbeat\n\ndata: {{\"candidates\":[{{\"text\":\"{}\"}}]}}\n\n",
            "verify your account ".repeat(5000)
        );
        let stream = futures::stream::iter(
            input
                .as_bytes()
                .chunks(1000)
                .map(|chunk| Ok::<_, String>(bytes::Bytes::copy_from_slice(chunk)))
                .collect::<Vec<_>>(),
        );
        let output: Vec<_> =
            guard_upstream_account_stream(Box::pin(stream), manager, "attention-test".to_string())
                .collect()
                .await;
        let bytes: Vec<_> = output
            .into_iter()
            .flat_map(|item| item.unwrap().to_vec())
            .collect();
        assert_eq!(bytes, input.as_bytes());
    }

    #[test]
    fn oversized_error_prefix_recognizes_structural_keys_after_metadata() {
        for prefix in [
            br#"data: {"requestId":"r","metadata":{"error":"not the provider error","items":[{},[]]},"error":{"code":403,"message":"Verify your account"#.as_slice(),
            br#"data: {"metadata":[{"text":"braces } and escaped \"error\" remain text"}],"response":{"responseId":"r","usage":{"tokens":1},"error":{"code":403"#,
            br#"data: {"requestId":"r","err\u006fr":{"code":403"#,
            b"\r\n data: {\"metadata\":null,\"response\":{\"id\":1,\"error\":{\"code\":403",
        ] {
            assert!(looks_like_provider_error_line(prefix), "{prefix:?}");
        }
    }

    #[test]
    fn oversized_error_prefix_ignores_model_text_nested_errors_and_truncated_keys() {
        for prefix in [
            br#"data: {"candidates":[{"content":{"parts":[{"text":"{\"error\": {\"message\":\"Verify your account\"}}"}]}}]}"#.as_slice(),
            br#"data: {"metadata":{"error":{"message":"Verify your account"}},"candidates":[]}"#,
            br#"data: {"response":{"candidates":[{"functionCall":{"args":{"error":{"code":403}}}}]}}"#,
            br#"data: {"error":null,"candidates":[{"text":"ordinary large content"}]}"#,
            br#"data: {"requestId":"truncated","err"#,
            br#"data: {"metadata":"unterminated string containing \"error\":{}"#,
        ] {
            assert!(!looks_like_provider_error_line(prefix), "{prefix:?}");
        }
        let beyond_limit = format!(
            "data: {{\"metadata\":\"{}\",\"error\":{{\"code\":403}}}}",
            "m".repeat(64 * 1024)
        );
        assert!(
            !looks_like_provider_error_line(beyond_limit.as_bytes()),
            "metadata before the error may exceed the fixed inspection window"
        );
    }

    #[tokio::test]
    async fn oversized_provider_error_after_metadata_stops_without_claiming_a_hold() {
        use futures::StreamExt;
        let root = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(crate::proxy::token_manager::TokenManager::new(
            root.path().to_path_buf(),
        ));
        let line = bytes::Bytes::from(format!("data: {{\"requestId\":\"{}\",\"response\":{{\"id\":1,\"error\":{{\"code\":403,\"message\":\"Verify your account {}\"}}}}}}\n\n", "r".repeat(60 * 1024), "m".repeat(70 * 1024)));
        let stream = futures::stream::iter(vec![
            Ok::<_, String>(line.slice(..60 * 1024)),
            Ok(line.slice(60 * 1024..)),
            Ok(bytes::Bytes::from_static(b"data: following-payload\n\n")),
        ]);
        let output: Vec<_> =
            guard_upstream_account_stream(Box::pin(stream), manager, "attention-test".into())
                .collect()
                .await;
        assert_eq!(output.len(), 1);
        let error = output[0].as_ref().unwrap_err();
        let body: Value = serde_json::from_str(error).unwrap();
        assert_eq!(body["error"]["code"], "UPSTREAM_ACCOUNT_ERROR_TOO_LARGE");
        assert_eq!(body["error"]["account_hold_persisted"], false);
        assert!(!error.contains("following-payload"));
    }

    #[test]
    fn explicit_challenges_and_account_bans_require_attention() {
        for body in [
            r#"{"error":{"message":"Verify your account","details":[{"reason":"VALIDATION_REQUIRED"}]}}"#,
            r#"{"error":{"details":[{"reason":"SUSPICIOUS_ACTIVITY"}]}}"#,
            "Further action is required to access your account",
        ] {
            assert_eq!(
                AccountAttention::classify(403, body),
                Some(AccountAttention::AuthenticationChallenge)
            );
        }
        assert_eq!(
            AccountAttention::classify(403, "Your account has been suspended"),
            Some(AccountAttention::AccountRestricted)
        );
    }

    #[test]
    fn rate_limits_server_failures_and_model_permissions_are_not_account_holds() {
        for (status, body) in [
            (429, "VALIDATION_REQUIRED"),
            (500, "account_banned"),
            (
                403,
                "Model claude-5 is temporarily suspended for this model",
            ),
            (403, "ACCESS_TOKEN_SCOPE_INSUFFICIENT"),
            (403, "The caller does not have permission"),
            (401, "Request had invalid authentication credentials"),
            (400, "The user wrote: verify your account"),
        ] {
            assert_eq!(
                AccountAttention::classify(status, body),
                None,
                "{status}: {body}"
            );
        }
    }

    #[test]
    fn only_proven_refresh_credential_failures_hold_accounts() {
        assert_eq!(
            AccountAttention::classify_refresh_failure("OAuth error: invalid_grant"),
            Some(AccountAttention::CredentialsRejected)
        );
        assert_eq!(
            AccountAttention::classify_refresh_failure("Token refresh timed out"),
            None
        );
        assert_eq!(
            AccountAttention::classify_refresh_failure("HTTP 429 Too many requests"),
            None
        );
    }

    #[test]
    fn stream_errors_require_a_provider_error_object_and_respect_actual_status() {
        assert!(AccountAttention::classify_stream_event(
            br#"data: {"error":{"code":403,"message":"Verify your account"}}"#
        )
        .is_some());
        assert!(AccountAttention::classify_stream_event(br#"data: {"response":{"error":{"status":"PERMISSION_DENIED","details":[{"reason":"VALIDATION_REQUIRED"}]}}}"#).is_some());
        for line in [
            br#"data: {"candidates":[{"content":{"parts":[{"text":"verify your account account_banned"}]}}]}"#.as_slice(),
            br#"data: {"error":{"code":429,"message":"Verify your account"}}"#,
            br#"data: {"error":{"type":"invalid_request_error","message":"Verify your account"}}"#,
            b": Verify your account",
        ] {
            assert!(AccountAttention::classify_stream_event(line).is_none());
        }
    }
}
