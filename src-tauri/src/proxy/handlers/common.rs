use crate::proxy::server::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio::time::{sleep, Duration};
use tracing::{debug, info};

// ===== Unified retry and backoff strategy =====

/// Retry strategy enum
#[derive(Debug, Clone)]
pub enum RetryStrategy {
    /// No retry, return the error directly
    NoRetry,
    /// Fixed delay
    FixedDelay(Duration),
    /// Linear backoff: base_ms * (attempt + 1)
    LinearBackoff { base_ms: u64 },
    /// Exponential backoff: base_ms * 2^attempt, capped at max_ms
    ExponentialBackoff { base_ms: u64, max_ms: u64 },
    /// [NEW] In-place retry (Grace Retry): wait a short window on the current account then
    /// retry directly, without counting toward the usual account rotation
    GraceRetry(Duration),
}

#[derive(Debug, Default)]
pub struct RequestRetryState {
    grace_retried_accounts: HashSet<String>,
}

impl RequestRetryState {
    pub fn determine_strategy(
        &mut self,
        account_id: &str,
        status_code: u16,
        error_text: &str,
        retry_after: Option<&str>,
        retried_without_thinking: bool,
    ) -> RetryStrategy {
        let allow_grace_retry = !self.grace_retried_accounts.contains(account_id);
        let strategy = determine_retry_strategy_inner(
            status_code,
            error_text,
            retry_after,
            retried_without_thinking,
            allow_grace_retry,
        );
        if matches!(strategy, RetryStrategy::GraceRetry(_)) {
            self.grace_retried_accounts.insert(account_id.to_string());
        }
        strategy
    }
}

pub fn next_rotation_attempt(
    used_attempts: &mut usize,
    max_attempts: usize,
    retry_same_account: bool,
) -> Option<usize> {
    if retry_same_account {
        return used_attempts.checked_sub(1);
    }
    if *used_attempts >= max_attempts {
        return None;
    }

    let attempt = *used_attempts;
    *used_attempts += 1;
    Some(attempt)
}

#[derive(Debug, Default)]
pub struct FailureStatusTracker {
    saw_failure: bool,
    last_non_rate_limit: Option<StatusCode>,
}

impl FailureStatusTracker {
    pub fn record(&mut self, status: StatusCode) {
        self.saw_failure = true;
        if status != StatusCode::TOO_MANY_REQUESTS {
            self.last_non_rate_limit = Some(status);
        }
    }

    pub fn final_status(&self) -> StatusCode {
        self.last_non_rate_limit.unwrap_or_else(|| {
            if self.saw_failure {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::BAD_GATEWAY
            }
        })
    }
}

/// Determine the retry strategy based on the error status code and error text
pub fn determine_retry_strategy(
    status_code: u16,
    error_text: &str,
    retried_without_thinking: bool,
) -> RetryStrategy {
    if status_code == 429 {
        return match crate::proxy::upstream::retry::parse_legacy_retry_delay(error_text) {
            Some(delay_ms) if delay_ms > 0 && delay_ms <= 2000 => {
                let actual_delay = delay_ms.saturating_add(100);
                tracing::info!(
                    "Grace Retry Triggered: Delay {}ms is within window, using same account",
                    actual_delay
                );
                RetryStrategy::GraceRetry(Duration::from_millis(actual_delay))
            }
            Some(delay_ms) => RetryStrategy::FixedDelay(Duration::from_millis(
                delay_ms.saturating_add(200).min(30_000),
            )),
            None => RetryStrategy::LinearBackoff { base_ms: 5000 },
        };
    }

    determine_retry_strategy_inner(
        status_code,
        error_text,
        None,
        retried_without_thinking,
        true,
    )
}

fn determine_retry_strategy_inner(
    status_code: u16,
    error_text: &str,
    retry_after: Option<&str>,
    retried_without_thinking: bool,
    allow_grace_retry: bool,
) -> RetryStrategy {
    // 400 signature errors must be case-insensitive and cover all Google variants.
    let lower = error_text.to_lowercase();
    match status_code {
        // 400 error: only retry once for a specific Thinking signature failure
        400 if !retried_without_thinking
            && (lower.contains("invalid thought signature")
                || lower.contains("invalid `signature`")
                || lower.contains("invalid signature")
                || lower.contains("thought_signature")
                || lower.contains("thoughtsignature")
                || lower.contains("thinking.signature")
                || lower.contains("thinking.thinking")
                || lower.contains("corrupted thought signature")) =>
        {
            RetryStrategy::FixedDelay(Duration::from_millis(200))
        }

        // 429 rate limit error
        429 => {
            // Prefer the Retry-After / quotaResetDelay returned by the server
            if let Some(parsed_delay) = crate::proxy::upstream::retry::parse_retry_delay_with_source(
                error_text,
                retry_after,
            ) {
                let delay_ms = parsed_delay.raw_ms;
                // If a short-window same-account retry has already been used, fall back to the
                // existing account-rotation logic immediately
                if crate::proxy::upstream::retry::should_grace_retry(delay_ms) {
                    if allow_grace_retry {
                        let actual_delay = parsed_delay.actual_wait_ms();
                        tracing::info!(
                            "Grace Retry Triggered: Delay {}ms is within window, using same account",
                            actual_delay
                        );
                        RetryStrategy::GraceRetry(Duration::from_millis(actual_delay))
                    } else {
                        RetryStrategy::FixedDelay(Duration::ZERO)
                    }
                } else {
                    let actual_delay = parsed_delay.actual_wait_ms().min(30_000);
                    RetryStrategy::FixedDelay(Duration::from_millis(actual_delay))
                }
            } else {
                // Otherwise use linear backoff: starting at 5s, increasing gradually
                RetryStrategy::LinearBackoff { base_ms: 5000 }
            }
        }

        // 503 service unavailable / 529 server overloaded
        503 | 529 => {
            // Exponential backoff: starting at 10s, capped at 60s (for Google edge node overload)
            RetryStrategy::ExponentialBackoff {
                base_ms: 10000,
                max_ms: 60000,
            }
        }

        // 500 internal server error
        500 => {
            // Linear backoff: starting at 3s
            RetryStrategy::LinearBackoff { base_ms: 3000 }
        }

        // 401/403 authentication/permission error: give a very short buffer before switching accounts
        401 | 403 => RetryStrategy::FixedDelay(Duration::from_millis(200)),

        // 404 resource not found: a 404 from the Google Cloud Code API is usually an
        // account-level intermittent issue (staged rollout, unsynced account permissions, etc.);
        // rotating accounts often resolves it
        404 => RetryStrategy::FixedDelay(Duration::from_millis(300)),

        // Other errors: do not retry
        _ => RetryStrategy::NoRetry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_short_429_preserves_rotation_budget_and_structured_status() {
        let body = r#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"1s"}]}}"#;

        let drive_failures = |account_count| {
            let mut state = RequestRetryState::default();
            let mut used_attempts = 0;
            let mut retry_same_account = false;
            let mut sends = Vec::new();

            while let Some(attempt) = next_rotation_attempt(
                &mut used_attempts,
                account_count,
                retry_same_account,
            ) {
                retry_same_account = false;
                sends.push(attempt);
                let account_id = format!("account-{}", attempt);
                let strategy =
                    state.determine_strategy(&account_id, 429, body, None, false);
                if matches!(strategy, RetryStrategy::GraceRetry(_)) {
                    assert!(!should_rotate_account(429, Some(&strategy)));
                    retry_same_account = true;
                } else {
                    assert!(should_rotate_account(429, Some(&strategy)));
                }
            }
            sends
        };

        assert_eq!(drive_failures(1), vec![0, 0]);
        assert_eq!(drive_failures(2), vec![0, 0, 1, 1]);

        let mut all_429 = FailureStatusTracker::default();
        all_429.record(StatusCode::TOO_MANY_REQUESTS);
        all_429.record(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(all_429.final_status(), StatusCode::TOO_MANY_REQUESTS);

        for non_429 in [StatusCode::FORBIDDEN, StatusCode::SERVICE_UNAVAILABLE] {
            let mut mixed = FailureStatusTracker::default();
            mixed.record(non_429);
            mixed.record(StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(mixed.final_status(), non_429);
        }

        let mut all_503 = FailureStatusTracker::default();
        all_503.record(StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(all_503.final_status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

/// Execute the backoff strategy and return whether retrying should continue
pub async fn apply_retry_strategy(
    strategy: RetryStrategy,
    attempt: usize,
    max_attempts: usize,
    status_code: u16,
    trace_id: &str,
) -> bool {
    match strategy {
        RetryStrategy::NoRetry => {
            debug!(
                "[{}] Non-retryable error {}, stopping",
                trace_id, status_code
            );
            false
        }

        RetryStrategy::FixedDelay(duration) => {
            let base_ms = duration.as_millis() as u64;
            info!(
                "[{}] ⏱️ Retry with fixed delay: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                base_ms
            );
            sleep(duration).await;
            true
        }

        RetryStrategy::LinearBackoff { base_ms } => {
            let calculated_ms = base_ms * (attempt as u64 + 1);
            info!(
                "[{}] ⏱️ Retry with linear backoff: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                calculated_ms
            );
            sleep(Duration::from_millis(calculated_ms)).await;
            true
        }

        RetryStrategy::ExponentialBackoff { base_ms, max_ms } => {
            let calculated_ms = (base_ms * 2_u64.pow(attempt as u32)).min(max_ms);
            info!(
                "[{}] ⏱️ Retry with exponential backoff: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                calculated_ms
            );
            sleep(Duration::from_millis(calculated_ms)).await;
            true
        }

        RetryStrategy::GraceRetry(duration) => {
            info!(
                "[{}] ⚡ Grace Retry: Performing micro-wait ({}ms) on current account...",
                trace_id,
                duration.as_millis()
            );
            sleep(duration).await;
            true // Whether an in-place retry switches accounts is decided at the handlers
                 // level via should_rotate_account
        }
    }
}

/// Determine whether the account should be rotated
pub fn should_rotate_account(status_code: u16, strategy: Option<&RetryStrategy>) -> bool {
    // [NEW] If identified as a Grace Retry, explicitly require no account rotation
    if let Some(RetryStrategy::GraceRetry(_)) = strategy {
        return false;
    }

    match status_code {
        // These errors are account-level or tied to a specific node's quota, so rotate
        429 | 401 | 403 | 404 | 500 => true,
        // 503/529 are usually backend overload; switching accounts has limited effect, so don't rotate for now
        503 | 529 => false,
        _ => false,
    }
}

/// Detects model capabilities and configuration
/// POST /v1/models/detect
pub async fn handle_detect_model(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let model_name = body.get("model").and_then(|v| v.as_str()).unwrap_or("");

    if model_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing 'model' field").into_response();
    }

    // 1. Resolve mapping
    let mapped_model = crate::proxy::common::model_mapping::resolve_model_route(
        model_name,
        &*state.custom_mapping.read().await,
    );

    // 2. Resolve capabilities
    let config = crate::proxy::mappers::common_utils::resolve_request_config(
        model_name,
        &mapped_model,
        &None, // We don't check tools for static capability detection
        None,  // size
        None,  // quality
        None,  // image_size
        None,  // body (not needed for static detection)
    );

    // 3. Construct response
    let mut response = json!({
        "model": model_name,
        "mapped_model": mapped_model,
        "type": config.request_type,
        "features": {
            "has_web_search": config.inject_google_search,
            "is_image_gen": config.request_type == "image_gen"
        }
    });

    if let Some(img_conf) = config.image_config {
        if let Some(obj) = response.as_object_mut() {
            obj.insert("config".to_string(), img_conf);
        }
    }

    Json(response).into_response()
}
