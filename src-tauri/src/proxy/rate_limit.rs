use dashmap::DashMap;
use regex::Regex;
use std::time::{Duration, SystemTime};

const MAX_LOCKOUT_SECONDS: u64 = 300;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetryParserMode {
    Current,
    Baseline,
}

/// Rate limit reason type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateLimitReason {
    /// Quota exhausted (QUOTA_EXHAUSTED)
    QuotaExhausted,
    /// Rate limit exceeded (RATE_LIMIT_EXCEEDED)
    RateLimitExceeded,
    /// Model capacity exhausted (MODEL_CAPACITY_EXHAUSTED)
    ModelCapacityExhausted,
    /// Server error (5xx)
    ServerError,
    /// Unknown reason
    Unknown,
}

pub(crate) fn normalize_image_model_id(model: &str) -> Option<String> {
    let normalized = crate::proxy::common::model_mapping::normalize_to_standard_id(model)?;
    matches!(
        normalized.as_str(),
        "gemini-3.1-flash-image" | "gemini-3-pro-image"
    )
    .then_some(normalized)
}

pub(crate) fn has_explicit_quota_exhausted(body: &str) -> bool {
    body.to_ascii_uppercase().contains("QUOTA_EXHAUSTED")
}

pub(crate) fn is_active_persisted_long_image_limit(
    model_key: &str,
    status: &crate::models::account::LiveLimitStatus,
    now: i64,
) -> bool {
    status.status == 429
        && status.reason == "QuotaExhausted"
        && status.until > now
        && status.until.saturating_sub(status.detected_at) > MAX_LOCKOUT_SECONDS as i64
        && normalize_image_model_id(model_key).is_some()
        && status.message.as_deref().is_some_and(|message| {
            has_explicit_quota_exhausted(message)
                && crate::proxy::upstream::retry::parse_retry_delay(message, None).is_some()
        })
}

/// Rate limit info
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    /// Rate limit reset time
    pub reset_time: SystemTime,
    /// Retry interval (seconds)
    #[allow(dead_code)]
    pub retry_after_sec: u64,
    /// Detection time
    #[allow(dead_code)]
    pub detected_at: SystemTime,
    /// Rate limit reason
    #[allow(dead_code)] // Used for logging and diagnostics
    pub reason: RateLimitReason,
    /// Associated model (used for model-level rate limiting)
    /// None means account-level rate limiting, Some(model) means a specific model is rate limited
    #[allow(dead_code)] // Used for model-level rate limiting
    pub model: Option<String>,
}

/// Failure count expiry: 1 hour (count resets if no failure occurs within this window)
const FAILURE_COUNT_EXPIRY_SECONDS: u64 = 3600;

/// Rate limit tracker
pub struct RateLimitTracker {
    limits: DashMap<String, RateLimitInfo>,
    /// Consecutive failure count (used for smart exponential backoff), with a timestamp for auto-expiry
    failure_counts: DashMap<String, (u32, SystemTime)>,
}

impl RateLimitTracker {
    pub fn new() -> Self {
        Self {
            limits: DashMap::new(),
            failure_counts: DashMap::new(),
        }
    }

    /// Generate the rate limit key
    /// - Account level: "account_id"
    /// - Model level: "account_id:model_id"
    fn get_limit_key(&self, account_id: &str, model: Option<&str>) -> String {
        match model {
            Some(m) if !m.is_empty() => format!("{}:{}", account_id, m),
            _ => account_id.to_string(),
        }
    }

    /// Get the account's remaining wait time (seconds)
    /// Supports checking both account-level and model-level locks
    pub fn get_remaining_wait(&self, account_id: &str, model: Option<&str>) -> u64 {
        let now = SystemTime::now();

        // 1. Check the global account lock
        if let Some(info) = self.limits.get(account_id) {
            if info.reset_time > now {
                return info
                    .reset_time
                    .duration_since(now)
                    .unwrap_or(Duration::from_secs(0))
                    .as_secs();
            }
        }

        // 2. If a model is specified, check the model-level lock
        if let Some(m) = model {
            let key = self.get_limit_key(account_id, Some(m));
            if let Some(info) = self.limits.get(&key) {
                if info.reset_time > now {
                    return info
                        .reset_time
                        .duration_since(now)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs();
                }
            }
        }

        0
    }

    /// Mark the account's request as successful, resetting the consecutive failure count
    ///
    /// Call this method after the account completes a request successfully, zeroing its failure count,
    /// so the next failure starts from the shortest lockout time (60 seconds).
    pub fn mark_success(&self, account_id: &str) {
        if self.failure_counts.remove(account_id).is_some() {
            tracing::debug!("Account {} request succeeded, failure count reset", account_id);
        }
        // Clear the account-level rate limit
        self.limits.remove(account_id);
        // Note: we currently cannot clear all model-level locks under this account, since we don't know which models are locked
        // without iterating limits. Since model-level locks are usually QuotaExhausted, letting them expire naturally is acceptable.
        // We could also introduce an index, but for simplicity we only clear the Account-level lock for now.
    }

    /// Precisely lock the account until a specific point in time
    ///
    /// Uses the reset_time from the account's quota to precisely lock the account,
    /// which is more accurate than exponential backoff.
    ///
    /// # Parameters
    /// - `model`: an optional model name, used for model-level rate limiting. None means account-level rate limiting
    pub fn set_lockout_until(
        &self,
        account_id: &str,
        reset_time: SystemTime,
        reason: RateLimitReason,
        model: Option<String>,
    ) {
        let now = SystemTime::now();
        let (mut retry_sec, mut effective_reset_time) = reset_time
            .duration_since(now)
            .map(|duration| (duration.as_secs(), reset_time))
            .unwrap_or((60, now + Duration::from_secs(60)));

        if retry_sec > MAX_LOCKOUT_SECONDS {
            tracing::info!(
                "Capping lockout time for {} from {}s to 300s (5 minutes)",
                account_id,
                retry_sec
            );
            retry_sec = MAX_LOCKOUT_SECONDS;
            effective_reset_time = now + Duration::from_secs(retry_sec);
        }

        let info = RateLimitInfo {
            reset_time: effective_reset_time,
            retry_after_sec: retry_sec,
            detected_at: now,
            reason,
            model: model.clone(), // New: supports model-level rate limiting
        };

        let key = self.get_limit_key(account_id, model.as_deref());
        self.limits.insert(key, info);

        if let Some(m) = &model {
            tracing::info!(
                "Account {}'s model {} has been precisely locked to the quota refresh time, {} seconds remaining",
                account_id,
                m,
                retry_sec
            );
        } else {
            tracing::info!(
                "Account {} has been precisely locked to the quota refresh time, {} seconds remaining",
                account_id,
                retry_sec
            );
        }
    }

    pub fn restore_persisted_long_image_limit(
        &self,
        account_id: &str,
        reset_time: SystemTime,
        detected_at: SystemTime,
        model: &str,
    ) -> bool {
        let Some(normalized_model) = normalize_image_model_id(model) else {
            return false;
        };
        let now = SystemTime::now();
        let Ok(original_duration) = reset_time.duration_since(detected_at) else {
            return false;
        };
        let Ok(remaining) = reset_time.duration_since(now) else {
            return false;
        };
        if original_duration <= Duration::from_secs(MAX_LOCKOUT_SECONDS) {
            return false;
        }

        let info = RateLimitInfo {
            reset_time,
            retry_after_sec: remaining.as_secs(),
            detected_at,
            reason: RateLimitReason::QuotaExhausted,
            model: Some(normalized_model.clone()),
        };
        let key = self.get_limit_key(account_id, Some(&normalized_model));
        self.limits.insert(key, info);
        true
    }

    /// Precisely lock the account using an ISO 8601 time string
    ///
    /// Parses a time string in a format like "2026-01-08T17:00:00Z"
    ///
    /// # Parameters
    /// - `model`: an optional model name, used for model-level rate limiting
    pub fn set_lockout_until_iso(
        &self,
        account_id: &str,
        reset_time_str: &str,
        reason: RateLimitReason,
        model: Option<String>,
    ) -> bool {
        // Try to parse the ISO 8601 format
        match chrono::DateTime::parse_from_rfc3339(reset_time_str) {
            Ok(dt) => {
                let reset_time =
                    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(dt.timestamp() as u64);
                self.set_lockout_until(account_id, reset_time, reason, model);
                true
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to parse quota refresh time '{}': {}, falling back to default backoff strategy",
                    reset_time_str,
                    e
                );
                false
            }
        }
    }

    /// Parse rate limit info from an error response
    ///
    /// # Arguments
    /// * `account_id` - the account ID
    /// * `status` - the HTTP status code
    /// * `retry_after_header` - the Retry-After header value
    /// * `body` - the error response body
    pub fn parse_from_error(
        &self,
        account_id: &str,
        status: u16,
        retry_after_header: Option<&str>,
        body: &str,
        model: Option<String>,
        backoff_steps: &[u64], // [NEW] the backoff config passed in
    ) -> Option<RateLimitInfo> {
        self.parse_from_error_with_mode(
            account_id,
            status,
            retry_after_header,
            body,
            model,
            backoff_steps,
            RetryParserMode::Current,
        )
    }

    pub fn parse_from_error_baseline(
        &self,
        account_id: &str,
        status: u16,
        retry_after_header: Option<&str>,
        body: &str,
        model: Option<String>,
        backoff_steps: &[u64],
    ) -> Option<RateLimitInfo> {
        self.parse_from_error_with_mode(
            account_id,
            status,
            retry_after_header,
            body,
            model,
            backoff_steps,
            RetryParserMode::Baseline,
        )
    }

    fn parse_from_error_with_mode(
        &self,
        account_id: &str,
        status: u16,
        retry_after_header: Option<&str>,
        body: &str,
        model: Option<String>,
        backoff_steps: &[u64],
        parser_mode: RetryParserMode,
    ) -> Option<RateLimitInfo> {
        // Supports 429 (rate limit) as well as 500/503/529 (backend failure soft-avoidance)
        if status != 429 && status != 500 && status != 503 && status != 529 && status != 404 {
            return None;
        }

        // 1. Parse the rate limit reason type
        let reason = if status == 429 {
            tracing::warn!("Google 429 Error Body: {}", body);
            self.parse_rate_limit_reason(body)
        } else if status == 404 {
            tracing::warn!(
                "Google 404: model unavailable on this account, short lockout before rotation"
            );
            RateLimitReason::ServerError
        } else {
            RateLimitReason::ServerError
        };

        let retry_after_sec = match parser_mode {
            RetryParserMode::Current => {
                crate::proxy::upstream::retry::parse_retry_delay(body, retry_after_header)
                    .map(|delay_ms| delay_ms.saturating_add(999) / 1000)
            }
            RetryParserMode::Baseline => retry_after_header
                .and_then(|value| value.parse::<u64>().ok())
                .or_else(|| self.parse_retry_time_from_body_baseline(body)),
        };
        let has_explicit_retry_time = retry_after_sec.is_some();
        let preserve_long_image_quota = parser_mode == RetryParserMode::Current
            && status == 429
            && reason == RateLimitReason::QuotaExhausted
            && has_explicit_quota_exhausted(body)
            && has_explicit_retry_time
            && model
                .as_deref()
                .and_then(normalize_image_model_id)
                .is_some();

        // 4. Handle default values and soft-avoidance logic (set different defaults per rate limit type)
        let retry_sec = match retry_after_sec {
            Some(s) => {
                // Set a safety buffer: minimum 2 seconds, to prevent extremely high-frequency wasted retries
                if s < 2 {
                    2
                } else {
                    s
                }
            }
            None => {
                // Get the consecutive failure count, used for exponential backoff (with auto-expiry logic)
                // [FIX] ServerError (5xx) does not accumulate failure_count, to avoid polluting the 429 backoff ladder
                let failure_count = if reason != RateLimitReason::ServerError {
                    // Only non-ServerError failures accumulate the failure count (used for exponential backoff)
                    let now = SystemTime::now();
                    // Here we use account_id as the key, without distinguishing by model,
                    // because this is meant to compute backoff for consecutive "account-level" issues.
                    // If per-model consecutive failure counting is needed, the failure_counts key may need to change.
                    // Keeping account_id for now, so that if one model keeps failing, the count still increases, which is reasonable.
                    let mut entry = self
                        .failure_counts
                        .entry(account_id.to_string())
                        .or_insert((0, now));

                    let elapsed = now
                        .duration_since(entry.1)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs();
                    if elapsed > FAILURE_COUNT_EXPIRY_SECONDS {
                        tracing::debug!(
                            "Account {}'s failure count has expired ({} seconds), reset to 0",
                            account_id,
                            elapsed
                        );
                        *entry = (0, now);
                    }
                    entry.0 += 1;
                    entry.1 = now;
                    entry.0
                } else {
                    // ServerError (5xx) uses a fixed value of 1, not accumulated, to avoid polluting the 429 backoff ladder
                    1
                };

                match reason {
                    RateLimitReason::QuotaExhausted => {
                        // [Smart rate limiting] Computed from failure_count and the configured backoff_steps
                        let index = (failure_count as usize).saturating_sub(1);
                        let lockout = if index < backoff_steps.len() {
                            backoff_steps[index]
                        } else {
                            *backoff_steps.last().unwrap_or(&7200)
                        };

                        tracing::warn!(
                            "Detected quota exhausted (QUOTA_EXHAUSTED), consecutive failure #{}, locking for {} seconds per config",
                            failure_count,
                            lockout
                        );
                        lockout
                    }
                    RateLimitReason::RateLimitExceeded => {
                        // Rate limit (TPM/RPM)
                        let body_lower = body.to_lowercase();
                        let lockout = if body_lower.contains("resource has been exhausted")
                            || body_lower.contains("resource_exhausted")
                        {
                            30
                        } else {
                            5
                        };
                        tracing::debug!(
                            "Detected rate limit exceeded (RATE_LIMIT_EXCEEDED), using default value of {} seconds",
                            lockout
                        );
                        lockout
                    }
                    RateLimitReason::ModelCapacityExhausted => {
                        // Model capacity exhausted
                        let lockout = match failure_count {
                            1 => 5,
                            2 => 10,
                            _ => 15,
                        };
                        tracing::warn!(
                            "Detected model capacity exhausted (MODEL_CAPACITY_EXHAUSTED), failure #{}, retrying after {} seconds",
                            failure_count,
                            lockout
                        );
                        lockout
                    }
                    RateLimitReason::ServerError => {
                        let lockout = if status == 404 { 5 } else { 8 };
                        tracing::warn!("Detected {} error, applying {}s soft-avoidance...", status, lockout);
                        lockout
                    }
                    RateLimitReason::Unknown => {
                        // Unknown reason
                        tracing::debug!("Failed to parse the 429 rate limit reason, using default value of 60 seconds");
                        60
                    }
                }
            }
        };

        let mut retry_sec = retry_sec;
        if retry_sec > MAX_LOCKOUT_SECONDS && !preserve_long_image_quota {
            tracing::info!(
                "Capping retry lockout time for {} from {}s to 300s (5 minutes)",
                account_id,
                retry_sec
            );
            retry_sec = MAX_LOCKOUT_SECONDS;
        }

        let info = RateLimitInfo {
            reset_time: SystemTime::now() + Duration::from_secs(retry_sec),
            retry_after_sec: retry_sec,
            detected_at: SystemTime::now(),
            reason,
            model: model.clone(),
        };

        // [FIX] Store using a composite key (when it's Quota and has a Model)
        // Only QuotaExhausted is suited to model-level isolation; others like RateLimitExceeded are usually account-wide TPM
        let use_model_key = matches!(reason, RateLimitReason::QuotaExhausted) && model.is_some();
        let key = if use_model_key {
            self.get_limit_key(account_id, model.as_deref())
        } else {
            // Other cases (like RateLimitExceeded, ServerError) usually affect the whole account
            // We could also decide whether to isolate based on config.
            // For simplicity, only QuotaExhausted gets fine-grained isolation.
            account_id.to_string()
        };

        self.limits.insert(key, info.clone());

        tracing::warn!(
            "Account {} [{}] rate limit type: {:?}, reset delay: {} seconds",
            account_id,
            status,
            reason,
            retry_sec
        );

        Some(info)
    }

    /// Parse the rate limit reason type
    fn parse_rate_limit_reason(&self, body: &str) -> RateLimitReason {
        // Try to extract the reason field from JSON
        let trimmed = body.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(reason_str) = json
                    .get("error")
                    .and_then(|e| e.get("details"))
                    .and_then(|d| d.as_array())
                    .and_then(|a| a.get(0))
                    .and_then(|o| o.get("reason"))
                    .and_then(|v| v.as_str())
                {
                    return match reason_str {
                        "QUOTA_EXHAUSTED" => RateLimitReason::QuotaExhausted,
                        "RATE_LIMIT_EXCEEDED" => RateLimitReason::RateLimitExceeded,
                        "MODEL_CAPACITY_EXHAUSTED" => RateLimitReason::ModelCapacityExhausted,
                        _ => RateLimitReason::Unknown,
                    };
                }
                // [NEW] Try text matching against the message field (to avoid a missed reason)
                if let Some(msg) = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                {
                    let msg_lower = msg.to_lowercase();
                    if msg_lower.contains("per minute") || msg_lower.contains("rate limit") {
                        return RateLimitReason::RateLimitExceeded;
                    }
                }
            }
        }

        // If it can't be parsed from JSON, try to infer from the message text
        let body_lower = body.to_lowercase();
        // [FIX] Prefer detecting per-minute limits first, to avoid misclassifying TPM as Quota
        let generic_resource_exhausted = body_lower.contains("resource has been exhausted")
            || body_lower.contains("resource_exhausted");
        let explicit_quota_exhausted = body_lower.contains("quota_exhausted")
            || body_lower.contains("quotaresetdelay")
            || body_lower.contains("quota reset")
            || body_lower.contains("quota limit")
            || body_lower.contains("per day")
            || body_lower.contains("daily quota");

        if body_lower.contains("per minute")
            || body_lower.contains("rate limit")
            || body_lower.contains("too many requests")
            || (generic_resource_exhausted && !explicit_quota_exhausted)
        {
            RateLimitReason::RateLimitExceeded
        } else if body_lower.contains("exhausted") || body_lower.contains("quota") {
            RateLimitReason::QuotaExhausted
        } else {
            RateLimitReason::Unknown
        }
    }

    /// Parse the reset time out of the error message body
    fn parse_retry_time_from_body(&self, body: &str) -> Option<u64> {
        crate::proxy::upstream::retry::parse_retry_delay(body, None)
            .map(|delay_ms| delay_ms.saturating_add(999) / 1000)
    }

    fn parse_duration_string_baseline(&self, value: &str) -> Option<u64> {
        let re = Regex::new(r"(?:(\d+)h)?(?:(\d+)m)?(?:(\d+(?:\.\d+)?)s)?(?:(\d+(?:\.\d+)?)ms)?")
            .ok()?;
        let captures = re.captures(value)?;
        let hours = captures
            .get(1)
            .and_then(|value| value.as_str().parse::<u64>().ok())
            .unwrap_or(0);
        let minutes = captures
            .get(2)
            .and_then(|value| value.as_str().parse::<u64>().ok())
            .unwrap_or(0);
        let seconds = captures
            .get(3)
            .and_then(|value| value.as_str().parse::<f64>().ok())
            .unwrap_or(0.0);
        let milliseconds = captures
            .get(4)
            .and_then(|value| value.as_str().parse::<f64>().ok())
            .unwrap_or(0.0);
        let total_seconds = hours * 3600
            + minutes * 60
            + seconds.ceil() as u64
            + (milliseconds / 1000.0).ceil() as u64;
        (total_seconds > 0).then_some(total_seconds)
    }

    fn parse_retry_time_from_body_baseline(&self, body: &str) -> Option<u64> {
        let trimmed = body.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(delay) = json
                    .get("error")
                    .and_then(|error| error.get("details"))
                    .and_then(|details| details.as_array())
                    .and_then(|details| details.first())
                    .and_then(|detail| detail.get("metadata"))
                    .and_then(|metadata| metadata.get("quotaResetDelay"))
                    .and_then(|value| value.as_str())
                    .and_then(|value| self.parse_duration_string_baseline(value))
                {
                    return Some(delay);
                }
                if let Some(retry) = json
                    .get("error")
                    .and_then(|error| error.get("retry_after"))
                    .and_then(|value| value.as_u64())
                {
                    return Some(retry);
                }
            }
        }

        for pattern in [
            r"(?i)try again in (\d+)m\s*(\d+)s",
            r"(?i)(?:try again in|backoff for|wait)\s*(\d+)s",
            r"(?i)quota will reset in (\d+) second",
            r"(?i)retry after (\d+) second",
            r"\(wait (\d+)s\)",
        ] {
            let captures = Regex::new(pattern).ok()?.captures(body);
            let Some(captures) = captures else {
                continue;
            };
            if captures.len() == 3 {
                let minutes = captures.get(1)?.as_str().parse::<u64>().ok()?;
                let seconds = captures.get(2)?.as_str().parse::<u64>().ok()?;
                return Some(minutes * 60 + seconds);
            }
            if let Some(seconds) = captures
                .get(1)
                .and_then(|value| value.as_str().parse::<u64>().ok())
            {
                return Some(seconds);
            }
        }
        None
    }

    /// Get the account's rate limit info
    pub fn get(&self, account_id: &str) -> Option<RateLimitInfo> {
        self.limits.get(account_id).map(|r| r.clone())
    }

    pub fn clear_model(&self, account_id: &str, model: &str) -> bool {
        let normalized = crate::proxy::common::model_mapping::normalize_to_standard_id(model)
            .unwrap_or_else(|| model.to_string());
        let mut cleared = self
            .limits
            .remove(&self.get_limit_key(account_id, Some(&normalized)))
            .is_some();
        if normalized != model {
            cleared |= self
                .limits
                .remove(&self.get_limit_key(account_id, Some(model)))
                .is_some();
        }
        cleared
    }

    /// Check whether the account is still rate limited
    /// Check whether the account is still rate limited (supports model level)
    pub fn is_rate_limited(&self, account_id: &str, model: Option<&str>) -> bool {
        // Checking using get_remaining_wait which handles both global and model keys
        self.get_remaining_wait(account_id, model) > 0
    }

    /// Get how many seconds remain until the rate limit resets
    pub fn get_reset_seconds(&self, account_id: &str) -> Option<u64> {
        if let Some(info) = self.get(account_id) {
            info.reset_time
                .duration_since(SystemTime::now())
                .ok()
                .map(|d| d.as_secs())
        } else {
            None
        }
    }

    /// Clear expired rate limit records
    #[allow(dead_code)]
    pub fn cleanup_expired(&self) -> usize {
        let now = SystemTime::now();
        let mut count = 0;

        self.limits.retain(|_k, v| {
            if v.reset_time <= now {
                count += 1;
                false
            } else {
                true
            }
        });

        if count > 0 {
            tracing::debug!("Cleared {} expired rate limit records", count);
        }

        count
    }

    /// Clear the rate limit records for a given account
    pub fn clear(&self, account_id: &str) -> bool {
        let prefix = format!("{}:", account_id);
        let before = self.limits.len();
        self.limits
            .retain(|key, _| key != account_id && !key.starts_with(&prefix));
        self.failure_counts.remove(account_id);
        self.limits.len() != before
    }

    pub fn clear_for_optimistic_reset(&self) {
        let now = SystemTime::now();
        self.limits.retain(|_, info| {
            info.reason == RateLimitReason::QuotaExhausted
                && info
                    .model
                    .as_deref()
                    .and_then(normalize_image_model_id)
                    .is_some()
                && info
                    .reset_time
                    .duration_since(info.detected_at)
                    .is_ok_and(|duration| duration > Duration::from_secs(MAX_LOCKOUT_SECONDS))
                && info.reset_time.duration_since(now).is_ok()
        });
    }

    /// Clear all rate limit records (optimistic reset strategy)
    ///
    /// Used for the optimistic reset mechanism: when all accounts are rate limited but the wait time is very short,
    /// clear all rate limit records to resolve the timing race condition
    pub fn clear_all(&self) {
        let count = self.limits.len();
        self.limits.clear();
        tracing::warn!(
            "🔄 Optimistic reset: Cleared all {} rate limit record(s)",
            count
        );
    }
}

impl Default for RateLimitTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_retry_time_minutes_seconds() {
        let tracker = RateLimitTracker::new();
        let body = "Rate limit exceeded. Try again in 2m 30s";
        let time = tracker.parse_retry_time_from_body(body);
        assert_eq!(time, Some(150));
    }

    #[test]
    fn test_parse_google_json_delay() {
        let tracker = RateLimitTracker::new();
        let body = r#"{
            "error": {
                "details": [
                    { 
                        "metadata": {
                            "quotaResetDelay": "42s" 
                        }
                    }
                ]
            }
        }"#;
        let time = tracker.parse_retry_time_from_body(body);
        assert_eq!(time, Some(42));
    }

    #[test]
    fn test_parse_retry_after_ignore_case() {
        let tracker = RateLimitTracker::new();
        let body = "Quota limit hit. Retry After 99 Seconds";
        let time = tracker.parse_retry_time_from_body(body);
        assert_eq!(time, Some(99));
    }

    #[test]
    fn test_get_remaining_wait() {
        let tracker = RateLimitTracker::new();
        tracker.parse_from_error("acc1", 429, Some("30"), "", None, &[]);
        let wait = tracker.get_remaining_wait("acc1", None);
        assert!(wait > 25 && wait <= 30);
    }

    #[test]
    fn test_safety_buffer() {
        let tracker = RateLimitTracker::new();
        // If the API returns 1s, we force it to 2s
        tracker.parse_from_error("acc1", 429, Some("1"), "", None, &[]);
        let wait = tracker.get_remaining_wait("acc1", None);
        // Due to time passing, it might be 1 or 2
        assert!(wait >= 1 && wait <= 2);
    }

    #[test]
    fn task_preserves_explicit_long_image_quota_deadline() {
        let tracker = RateLimitTracker::new();
        let body = r#"{
            "error": {
                "details": [{
                    "reason": "QUOTA_EXHAUSTED",
                    "metadata": {"quotaResetDelay": "58h24m53s"}
                }]
            }
        }"#;
        let info = tracker
            .parse_from_error(
                "acc-long",
                429,
                None,
                body,
                Some("gemini-3-pro-image".to_string()),
                &[60, 300],
            )
            .unwrap();
        assert_eq!(info.retry_after_sec, 210_293);

        tracker.clear_for_optimistic_reset();
        assert!(tracker.is_rate_limited("acc-long", Some("gemini-3-pro-image")));

        let now = SystemTime::now();
        assert!(tracker.restore_persisted_long_image_limit(
            "acc-expiring",
            now + Duration::from_secs(30),
            now - Duration::from_secs(301),
            "gemini-3.1-flash-image",
        ));
        tracker.clear_for_optimistic_reset();
        assert!(tracker.is_rate_limited("acc-expiring", Some("gemini-3.1-flash-image")));

        let inferred = tracker
            .parse_from_error(
                "acc-inferred",
                429,
                None,
                "Quota limit hit; reset after 72h",
                Some("gemini-3-pro-image".to_string()),
                &[60, 300],
            )
            .unwrap();
        assert_eq!(inferred.retry_after_sec, 300);

        let text_model = tracker
            .parse_from_error(
                "acc-text",
                429,
                Some("72h"),
                r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#,
                Some("gemini-2.5-pro".to_string()),
                &[60, 300],
            )
            .unwrap();
        assert_eq!(text_model.retry_after_sec, 300);

        let broad_quota = tracker
            .parse_from_error(
                "acc-broad",
                429,
                None,
                "Quota limit hit; reset after 72h",
                Some("gemini-3.1-flash-image".to_string()),
                &[60, 300],
            )
            .unwrap();
        assert_eq!(broad_quota.retry_after_sec, 300);

        let inferred_reset = SystemTime::now() + Duration::from_secs(72 * 3600);
        tracker.set_lockout_until(
            "acc-inferred-reset",
            inferred_reset,
            RateLimitReason::QuotaExhausted,
            Some("gemini-3-pro-image".to_string()),
        );
        assert!(
            tracker.get_remaining_wait("acc-inferred-reset", Some("gemini-3-pro-image")) <= 300
        );
    }

    #[test]
    fn task_claude_baseline_ignores_retry_info_delay() {
        for (index, delay) in ["1s", "3s"].into_iter().enumerate() {
            let body = format!(
                r#"{{"error":{{"details":[{{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"{}"}}]}}}}"#,
                delay
            );
            let current = RateLimitTracker::new()
                .parse_from_error(
                    &format!("current-{}", index),
                    429,
                    None,
                    &body,
                    None,
                    &[60, 300],
                )
                .unwrap();
            assert_eq!(current.retry_after_sec, if index == 0 { 2 } else { 3 });

            let baseline = RateLimitTracker::new()
                .parse_from_error_baseline(
                    &format!("baseline-{}", index),
                    429,
                    None,
                    &body,
                    None,
                    &[60, 300],
                )
                .unwrap();
            assert_eq!(baseline.retry_after_sec, 60);
        }
    }

    #[test]
    fn test_tpm_exhausted_is_rate_limit_exceeded() {
        let tracker = RateLimitTracker::new();
        // Simulate a real-world TPM error, containing both "Resource exhausted" and "per minute"
        let body = "Resource has been exhausted (e.g. check quota). Quota limit 'Tokens per minute' exceeded.";
        let reason = tracker.parse_rate_limit_reason(body);
        // Should be recognized as RateLimitExceeded, not QuotaExhausted
        assert_eq!(reason, RateLimitReason::RateLimitExceeded);
    }

    #[test]
    fn test_generic_resource_exhausted_is_short_rate_limit() {
        let tracker = RateLimitTracker::new();
        let body = r#"{
            "error": {
                "code": 429,
                "message": "Resource has been exhausted (e.g. check quota).",
                "status": "RESOURCE_EXHAUSTED"
            }
        }"#;
        let reason = tracker.parse_rate_limit_reason(body);
        assert_eq!(reason, RateLimitReason::RateLimitExceeded);
    }

    #[test]
    fn test_server_error_does_not_accumulate_failure_count() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];

        // Simulate 5 consecutive 5xx errors
        for i in 1..=5 {
            let info = tracker.parse_from_error(
                "acc1",
                503,
                None,
                "Service Unavailable",
                None,
                &backoff_steps,
            );
            assert!(info.is_some(), "The {}th 5xx should return a RateLimitInfo", i);
            let info = info.unwrap();
            // 5xx should always lock for 8 seconds, unaffected by failure_count
            assert_eq!(info.retry_after_sec, 8, "The {}th 5xx should lock for 8 seconds", i);
        }

        // Now trigger a single 429 QuotaExhausted (with no quotaResetDelay)
        let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;
        let info = tracker.parse_from_error("acc1", 429, None, quota_body, None, &backoff_steps);
        assert!(info.is_some());
        let info = info.unwrap();

        // Key assertion: the 429 should start from failure #1 (lock 60 seconds), not inherit the 5xx count
        assert_eq!(
            info.retry_after_sec, 60,
            "The 429 should start backoff from failure #1 (60 seconds), not be polluted by the 5xx count"
        );
    }

    #[test]
    fn test_quota_exhausted_does_accumulate_failure_count() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];
        let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;

        // 429 failure #1 -> 60 seconds
        let info = tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 60);

        // 429 failure #2 -> 300 seconds
        let info = tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 300);

        // 429 failure #3 -> 1800 seconds
        let info = tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 1800);

        // 429 failure #4 -> 7200 seconds
        let info = tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 7200);
    }
}
