// Removed redundant top-level imports, since these are already handled in the code via full paths or local imports
use axum::http::StatusCode;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

use crate::proxy::rate_limit::RateLimitTracker;
use crate::proxy::server::{ImagePermit, ImageScheduler};
use crate::proxy::sticky_config::StickySessionConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnDiskAccountState {
    Enabled,
    Disabled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackerParserMode {
    Current,
    Baseline,
}

fn classify_rate_limit_reason(error_body: &str) -> crate::proxy::rate_limit::RateLimitReason {
    use crate::proxy::rate_limit::RateLimitReason;

    let body = error_body.to_lowercase();
    let generic_resource_exhausted =
        body.contains("resource has been exhausted") || body.contains("resource_exhausted");
    let explicit_quota_exhausted = body.contains("quota_exhausted")
        || body.contains("quotaresetdelay")
        || body.contains("quota reset")
        || body.contains("quota limit")
        || body.contains("per day")
        || body.contains("daily quota");

    if body.contains("model_capacity") {
        RateLimitReason::ModelCapacityExhausted
    } else if body.contains("per minute")
        || body.contains("rate limit")
        || body.contains("too many requests")
        || (generic_resource_exhausted && !explicit_quota_exhausted)
    {
        RateLimitReason::RateLimitExceeded
    } else if explicit_quota_exhausted || body.contains("exhausted") || body.contains("quota") {
        RateLimitReason::QuotaExhausted
    } else {
        RateLimitReason::Unknown
    }
}

const IMAGE_ACCOUNT_RESELECT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

async fn wait_for_image_account_change(
    changes: &mut tokio::sync::watch::Receiver<u64>,
    remaining: std::time::Duration,
) -> bool {
    if remaining.is_zero() {
        return false;
    }
    tokio::select! {
        result = changes.changed() => result.is_ok(),
        _ = tokio::time::sleep(remaining.min(IMAGE_ACCOUNT_RESELECT_INTERVAL)) => true,
    }
}

async fn wait_for_image_token_selection<T>(
    deadline: tokio::time::Instant,
    selection: impl std::future::Future<Output = T>,
) -> Option<T> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return None;
    }
    tokio::time::timeout(remaining, selection).await.ok()
}

/// Async-safe account JSON update function
///
/// Uses `tokio::task::spawn_blocking` to move the blocking file I/O and the
/// `std::sync::Mutex` acquisition onto Tokio's blocking thread pool, avoiding
/// occupying a Tokio Worker Thread and preventing Tokio runtime starvation caused
/// by sync lock contention under high concurrency.
async fn update_account_json(
    path: &std::path::Path,
    update: impl FnOnce(&mut serde_json::Value) + Send + 'static,
) -> Result<(), String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _account_write = crate::modules::account::lock_account_file_updates()?;
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {}", e))?;
        let mut content: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("Failed to parse JSON: {}", e))?;
        update(&mut content);
        let serialized = serde_json::to_string_pretty(&content)
            .map_err(|e| format!("Failed to serialize JSON: {}", e))?;
        std::fs::write(&path, serialized).map_err(|e| format!("Failed to write file: {}", e))
    })
    .await
    .map_err(|e| format!("spawn_blocking panicked: {}", e))?
}

fn unix_timestamp_ceil(time: std::time::SystemTime) -> Option<i64> {
    let since_epoch = time
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?;
    let seconds = since_epoch
        .as_secs()
        .saturating_add(u64::from(since_epoch.subsec_nanos() > 0));
    i64::try_from(seconds).ok()
}

#[derive(Debug, Clone)]
pub struct ProxyToken {
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub timestamp: i64,
    pub email: String,
    pub account_path: PathBuf, // Account file path, used for updates
    pub project_id: Option<String>,
    pub subscription_tier: Option<String>, // "FREE" | "PRO" | "ULTRA"
    pub remaining_quota: Option<i32>,      // [FIX #563] Remaining quota for priority sorting
    pub protected_models: HashSet<String>, // [NEW #621]
    pub health_score: f32,                 // [NEW] Health score (0.0 - 1.0)
    pub reset_time: Option<i64>,           // [NEW] Quota refresh timestamp (used for sort optimization)
    pub validation_blocked: bool, // [NEW] Check for validation block (VALIDATION_REQUIRED temporary block)
    pub validation_blocked_until: i64, // [NEW] Timestamp until which the account is blocked
    pub validation_url: Option<String>, // [NEW] Validation URL (#1522)
    pub model_quotas: HashMap<String, i32>, // [OPTIMIZATION] In-memory cache for model-specific quotas
    pub model_limits: HashMap<String, u64>, // [NEW] max_output_tokens per model from quota data
}

pub struct TokenManager {
    tokens: Arc<DashMap<String, ProxyToken>>, // account_id -> ProxyToken
    current_index: Arc<AtomicUsize>,
    last_used_account: Arc<tokio::sync::Mutex<Option<(String, std::time::Instant)>>>,
    data_dir: PathBuf,
    rate_limit_tracker: Arc<RateLimitTracker>, // Added: rate limit tracker
    sticky_config: Arc<tokio::sync::RwLock<StickySessionConfig>>, // Added: scheduling config
    session_accounts: Arc<DashMap<String, String>>, // Added: session-to-account mapping (SessionID -> AccountID)
    preferred_account_id: Arc<tokio::sync::RwLock<Option<String>>>, // [FIX #820] Preferred account ID (fixed account mode)
    health_scores: Arc<DashMap<String, f32>>,                       // account_id -> health_score
    circuit_breaker_config: Arc<tokio::sync::RwLock<crate::models::CircuitBreakerConfig>>, // [NEW] Circuit breaker config cache

    // [NEW] Per-account sync refresh lock.
    // Used to implement Double-Checked Locking, preventing concurrent requests from causing a single account to call OAuth Refresh multiple times in a short window.
    refresh_locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,

    // [NEW] Async SingleFlight coalescing table for loadCodeAssist (fetch_project_id)
    // Key is account_id, Value is the result watcher, ensuring concurrent requests share the same upstream probe result
    load_code_assist_inflight:
        Arc<DashMap<String, tokio::sync::watch::Receiver<Option<Result<String, String>>>>>,

    // [NEW] Tracks consecutive invalid_grant failure counts per account, to avoid mistakenly deactivating an account due to a single transient network blip
    invalid_grant_failures: Arc<DashMap<String, u32>>,

    /// Supports actively aborting background tasks during graceful shutdown
    auto_cleanup_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    cancel_token: CancellationToken,
    image_scheduler: std::sync::RwLock<Option<Weak<ImageScheduler>>>,
}

impl TokenManager {
    /// Create a new TokenManager
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            tokens: Arc::new(DashMap::new()),
            current_index: Arc::new(AtomicUsize::new(0)),
            last_used_account: Arc::new(tokio::sync::Mutex::new(None)),
            data_dir,
            rate_limit_tracker: Arc::new(RateLimitTracker::new()),
            sticky_config: Arc::new(tokio::sync::RwLock::new(StickySessionConfig::default())),
            session_accounts: Arc::new(DashMap::new()),
            preferred_account_id: Arc::new(tokio::sync::RwLock::new(None)), // [FIX #820]
            health_scores: Arc::new(DashMap::new()),
            circuit_breaker_config: Arc::new(tokio::sync::RwLock::new(
                crate::models::CircuitBreakerConfig::default(),
            )),
            refresh_locks: Arc::new(DashMap::new()),
            load_code_assist_inflight: Arc::new(DashMap::new()), // Initialize the inflight table
            invalid_grant_failures: Arc::new(DashMap::new()),
            auto_cleanup_handle: Arc::new(tokio::sync::Mutex::new(None)),
            cancel_token: CancellationToken::new(),
            image_scheduler: std::sync::RwLock::new(None),
        }
    }

    pub(crate) fn register_image_scheduler(&self, scheduler: &Arc<ImageScheduler>) {
        if let Ok(mut slot) = self.image_scheduler.write() {
            *slot = Some(Arc::downgrade(scheduler));
        }
        scheduler.sync_accounts(self.enabled_account_ids());
    }

    pub(crate) fn enabled_account_ids(&self) -> Vec<String> {
        self.tokens
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    fn sync_image_scheduler_accounts(&self) {
        let scheduler = self
            .image_scheduler
            .read()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(Weak::upgrade));
        if let Some(scheduler) = scheduler {
            scheduler.sync_accounts(self.enabled_account_ids());
        }
    }

    /// Start the background task that auto-cleans rate limit records (checks and clears expired records every 15 seconds)
    pub async fn start_auto_cleanup(&self) {
        let tracker = self.rate_limit_tracker.clone();
        let cancel = self.cancel_token.child_token();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("Auto-cleanup task received cancel signal");
                        break;
                    }
                    _ = interval.tick() => {
                        let cleaned = tracker.cleanup_expired();
                        if cleaned > 0 {
                            tracing::info!(
                                "Auto-cleanup: Removed {} expired rate limit record(s)",
                                cleaned
                            );
                        }
                    }
                }
            }
        });

        // Abort the old task first (to prevent task leaks), then store the new handle
        let mut guard = self.auto_cleanup_handle.lock().await;
        if let Some(old) = guard.take() {
            old.abort();
            tracing::warn!("Aborted previous auto-cleanup task");
        }
        *guard = Some(handle);

        tracing::info!("Rate limit auto-cleanup task started (interval: 15s)");
    }

    /// Load all accounts from the main app's accounts directory
    pub async fn load_accounts(&self) -> Result<usize, String> {
        let accounts_dir = self.data_dir.join("accounts");

        if !accounts_dir.exists() {
            return Err(format!("Accounts directory does not exist: {:?}", accounts_dir));
        }

        // Reload should reflect current on-disk state (accounts can be added/removed/disabled).
        self.tokens.clear();
        self.rate_limit_tracker.clear_all();
        self.sync_image_scheduler_accounts();
        self.current_index.store(0, Ordering::SeqCst);
        {
            let mut last_used = self.last_used_account.lock().await;
            *last_used = None;
        }

        let entries =
            std::fs::read_dir(&accounts_dir).map_err(|e| format!("Failed to read accounts directory: {}", e))?;

        let mut count = 0;

        for entry in entries {
            let entry = entry.map_err(|e| {
                self.sync_image_scheduler_accounts();
                format!("Failed to read directory entry: {}", e)
            })?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Try to load the account
            match self.load_single_account(&path).await {
                Ok(Some(token)) => {
                    let account_id = token.account_id.clone();
                    self.tokens.insert(account_id, token);
                    count += 1;
                }
                Ok(None) => {
                    // Skip invalid account
                }
                Err(e) => {
                    tracing::debug!("Failed to load account {:?}: {}", path, e);
                }
            }
        }

        self.sync_image_scheduler_accounts();
        Ok(count)
    }

    /// Reload a given account (used for real-time sync after a quota update)
    pub async fn reload_account(&self, account_id: &str) -> Result<(), String> {
        let path = self
            .data_dir
            .join("accounts")
            .join(format!("{}.json", account_id));
        if !path.exists() {
            return Err(format!("Account file does not exist: {:?}", path));
        }

        match self.load_single_account(&path).await {
            Ok(Some(token)) => {
                self.tokens.insert(account_id.to_string(), token);
                self.sync_image_scheduler_accounts();
                Ok(())
            }
            Ok(None) => {
                // [FIX] When the account is disabled or unavailable, fully remove it from the in-memory pool (Issue #1565)
                // load_single_account returning None means the account should be skipped in its
                // current state (disabled / proxy_disabled / quota_protection / validation_blocked...).
                self.remove_account(account_id);
                Ok(())
            }
            Err(e) => Err(format!("Failed to sync account: {}", e)),
        }
    }

    /// Reload all accounts
    pub async fn reload_all_accounts(&self) -> Result<usize, String> {
        self.load_accounts().await
    }

    /// Fully remove a given account and its associated data from memory (Issue #1477)
    pub fn remove_account(&self, account_id: &str) {
        // ... (original logic omitted)
        if self.tokens.remove(account_id).is_some() {
            tracing::info!("[Proxy] Removed account {} from memory cache", account_id);
        }
        self.health_scores.remove(account_id);
        self.rate_limit_tracker.clear(account_id);
        self.session_accounts.retain(|_, v| v != account_id);
        if let Ok(mut preferred) = self.preferred_account_id.try_write() {
            if preferred.as_deref() == Some(account_id) {
                *preferred = None;
                tracing::info!(
                    "[Proxy] Cleared preferred account status for {}",
                    account_id
                );
            }
        }
        self.sync_image_scheduler_accounts();
    }

    /// Get the full ProxyToken object by account ID (v4.1.29)
    pub fn get_token_by_id(&self, account_id: &str) -> Option<ProxyToken> {
        self.tokens.get(account_id).map(|t| t.clone())
    }

    /// Check if an account has been disabled on disk.
    ///
    /// Safety net: avoids selecting a disabled account when the in-memory pool hasn't been
    /// reloaded yet (e.g. fixed account mode / sticky session).
    ///
    /// Note: this is intentionally tolerant to transient read/parse failures (e.g. concurrent
    /// writes). Failures are reported as `Unknown` so callers can skip without purging the in-memory
    /// token pool.
    async fn get_account_state_on_disk(account_path: &std::path::PathBuf) -> OnDiskAccountState {
        const MAX_RETRIES: usize = 2;
        const RETRY_DELAY_MS: u64 = 5;

        for attempt in 0..=MAX_RETRIES {
            let content = match tokio::fs::read_to_string(account_path).await {
                Ok(c) => c,
                Err(e) => {
                    // If the file is gone, the in-memory token is definitely stale.
                    if e.kind() == std::io::ErrorKind::NotFound {
                        return OnDiskAccountState::Disabled;
                    }
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                        continue;
                    }
                    tracing::debug!(
                        "Failed to read account file on disk {:?}: {}",
                        account_path,
                        e
                    );
                    return OnDiskAccountState::Unknown;
                }
            };

            let account = match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(v) => v,
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                        continue;
                    }
                    tracing::debug!(
                        "Failed to parse account JSON on disk {:?}: {}",
                        account_path,
                        e
                    );
                    return OnDiskAccountState::Unknown;
                }
            };

            let disabled = account
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || account
                    .get("proxy_disabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                || account
                    .get("quota")
                    .and_then(|q| q.get("is_forbidden"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

            return if disabled {
                OnDiskAccountState::Disabled
            } else {
                OnDiskAccountState::Enabled
            };
        }

        OnDiskAccountState::Unknown
    }

    /// Load a single account
    async fn load_single_account(&self, path: &PathBuf) -> Result<Option<ProxyToken>, String> {
        let content = std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let mut account: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        // [Fix #1344] First check whether the account was manually disabled (for a reason other than quota protection)
        let is_proxy_disabled = account
            .get("proxy_disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let disabled_reason = account
            .get("proxy_disabled_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if is_proxy_disabled && disabled_reason != "quota_protection" {
            // Account manually disabled
            tracing::debug!(
                "Account skipped due to manual disable: {:?} (email={}, reason={})",
                path,
                account
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>"),
                disabled_reason
            );
            return Ok(None);
        }

        // [NEW] Check for validation block (VALIDATION_REQUIRED temporary block)
        if account
            .get("validation_blocked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let block_until = account
                .get("validation_blocked_until")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let now = chrono::Utc::now().timestamp();

            if now < block_until {
                // Still blocked
                tracing::debug!(
                    "Skipping validation-blocked account: {:?} (email={}, blocked until {})",
                    path,
                    account
                        .get("email")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<unknown>"),
                    chrono::DateTime::from_timestamp(block_until, 0)
                        .map(|dt| dt.format("%H:%M:%S").to_string())
                        .unwrap_or_else(|| block_until.to_string())
                );
                return Ok(None);
            } else {
                // Block expired - clear it
                account["validation_blocked"] = serde_json::json!(false);
                account["validation_blocked_until"] = serde_json::json!(0);
                account["validation_blocked_reason"] = serde_json::Value::Null;

                update_account_json(path, |latest| {
                    latest["validation_blocked"] = serde_json::json!(false);
                    latest["validation_blocked_until"] = serde_json::json!(0);
                    latest["validation_blocked_reason"] = serde_json::Value::Null;
                })
                .await?;
                tracing::info!(
                    "Validation block expired and cleared for account: {}",
                    account
                        .get("email")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<unknown>")
                );
            }
        }

        // Final check of the account's main enabled/disabled switch
        if account
            .get("disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Skipping disabled account file: {:?} (email={})",
                path,
                account
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>")
            );
            return Ok(None);
        }

        // Safety check: verify state on disk again to handle concurrent mid-parse writes
        if Self::get_account_state_on_disk(path).await == OnDiskAccountState::Disabled {
            tracing::debug!("Account file {:?} is disabled on disk, skipping.", path);
            return Ok(None);
        }

        // Quota protection check - only handles the quota protection logic
        // This way, an account whose quota has recovered gets auto-restored on load
        if self.check_and_protect_quota(&mut account, path).await {
            tracing::debug!(
                "Account skipped due to quota protection: {:?} (email={})",
                path,
                account
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>")
            );
            return Ok(None);
        }

        // [Compatibility] Re-confirm the final state (may have been modified by check_and_protect_quota)
        if account
            .get("proxy_disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Skipping proxy-disabled account file: {:?} (email={})",
                path,
                account
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>")
            );
            return Ok(None);
        }

        let account_id = account["id"].as_str().ok_or("Missing id field")?.to_string();

        let email = account["email"]
            .as_str()
            .ok_or("Missing email field")?
            .to_string();

        let token_obj = account["token"].as_object().ok_or("Missing token field")?;

        let access_token = token_obj["access_token"]
            .as_str()
            .ok_or("Missing access_token")?
            .to_string();

        let refresh_token = token_obj["refresh_token"]
            .as_str()
            .ok_or("Missing refresh_token")?
            .to_string();

        let expires_in = token_obj["expires_in"].as_i64().ok_or("Missing expires_in")?;

        let timestamp = token_obj["expiry_timestamp"]
            .as_i64()
            .ok_or("Missing expiry_timestamp")?;

        // project_id is optional
        let project_id = token_obj
            .get("project_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // [Added] Extract the subscription tier (subscription_tier is "FREE" | "PRO" | "ULTRA")
        let subscription_tier = account
            .get("quota")
            .and_then(|q| q.get("subscription_tier"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // [FIX #563] Extract the max remaining quota percentage for priority sorting (Option<i32> now)
        let remaining_quota = account
            .get("quota")
            .and_then(|q| self.calculate_quota_stats(q));
        // .filter(|&r| r > 0); // Removed the >0 filter, since 0% is also valid data, just lower priority

        // [Added #621] Extract the list of restricted models
        let protected_models: HashSet<String> = account
            .get("protected_models")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let health_score = self
            .health_scores
            .get(&account_id)
            .map(|v| *v)
            .unwrap_or(1.0);

        // [NEW] Extract the most recent quota refresh time (for sort optimization: sooner refresh = higher priority)
        let reset_time = self.extract_earliest_reset_time(&account);

        // [OPTIMIZATION] Build an in-memory cache of model quotas, to avoid reading disk during sorting
        let mut model_quotas = HashMap::new();
        // [NEW] Build an in-memory cache of per-model output limits (max_output_tokens)
        let mut model_limits: HashMap<String, u64> = HashMap::new();
        if let Some(models) = account
            .get("quota")
            .and_then(|q| q.get("models"))
            .and_then(|m| m.as_array())
        {
            for model in models {
                if let (Some(name), Some(pct)) = (
                    model.get("name").and_then(|v| v.as_str()),
                    model.get("percentage").and_then(|v| v.as_i64()),
                ) {
                    // Normalize name to standard ID
                    let standard_id =
                        crate::proxy::common::model_mapping::normalize_to_standard_id(name)
                            .unwrap_or_else(|| name.to_string());
                    model_quotas.insert(standard_id, pct as i32);
                }
                // [NEW] Parse and cache max_output_tokens (keyed by raw model name, not normalized)
                if let (Some(name), Some(limit)) = (
                    model.get("name").and_then(|v| v.as_str()),
                    model.get("max_output_tokens").and_then(|v| v.as_u64()),
                ) {
                    model_limits.insert(name.to_string(), limit);
                }
            }
        }

        if let Some(live_limits) = account
            .get("live_limited_models")
            .and_then(|value| value.as_object())
        {
            let now = chrono::Utc::now().timestamp();
            for (model_key, status) in live_limits {
                let Ok(status) = serde_json::from_value::<crate::models::account::LiveLimitStatus>(
                    status.clone(),
                ) else {
                    continue;
                };
                if !crate::proxy::rate_limit::is_active_persisted_long_image_limit(
                    model_key, &status, now,
                ) {
                    continue;
                }
                let (Ok(until_seconds), Ok(detected_at_seconds)) = (
                    u64::try_from(status.until),
                    u64::try_from(status.detected_at),
                ) else {
                    continue;
                };
                self.rate_limit_tracker.restore_persisted_long_image_limit(
                    &account_id,
                    std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(until_seconds),
                    std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(detected_at_seconds),
                    model_key,
                );
            }
        }

        // [NEW] On startup, automatically sync the persisted deprecated-model routing table and inject the hot-update interceptor
        if let Some(rules) = account
            .get("quota")
            .and_then(|q| q.get("model_forwarding_rules"))
            .and_then(|r| r.as_object())
        {
            for (k, v) in rules {
                if let Some(new_model) = v.as_str() {
                    // Register dynamic forwarding rules (including those mapping to gemini-pro-agent)
                    crate::proxy::common::model_mapping::update_dynamic_forwarding_rules(
                        k.to_string(),
                        new_model.to_string(),
                    );
                }
            }
        }

        Ok(Some(ProxyToken {
            account_id,
            access_token,
            refresh_token,
            expires_in,
            timestamp,
            email,
            account_path: path.clone(),
            project_id,
            subscription_tier,
            remaining_quota,
            protected_models,
            health_score,
            reset_time,
            validation_blocked: account
                .get("validation_blocked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            validation_blocked_until: account
                .get("validation_blocked_until")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            validation_url: account
                .get("validation_url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            model_quotas,
            model_limits,
        }))
    }

    /// Check whether an account should be quota-protected
    /// If the quota is below the threshold, automatically disable the account and return true
    async fn check_and_protect_quota(
        &self,
        account_json: &mut serde_json::Value,
        account_path: &PathBuf,
    ) -> bool {
        // 1. Load the quota protection config
        let config = match crate::modules::config::load_app_config() {
            Ok(cfg) => cfg.quota_protection,
            Err(_) => return false, // Config load failed, skip protection
        };

        if !config.enabled {
            return false; // Quota protection not enabled
        }

        // 2. Get the quota info
        // Note: we need to clone the quota info to iterate over it, to avoid borrow conflicts, but the mutation targets account_json
        let quota = match account_json.get("quota") {
            Some(q) => q.clone(),
            None => return false, // No quota info, skip
        };

        // 3. [Compatibility #621] Check whether it was disabled by the legacy account-level quota protection, try to restore and migrate to model level
        let is_proxy_disabled = account_json
            .get("proxy_disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let reason = account_json
            .get("proxy_disabled_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if is_proxy_disabled && reason == "quota_protection" {
            // If it was disabled by the legacy account-level protection, try to restore and migrate to model level
            return self
                .check_and_restore_quota(account_json, account_path, &quota, &config)
                .await;
        }

        // [Fix #1344] No longer handles other disable reasons; the caller is responsible for checking manual disable

        // 4. Get the model list
        let models = match quota.get("models").and_then(|m| m.as_array()) {
            Some(m) => m,
            None => return false,
        };

        // 5. [Refactor] Aggregate determination logic: group all of the account's model variants by Standard ID
        // This solves the state conflict caused by e.g. Pro-Low (0%) and Pro-High (100%) coexisting within the same account
        let mut group_max_percentage: HashMap<String, i32> = HashMap::new();

        for model in models {
            let name = model.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let percentage = model
                .get("percentage")
                .and_then(|v| v.as_i64())
                .unwrap_or(100) as i32;

            if let Some(std_id) =
                crate::proxy::common::model_mapping::normalize_to_standard_id(name)
            {
                let entry = group_max_percentage.entry(std_id).or_insert(-1);
                if percentage > *entry {
                    *entry = percentage;
                }
            }
        }

        // 6. Iterate over the monitored Standard IDs, and lock or restore based on the group's "best state"
        let threshold = config.threshold_percentage as i32;
        let account_id = account_json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let mut changed = false;

        for std_id in &config.monitored_models {
            // Get the group's highest percentage; if the account has no model in that group, treat it as 100%
            let max_pct = group_max_percentage.get(std_id).cloned().unwrap_or(100);

            if max_pct < threshold {
                // Only trigger group-wide protection if every model in the group is below threshold
                if self
                    .trigger_quota_protection(
                        account_json,
                        &account_id,
                        account_path,
                        max_pct,
                        threshold,
                        std_id,
                    )
                    .await
                    .unwrap_or(false)
                {
                    changed = true;
                }
            } else {
                // Only try to restore from a previously restricted state if the whole group is fine (or the model doesn't exist for this account)
                let protected_models = account_json
                    .get("protected_models")
                    .and_then(|v| v.as_array());

                let is_protected = protected_models.map_or(false, |arr| {
                    arr.iter().any(|m| m.as_str() == Some(std_id as &str))
                });

                if is_protected {
                    if self
                        .restore_quota_protection(account_json, &account_id, account_path, std_id)
                        .await
                        .unwrap_or(false)
                    {
                        changed = true;
                    }
                }
            }
        }

        let _ = changed; // Avoid an unused warning; can continue to be used if later logic needs it

        // We no longer return true for quota reasons (i.e. no longer skip the account),
        // instead we load it and filter during get_token.
        false
    }

    /// Compute the account's max remaining quota percentage (used for sorting)
    /// Return value: Option<i32> (max_percentage)
    fn calculate_quota_stats(&self, quota: &serde_json::Value) -> Option<i32> {
        let models = match quota.get("models").and_then(|m| m.as_array()) {
            Some(m) => m,
            None => return None,
        };

        let mut max_percentage = 0;
        let mut has_data = false;

        for model in models {
            if let Some(pct) = model.get("percentage").and_then(|v| v.as_i64()) {
                let pct_i32 = pct as i32;
                if pct_i32 > max_percentage {
                    max_percentage = pct_i32;
                }
                has_data = true;
            }
        }

        if has_data {
            Some(max_percentage)
        } else {
            None
        }
    }

    /// Read a specific model's quota percentage from disk [FIX] Sorting uses the target model's quota, not the max
    ///
    /// # Parameters
    /// * `account_path` - the account JSON file path
    /// * `model_name` - the target model name (already normalized)
    #[allow(dead_code)] // Reserved for precise quota-reading logic
    fn get_model_quota_from_json(account_path: &PathBuf, model_name: &str) -> Option<i32> {
        let content = std::fs::read_to_string(account_path).ok()?;
        let account: serde_json::Value = serde_json::from_str(&content).ok()?;
        let models = account.get("quota")?.get("models")?.as_array()?;

        for model in models {
            if let Some(name) = model.get("name").and_then(|v| v.as_str()) {
                if crate::proxy::common::model_mapping::normalize_to_standard_id(name)
                    .unwrap_or_else(|| name.to_string())
                    == model_name
                {
                    return model
                        .get("percentage")
                        .and_then(|v| v.as_i64())
                        .map(|p| p as i32);
                }
            }
        }
        None
    }

    fn get_available_models_from_json(account_path: &PathBuf) -> Option<HashSet<String>> {
        let content = std::fs::read_to_string(account_path).ok()?;
        let account: serde_json::Value = serde_json::from_str(&content).ok()?;
        let models = account.get("quota")?.get("models")?.as_array()?;
        let mut result = HashSet::new();
        for model in models {
            if let Some(name) = model.get("name").and_then(|v| v.as_str()) {
                let normalized = name.trim().to_lowercase();
                if !normalized.is_empty() {
                    result.insert(normalized);
                }
            }
        }
        Some(result)
    }

    fn build_dynamic_model_candidates(model_name: &str) -> Option<Vec<String>> {
        let model = model_name.trim().to_lowercase();
        if model.is_empty() {
            return None;
        }

        // Image models: drift ONLY across versions within the SAME tier
        // (pro-image ↔ pro-image, flash-image ↔ flash-image). Never silently downgrade
        // pro→flash. If the account has no model in the requested tier, the name is left
        // unchanged and upstream returns 404 — which is honest (the account lacks that model).
        // To alias e.g. gemini-3-pro-image to a flash model, use the app's Model Routing Center.
        let pro_image = ["gemini-3-pro-image", "gemini-3.1-pro-image"];
        let flash_image = ["gemini-3-flash-image", "gemini-3.1-flash-image"];
        let is_pro_image = pro_image.contains(&model.as_str());
        let is_flash_image = flash_image.contains(&model.as_str());
        if is_pro_image || is_flash_image {
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            let mut push = |candidate: &str| {
                let c = candidate.to_string();
                if seen.insert(c.clone()) {
                    out.push(c);
                }
            };
            push(&model); // requested first
            if is_pro_image {
                push("gemini-3.1-pro-image");
                push("gemini-3-pro-image");
            } else {
                push("gemini-3.1-flash-image");
                push("gemini-3-flash-image");
            }
            return Some(out);
        }

        let pro_family = [
            "gemini-3-pro",
            "gemini-3-pro-preview",
            "gemini-3-pro-high",
            "gemini-3-pro-low",
            "gemini-3.1-pro",
            "gemini-3.1-pro-preview",
            "gemini-3.1-pro-high",
            "gemini-3.1-pro-low",
            "gemini-pro-agent",
        ];

        if !pro_family.contains(&model.as_str()) {
            return None;
        }

        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut push = |candidate: &str| {
            let c = candidate.to_string();
            if seen.insert(c.clone()) {
                out.push(c);
            }
        };

        // Keep requested model as top priority, then fallback across the same family.
        push(&model);
        push("gemini-pro-agent");
        push("gemini-3.1-pro-preview");
        push("gemini-3-pro-preview");
        push("gemini-3.1-pro-high");
        push("gemini-3-pro-high");
        push("gemini-3.1-pro-low");
        push("gemini-3-pro-low");

        Some(out)
    }

    pub async fn resolve_dynamic_model_for_account(
        &self,
        account_id: &str,
        mapped_model: &str,
    ) -> String {
        let candidates = match Self::build_dynamic_model_candidates(mapped_model) {
            Some(c) => c,
            None => return mapped_model.to_string(),
        };

        let account_path = match self.tokens.get(account_id) {
            Some(token) => token.account_path.clone(),
            None => return mapped_model.to_string(),
        };

        let available_models = match Self::get_available_models_from_json(&account_path) {
            Some(models) if !models.is_empty() => models,
            _ => return mapped_model.to_string(),
        };

        for candidate in candidates {
            if available_models.contains(&candidate) {
                if candidate != mapped_model.to_lowercase() {
                    tracing::info!(
                        "[Dynamic-Model-Rewrite] account={} {} -> {}",
                        account_id,
                        mapped_model,
                        candidate
                    );
                }
                return candidate;
            }
        }

        mapped_model.to_string()
    }

    /// Test helper function: expose access to get_model_quota_from_json
    #[cfg(test)]
    pub fn get_model_quota_from_json_for_test(
        account_path: &PathBuf,
        model_name: &str,
    ) -> Option<i32> {
        Self::get_model_quota_from_json(account_path, model_name)
    }

    /// Trigger quota protection, restricting a specific model (Issue #621)
    /// Returns true if a change occurred
    async fn trigger_quota_protection(
        &self,
        account_json: &mut serde_json::Value,
        account_id: &str,
        account_path: &PathBuf,
        current_val: i32,
        threshold: i32,
        model_name: &str,
    ) -> Result<bool, String> {
        // 1. Initialize the protected_models array (if it doesn't exist)
        if account_json.get("protected_models").is_none() {
            account_json["protected_models"] = serde_json::Value::Array(Vec::new());
        }

        let protected_models = account_json["protected_models"].as_array_mut().unwrap();

        // 2. Check whether it already exists
        if !protected_models
            .iter()
            .any(|m| m.as_str() == Some(model_name))
        {
            protected_models.push(serde_json::Value::String(model_name.to_string()));

            tracing::info!(
                "Account {}'s model {} was added to the protection list due to quota limits ({}% < {}%)",
                account_id,
                model_name,
                current_val,
                threshold
            );

            // 3. Write to disk
            let model_name_owned = model_name.to_string();
            update_account_json(account_path, move |latest| {
                if latest
                    .get("protected_models")
                    .and_then(|value| value.as_array())
                    .is_none()
                {
                    latest["protected_models"] = serde_json::Value::Array(Vec::new());
                }
                let protected_models = latest["protected_models"].as_array_mut().unwrap();
                if !protected_models
                    .iter()
                    .any(|model| model.as_str() == Some(&model_name_owned))
                {
                    protected_models.push(serde_json::Value::String(model_name_owned));
                }
            })
            .await?;

            // [FIX] Trigger the TokenManager account reload signal, to ensure the in-memory protected_models stays in sync
            crate::proxy::server::trigger_account_reload(account_id);

            return Ok(true);
        }

        Ok(false)
    }

    /// Check and restore from account-level protection (migrating to model level, Issue #621)
    async fn check_and_restore_quota(
        &self,
        account_json: &mut serde_json::Value,
        account_path: &PathBuf,
        quota: &serde_json::Value,
        config: &crate::models::QuotaProtectionConfig,
    ) -> bool {
        // [Compatibility] If this account currently has proxy_disabled=true with reason quota_protection,
        // we set its proxy_disabled to false, while also updating its protected_models list.
        tracing::info!(
            "Migrating account {} from global quota protection mode to model-level protection mode",
            account_json
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        );

        account_json["proxy_disabled"] = serde_json::Value::Bool(false);
        account_json["proxy_disabled_reason"] = serde_json::Value::Null;
        account_json["proxy_disabled_at"] = serde_json::Value::Null;

        let threshold = config.threshold_percentage as i32;
        let mut protected_list = Vec::new();

        if let Some(models) = quota.get("models").and_then(|m| m.as_array()) {
            let mut group_max_percentage: HashMap<String, i32> = HashMap::new();

            for model in models {
                let name = model.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let percentage = model
                    .get("percentage")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;

                if let Some(std_id) =
                    crate::proxy::common::model_mapping::normalize_to_standard_id(name)
                {
                    let entry = group_max_percentage.entry(std_id).or_insert(-1);
                    if percentage > *entry {
                        *entry = percentage;
                    }
                }
            }

            for std_id in &config.monitored_models {
                let max_pct = group_max_percentage.get(std_id).cloned().unwrap_or(100);
                if max_pct < threshold {
                    protected_list.push(serde_json::Value::String(std_id.clone()));
                }
            }
        }

        account_json["protected_models"] = serde_json::Value::Array(protected_list.clone());

        let _ = update_account_json(account_path, |latest| {
            latest["proxy_disabled"] = serde_json::Value::Bool(false);
            latest["proxy_disabled_reason"] = serde_json::Value::Null;
            latest["proxy_disabled_at"] = serde_json::Value::Null;
            latest["protected_models"] = serde_json::Value::Array(protected_list);
        })
        .await;

        false // Returns false, meaning the account can now be attempted for loading (model-level filtering happens during get_token)
    }

    /// Restore quota protection for a specific model (Issue #621)
    /// Returns true if a change occurred
    async fn restore_quota_protection(
        &self,
        account_json: &mut serde_json::Value,
        account_id: &str,
        account_path: &PathBuf,
        model_name: &str,
    ) -> Result<bool, String> {
        if let Some(arr) = account_json
            .get_mut("protected_models")
            .and_then(|v| v.as_array_mut())
        {
            let original_len = arr.len();
            arr.retain(|m| m.as_str() != Some(model_name));

            if arr.len() < original_len {
                tracing::info!(
                    "Account {}'s model {} quota has recovered, removed from the protection list",
                    account_id,
                    model_name
                );
                let model_name_owned = model_name.to_string();
                update_account_json(account_path, move |latest| {
                    if let Some(protected_models) = latest
                        .get_mut("protected_models")
                        .and_then(|value| value.as_array_mut())
                    {
                        protected_models.retain(|model| model.as_str() != Some(&model_name_owned));
                    }
                })
                .await?;
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Candidate pool size for the P2C algorithm - randomly select from the top N best candidates
    const P2C_POOL_SIZE: usize = 5;

    /// Power of 2 Choices (P2C) selection algorithm
    /// Randomly pick 2 from the top 5 candidates, choose the one with higher quota -> avoids hotspots
    /// Returns the selected index
    ///
    /// # Parameters
    /// * `candidates` - the sorted list of candidate tokens
    /// * `attempted` - the set of account IDs that have already been tried and failed
    /// * `normalized_target` - the normalized target model name
    /// * `quota_protection_enabled` - whether quota protection is enabled
    fn select_with_p2c<'a>(
        &self,
        candidates: &'a [ProxyToken],
        attempted: &HashSet<String>,
        normalized_target: &str,
        quota_protection_enabled: bool,
    ) -> Option<&'a ProxyToken> {
        use rand::Rng;

        // Filter usable tokens
        let available: Vec<&ProxyToken> = candidates
            .iter()
            .filter(|t| !attempted.contains(&t.account_id))
            .filter(|t| {
                !quota_protection_enabled || !t.protected_models.contains(normalized_target)
            })
            .collect();

        if available.is_empty() {
            return None;
        }
        if available.len() == 1 {
            return Some(available[0]);
        }

        // P2C: randomly pick 2 from the first min(P2C_POOL_SIZE, len) candidates
        let pool_size = available.len().min(Self::P2C_POOL_SIZE);
        let mut rng = rand::thread_rng();

        let pick1 = rng.gen_range(0..pool_size);
        let pick2 = rng.gen_range(0..pool_size);
        // Ensure two distinct candidates are chosen
        let pick2 = if pick2 == pick1 {
            (pick1 + 1) % pool_size
        } else {
            pick2
        };

        let c1 = available[pick1];
        let c2 = available[pick2];

        // Choose the one with the higher quota
        let selected = if c1.remaining_quota.unwrap_or(0) >= c2.remaining_quota.unwrap_or(0) {
            c1
        } else {
            c2
        };

        tracing::debug!(
            "🎲 [P2C] Selected {} ({}%) from [{}({}%), {}({}%)]",
            selected.email,
            selected.remaining_quota.unwrap_or(0),
            c1.email,
            c1.remaining_quota.unwrap_or(0),
            c2.email,
            c2.remaining_quota.unwrap_or(0)
        );

        Some(selected)
    }

    /// Send the cancellation signal first, then wait for the tasks to finish with a timeout
    ///
    /// # Parameters
    /// * `timeout` - the timeout for waiting on tasks to finish
    pub async fn graceful_shutdown(&self, timeout: std::time::Duration) {
        tracing::info!("Initiating graceful shutdown of background tasks...");

        // Send the cancellation signal to all background tasks
        self.cancel_token.cancel();

        // Wait for tasks to finish with a timeout
        match tokio::time::timeout(timeout, self.abort_background_tasks()).await {
            Ok(_) => tracing::info!("All background tasks cleaned up gracefully"),
            Err(_) => tracing::warn!(
                "Graceful cleanup timed out after {:?}, tasks were force-aborted",
                timeout
            ),
        }
    }

    /// Abort and wait for all background tasks to finish
    /// abort() only sets the cancellation flag; you must await to confirm cleanup is complete
    pub async fn abort_background_tasks(&self) {
        Self::abort_task(&self.auto_cleanup_handle, "Auto-cleanup task").await;
    }

    /// Abort a single background task and log the result
    ///
    /// # Parameters
    /// * `handle` - a Mutex reference to the task handle
    /// * `task_name` - the task name (used for logging)
    async fn abort_task(
        handle: &tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
        task_name: &str,
    ) {
        let Some(handle) = handle.lock().await.take() else {
            return;
        };

        handle.abort();
        match handle.await {
            Ok(()) => tracing::debug!("{} completed", task_name),
            Err(e) if e.is_cancelled() => tracing::info!("{} aborted", task_name),
            Err(e) => tracing::warn!("{} error: {}", task_name, e),
        }
    }

    /// Get the currently available Token (supports sticky sessions and smart scheduling)
    /// Parameter `quota_group` distinguishes the "claude" vs "gemini" group
    /// When `force_rotate` is true, ignores locking and force-switches accounts
    /// Parameter `session_id` is used to maintain session stickiness across requests
    /// Parameter `target_model` is used to check quota protection (Issue #621)
    pub async fn get_token(
        &self,
        quota_group: &str,
        force_rotate: bool,
        session_id: Option<&str>,
        target_model: &str,
    ) -> Result<(String, String, String, String, u64), String> {
        let excluded_accounts = HashSet::new();
        self.get_token_filtered(
            quota_group,
            force_rotate,
            session_id,
            target_model,
            &excluded_accounts,
        )
        .await
    }

    pub async fn get_image_token(
        &self,
        force_rotate: bool,
        session_id: Option<&str>,
        target_model: &str,
        scheduler: &Arc<ImageScheduler>,
        request_timeout: u64,
    ) -> Result<(String, String, String, String, u64, ImagePermit), (StatusCode, String)> {
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(request_timeout);
        let mut scheduler_changes = scheduler.subscribe_changes();

        loop {
            scheduler_changes.borrow_and_update();
            let mut busy_accounts = HashSet::new();

            loop {
                let selection = wait_for_image_token_selection(
                    deadline,
                    self.get_token_filtered(
                        "image_gen",
                        force_rotate,
                        session_id,
                        target_model,
                        &busy_accounts,
                    ),
                )
                .await;
                match selection {
                    None => {
                        return Err((
                            StatusCode::TOO_MANY_REQUESTS,
                            "Image queue wait timed out".to_string(),
                        ));
                    }
                    Some(Ok((access_token, project_id, email, account_id, wait_ms))) => {
                        if let Some(permit) = scheduler.try_acquire(&account_id) {
                            return Ok((
                                access_token,
                                project_id,
                                email,
                                account_id,
                                wait_ms,
                                permit,
                            ));
                        }
                        busy_accounts.insert(account_id);
                    }
                    Some(Err(selection_error)) => {
                        if busy_accounts.is_empty() {
                            return Err((
                                StatusCode::SERVICE_UNAVAILABLE,
                                format!("Token error: {}", selection_error),
                            ));
                        }
                        break;
                    }
                }
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if !wait_for_image_account_change(&mut scheduler_changes, remaining).await {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    "Image queue wait timed out".to_string(),
                ));
            }
        }
    }

    async fn get_token_filtered(
        &self,
        quota_group: &str,
        force_rotate: bool,
        session_id: Option<&str>,
        target_model: &str,
        excluded_accounts: &HashSet<String>,
    ) -> Result<(String, String, String, String, u64), String> {
        // [FIX] Check and process accounts pending reload (quota protection sync)
        let pending_reload = crate::proxy::server::take_pending_reload_accounts();
        for account_id in pending_reload {
            if let Err(e) = self.reload_account(&account_id).await {
                tracing::warn!("[Quota] Failed to reload account {}: {}", account_id, e);
            } else {
                tracing::info!(
                    "[Quota] Reloaded account {} (protected_models synced)",
                    account_id
                );
            }
        }

        // [FIX #1477] Check and process accounts pending deletion (full cache purge)
        let pending_delete = crate::proxy::server::take_pending_delete_accounts();
        for account_id in pending_delete {
            self.remove_account(&account_id);
            tracing::info!(
                "[Proxy] Purged deleted account {} from all caches",
                account_id
            );
        }

        // [Optimization Issue #284] Add a 5-second timeout, to prevent deadlock
        let timeout_duration = std::time::Duration::from_secs(5);
        match tokio::time::timeout(
            timeout_duration,
            self.get_token_internal(
                quota_group,
                force_rotate,
                session_id,
                target_model,
                excluded_accounts,
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(
                "Token acquisition timeout (5s) - system too busy or deadlock detected".to_string(),
            ),
        }
    }

    /// Internal implementation: the core logic for acquiring a Token
    async fn get_token_internal(
        &self,
        quota_group: &str,
        force_rotate: bool,
        session_id: Option<&str>,
        target_model: &str,
        excluded_accounts: &HashSet<String>,
    ) -> Result<(String, String, String, String, u64), String> {
        let mut tokens_snapshot: Vec<ProxyToken> =
            self.tokens.iter().map(|e| e.value().clone()).collect();
        tokens_snapshot.retain(|token| !excluded_accounts.contains(&token.account_id));
        let mut total = tokens_snapshot.len();
        if total == 0 {
            return Err("Token pool is empty".to_string());
        }

        // [NEW] 1. Dynamic capability filtering (Capability Filter)

        // Define constants
        const RESET_TIME_THRESHOLD_SECS: i64 = 600; // 10-minute threshold

        // Normalize the target model name to its standard ID
        let normalized_target =
            crate::proxy::common::model_mapping::normalize_to_standard_id(target_model)
                .unwrap_or_else(|| target_model.to_string());

        // Keep only accounts that explicitly have quota for this model
        // This step ensures "only accounts with the model can enter the rotation", especially for premium models like Opus 4.6
        let candidate_count_before = tokens_snapshot.len();

        // This assumes every supported model appears in model_quotas
        // If the API's quota info is incomplete this could cause false exclusions, but we apply this filter for strictness
        tokens_snapshot.retain(|t| t.model_quotas.contains_key(&normalized_target));

        if tokens_snapshot.is_empty() {
            if candidate_count_before > 0 {
                // If there were accounts before filtering but none after, it means no account has quota for this model
                tracing::warn!(
                    "No accounts have satisfied quota for model: {}",
                    normalized_target
                );
                return Err(format!(
                    "No accounts available with quota for model: {}",
                    normalized_target
                ));
            }
            return Err("Token pool is empty".to_string());
        }

        tokens_snapshot.sort_by(|a, b| {
            // Priority 0: strict subscription tier ordering (ULTRA > PRO > FREE)
            // User requirement: rotation should follow Ultra -> Pro -> Free
            // Since accounts not supporting this model have already been filtered out, the rest all support it
            // At this point we prefer the higher-tier subscription
            let tier_priority = |tier: &Option<String>| {
                let t = tier.as_deref().unwrap_or("").to_lowercase();
                if t.contains("ultra") {
                    0
                } else if t.contains("pro") {
                    1
                } else if t.contains("free") {
                    2
                } else {
                    3
                }
            };

            let tier_cmp =
                tier_priority(&a.subscription_tier).cmp(&tier_priority(&b.subscription_tier));
            if tier_cmp != std::cmp::Ordering::Equal {
                return tier_cmp;
            }

            // Priority 1: the target model's quota (higher is better) -> protects low-quota accounts
            // After filtering, the key is guaranteed to exist
            let quota_a = a.model_quotas.get(&normalized_target).copied().unwrap_or(0);
            let quota_b = b.model_quotas.get(&normalized_target).copied().unwrap_or(0);

            let quota_cmp = quota_b.cmp(&quota_a);
            if quota_cmp != std::cmp::Ordering::Equal {
                return quota_cmp;
            }

            // Priority 2: Health score (higher is better)
            let health_cmp = b
                .health_score
                .partial_cmp(&a.health_score)
                .unwrap_or(std::cmp::Ordering::Equal);
            if health_cmp != std::cmp::Ordering::Equal {
                return health_cmp;
            }

            // Priority 3: Reset time (earlier is better, but only if diff > 10 min)
            let reset_a = a.reset_time.unwrap_or(i64::MAX);
            let reset_b = b.reset_time.unwrap_or(i64::MAX);
            if (reset_a - reset_b).abs() >= RESET_TIME_THRESHOLD_SECS {
                reset_a.cmp(&reset_b)
            } else {
                std::cmp::Ordering::Equal
            }
        });

        // [Debug log] Print the sorted account order (showing the target model's quota)
        tracing::debug!(
            "🔄 [Token Rotation] target={} Accounts: {:?}",
            normalized_target,
            tokens_snapshot
                .iter()
                .map(|t| format!(
                    "{}(quota={}%, reset={:?}, health={:.2})",
                    t.email,
                    t.model_quotas.get(&normalized_target).copied().unwrap_or(0),
                    t.reset_time.map(|ts| {
                        let now = chrono::Utc::now().timestamp();
                        let diff_secs = ts - now;
                        if diff_secs > 0 {
                            format!("{}m", diff_secs / 60)
                        } else {
                            "now".to_string()
                        }
                    }),
                    t.health_score
                ))
                .collect::<Vec<_>>()
        );

        // 0. Read the current scheduling config
        let scheduling = self.sticky_config.read().await.clone();
        use crate::proxy::sticky_config::SchedulingMode;

        // [Added] Check whether quota protection is enabled (if off, ignore the protected_models check)
        let quota_protection_enabled = crate::modules::config::load_app_config()
            .map(|cfg| cfg.quota_protection.enabled)
            .unwrap_or(false);

        // ===== [FIX #820] Fixed account mode: prefer the specified account =====
        let preferred_id = self.preferred_account_id.read().await.clone();
        if let Some(ref pref_id) = preferred_id {
            // Look up the preferred account
            if let Some(preferred_token) = tokens_snapshot
                .iter()
                .find(|t| &t.account_id == pref_id)
                .cloned()
            {
                // Check whether the account is usable (not rate limited, not quota-protected)
                match Self::get_account_state_on_disk(&preferred_token.account_path).await {
                    OnDiskAccountState::Disabled => {
                        tracing::warn!(
                            "🔒 [FIX #820] Preferred account {} is disabled on disk, purging and falling back",
                            preferred_token.email
                        );
                        self.remove_account(&preferred_token.account_id);
                        tokens_snapshot.retain(|t| t.account_id != preferred_token.account_id);
                        total = tokens_snapshot.len();

                        {
                            let mut preferred = self.preferred_account_id.write().await;
                            if preferred.as_deref() == Some(pref_id.as_str()) {
                                *preferred = None;
                            }
                        }

                        if total == 0 {
                            return Err("Token pool is empty".to_string());
                        }
                    }
                    OnDiskAccountState::Unknown => {
                        tracing::warn!(
                            "🔒 [FIX #820] Preferred account {} state on disk is unavailable, falling back",
                            preferred_token.email
                        );
                        // Don't purge on transient read/parse failures; just skip this token for this request.
                        tokens_snapshot.retain(|t| t.account_id != preferred_token.account_id);
                        total = tokens_snapshot.len();
                        if total == 0 {
                            return Err("Token pool is empty".to_string());
                        }
                    }
                    OnDiskAccountState::Enabled => {
                        let normalized_target =
                            crate::proxy::common::model_mapping::normalize_to_standard_id(
                                target_model,
                            )
                            .unwrap_or_else(|| target_model.to_string());

                        let is_rate_limited = self
                            .is_rate_limited(&preferred_token.account_id, Some(&normalized_target))
                            .await;
                        let is_quota_protected = quota_protection_enabled
                            && preferred_token
                                .protected_models
                                .contains(&normalized_target);

                        if !is_rate_limited && !is_quota_protected {
                            tracing::info!(
                                "🔒 [FIX #820] Using preferred account: {} (fixed mode)",
                                preferred_token.email
                            );

                            // Use the preferred account directly, skipping the rotation logic
                            let mut token = preferred_token.clone();

                            // [NEW] Check whether the token has expired (refresh timing aligned with the official 90s grace period)
                            let now = chrono::Utc::now().timestamp();
                            if now >= token.timestamp - 90 {
                                // [NEW] Double-checked locking logic (Double-Checked Locking)
                                // 1. Get (or create) this account's dedicated refresh lock
                                let refresh_mu = self
                                    .refresh_locks
                                    .entry(token.account_id.clone())
                                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                                    .clone();

                                // 2. Try to acquire the lock
                                let _guard = refresh_mu.lock().await;

                                // 3. Re-check this account's latest state (may already have been refreshed by another concurrent request)
                                let latest_token_opt =
                                    self.tokens.get(&token.account_id).map(|r| r.clone());
                                if let Some(latest) = latest_token_opt {
                                    if now < latest.timestamp - 90 {
                                        // Already refreshed by someone else; sync the latest data and skip the refresh action
                                        token = latest.clone();
                                        tracing::debug!(
                                            "Account {} was already refreshed by a concurrent thread, skipping duplicate refresh",
                                            token.email
                                        );
                                    } else {
                                        // Refresh is genuinely needed
                                        tracing::debug!(
                                            "Account {}'s token is about to expire ({}s), refreshing...",
                                            token.email,
                                            token.timestamp - now
                                        );
                                        match crate::modules::oauth::refresh_access_token(
                                            &token.refresh_token,
                                            Some(&token.account_id),
                                        )
                                        .await
                                        {
                                            Ok(token_response) => {
                                                token.access_token =
                                                    token_response.access_token.clone();
                                                token.expires_in = token_response.expires_in;
                                                token.timestamp = now + token_response.expires_in;

                                                if let Some(mut entry) =
                                                    self.tokens.get_mut(&token.account_id)
                                                {
                                                    entry.access_token = token.access_token.clone();
                                                    entry.expires_in = token.expires_in;
                                                    entry.timestamp = token.timestamp;
                                                }
                                                // [FIX] Backgrounded the disk write: avoids blocking get_token's 5s timeout window
                                                // Memory has already been updated; the disk persistence is spawned onto the blocking thread pool
                                                {
                                                    let write_path = token.account_path.clone();
                                                    let access_token = token_response.access_token.clone();
                                                    let expires_in = token_response.expires_in;
                                                    let id_token = token_response.id_token.clone();
                                                    let new_rt = token_response.refresh_token.clone();
                                                    let write_ts = now + token_response.expires_in;
                                                    tokio::task::spawn_blocking(move || {
                                                        let Ok(_lk) = crate::modules::account::lock_account_file_updates() else { return; };
                                                        let Ok(raw) = std::fs::read_to_string(&write_path) else { return; };
                                                        let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&raw) else { return; };
                                                        val["token"]["access_token"] = access_token.into();
                                                        val["token"]["expires_in"] = expires_in.into();
                                                        val["token"]["expiry_timestamp"] = write_ts.into();
                                                        if let Some(it) = id_token { val["token"]["id_token"] = it.into(); }
                                                        if let Some(rt) = new_rt { val["token"]["refresh_token"] = rt.into(); }
                                                        if let Ok(s) = serde_json::to_string_pretty(&val) { let _ = std::fs::write(&write_path, s); }
                                                    });
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Preferred account token refresh failed: {}",
                                                    e
                                                );
                                                // Continue using the old token, letting downstream logic handle the failure
                                            }
                                        }
                                    }
                                }
                            }

                            // Ensure project_id is present (filter empty strings to trigger re-fetch)
                            let project_id = if let Some(pid) = &token.project_id {
                                if pid.is_empty() {
                                    None
                                } else {
                                    Some(pid.clone())
                                }
                            } else {
                                None
                            };
                            let project_id = if let Some(pid) = project_id {
                                pid
                            } else {
                                match crate::proxy::project_resolver::fetch_project_id(
                                    &token.access_token,
                                )
                                .await
                                {
                                    Ok(pid) => {
                                        if let Some(mut entry) =
                                            self.tokens.get_mut(&token.account_id)
                                        {
                                            entry.project_id = Some(pid.clone());
                                        }
                                        // [FIX] Backgrounded the disk write: project_id has already been written to memory, disk persistence doesn't block the hot path
                                        {
                                            let write_path = token.account_path.clone();
                                            let pid_clone = pid.clone();
                                            tokio::task::spawn_blocking(move || {
                                                let Ok(_lk) = crate::modules::account::lock_account_file_updates() else { return; };
                                                let Ok(raw) = std::fs::read_to_string(&write_path) else { return; };
                                                let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&raw) else { return; };
                                                val["token"]["project_id"] = pid_clone.into();
                                                if let Ok(s) = serde_json::to_string_pretty(&val) { let _ = std::fs::write(&write_path, s); }
                                            });
                                        }
                                        pid
                                    }
                                    Err(_) => "bamboo-precept-lgxtn".to_string(), // fallback
                                }
                            };

                            return Ok((
                                token.access_token,
                                project_id,
                                token.email,
                                token.account_id,
                                0,
                            ));
                        } else {
                            if is_rate_limited {
                                tracing::warn!("🔒 [FIX #820] Preferred account {} is rate-limited, falling back to round-robin", preferred_token.email);
                            } else {
                                tracing::warn!("🔒 [FIX #820] Preferred account {} is quota-protected for {}, falling back to round-robin", preferred_token.email, target_model);
                            }
                        }
                    }
                }
            } else {
                tracing::warn!("🔒 [FIX #820] Preferred account {} not found in pool, falling back to round-robin", pref_id);
            }
        }
        // ===== [END FIX #820] =====

        // [Optimization Issue #284] Move the lock operation outside the loop, to avoid repeated lock acquisition
        // Pre-fetch a snapshot of last_used_account, to avoid locking repeatedly inside the loop
        let last_used_account_id = if quota_group != "image_gen" {
            let last_used = self.last_used_account.lock().await;
            last_used.clone()
        } else {
            None
        };

        let mut attempted: HashSet<String> = HashSet::new();
        let mut last_error: Option<String> = None;
        let mut need_update_last_used: Option<(String, std::time::Instant)> = None;

        for attempt in 0..total {
            let rotate = force_rotate || attempt > 0;

            // ===== [Core] Sticky session and smart scheduling logic =====
            let mut target_token: Option<ProxyToken> = None;

            // Normalize the target model name to its standard ID, used for the quota protection check
            let normalized_target =
                crate::proxy::common::model_mapping::normalize_to_standard_id(target_model)
                    .unwrap_or_else(|| target_model.to_string());

            // Mode A: sticky session handling (CacheFirst or Balance, with a session_id)
            if !rotate
                && session_id.is_some()
                && scheduling.mode != SchedulingMode::PerformanceFirst
            {
                let sid = session_id.unwrap();

                // 1. Check whether the session is already bound to an account
                if let Some(bound_id) = self.session_accounts.get(sid).map(|v| v.clone()) {
                    // [Fix] First find the matching account by account_id, get its email
                    // 2. Convert email -> account_id to check whether the bound account is rate limited
                    if let Some(bound_token) =
                        tokens_snapshot.iter().find(|t| t.account_id == bound_id)
                    {
                        let key = self
                            .email_to_account_id(&bound_token.email)
                            .unwrap_or_else(|| bound_token.account_id.clone());
                        // [FIX] Pass None for specific model wait time if not applicable
                        let reset_sec = self.rate_limit_tracker.get_remaining_wait(&key, None);
                        if reset_sec > 0 {
                            // [Fix Issue #284] Immediately unbind and switch accounts instead of blocking to wait
                            // Reason: blocking to wait causes client socket timeouts (UND_ERR_SOCKET) under concurrent requests
                            tracing::debug!(
                                "Sticky Session: Bound account {} is rate-limited ({}s), unbinding and switching.",
                                bound_token.email, reset_sec
                            );
                            self.session_accounts.remove(sid);
                        } else if !attempted.contains(&bound_id)
                            && !(quota_protection_enabled
                                && bound_token.protected_models.contains(&normalized_target))
                        {
                            // 3. The account is usable and not marked as a failed attempt; reuse it preferentially
                            tracing::debug!("Sticky Session: Successfully reusing bound account {} for session {}", bound_token.email, sid);
                            target_token = Some(bound_token.clone());
                        } else if quota_protection_enabled
                            && bound_token.protected_models.contains(&normalized_target)
                        {
                            tracing::debug!("Sticky Session: Bound account {} is quota-protected for model {} [{}], unbinding and switching.", bound_token.email, normalized_target, target_model);
                            self.session_accounts.remove(sid);
                        }
                    } else {
                        // The bound account no longer exists (may have been deleted), unbind
                        tracing::debug!(
                            "Sticky Session: Bound account not found for session {}, unbinding",
                            sid
                        );
                        self.session_accounts.remove(sid);
                    }
                }
            }

            // Mode B: atomic 60s global lock (default protection for the no-session_id case)
            // [Fix] Performance-first mode should skip the 60s lock;
            if target_token.is_none()
                && !rotate
                && quota_group != "image_gen"
                && scheduling.mode != SchedulingMode::PerformanceFirst
            {
                // [Optimization] Use the pre-fetched snapshot, no longer locking inside the loop
                if let Some((account_id, last_time)) = &last_used_account_id {
                    // [FIX #3] The 60s lock logic should check the `attempted` set, to avoid retrying accounts that already failed
                    if last_time.elapsed().as_secs() < 60 && !attempted.contains(account_id) {
                        if let Some(found) =
                            tokens_snapshot.iter().find(|t| &t.account_id == account_id)
                        {
                            // [Fix] Check rate limit status and quota protection, to avoid reusing an already-locked account
                            if !self
                                .is_rate_limited(&found.account_id, Some(&normalized_target))
                                .await
                                && !(quota_protection_enabled
                                    && found.protected_models.contains(&normalized_target))
                            {
                                tracing::debug!(
                                    "60s Window: Force reusing last account: {}",
                                    found.email
                                );
                                target_token = Some(found.clone());
                            } else {
                                if self
                                    .is_rate_limited(&found.account_id, Some(&normalized_target))
                                    .await
                                {
                                    tracing::debug!(
                                        "60s Window: Last account {} is rate-limited, skipping",
                                        found.email
                                    );
                                } else {
                                    tracing::debug!("60s Window: Last account {} is quota-protected for model {} [{}], skipping", found.email, normalized_target, target_model);
                                }
                            }
                        }
                    }
                }

                // If there's no lock, use P2C to select an account (avoids hotspot issues)
                if target_token.is_none() {
                    // First filter to accounts that are not rate limited
                    let mut non_limited: Vec<ProxyToken> = Vec::new();
                    for t in &tokens_snapshot {
                        if !self
                            .is_rate_limited(&t.account_id, Some(&normalized_target))
                            .await
                        {
                            non_limited.push(t.clone());
                        }
                    }

                    if let Some(selected) = self.select_with_p2c(
                        &non_limited,
                        &attempted,
                        &normalized_target,
                        quota_protection_enabled,
                    ) {
                        target_token = Some(selected.clone());
                        need_update_last_used =
                            Some((selected.account_id.clone(), std::time::Instant::now()));

                        // If this is the session's first assignment and stickiness is needed, establish the binding here
                        if let Some(sid) = session_id {
                            if scheduling.mode != SchedulingMode::PerformanceFirst {
                                self.session_accounts
                                    .insert(sid.to_string(), selected.account_id.clone());
                                tracing::debug!(
                                    "Sticky Session: Bound new account {} to session {}",
                                    selected.email,
                                    sid
                                );
                            }
                        }
                    }
                }
            } else if target_token.is_none() {
                // Mode C: P2C selection (replaces pure round-robin)
                tracing::debug!("🔄 [Mode C] P2C selection from {} candidates", total);

                // First filter to accounts that are not rate limited
                let mut non_limited: Vec<ProxyToken> = Vec::new();
                for t in &tokens_snapshot {
                    if !self
                        .is_rate_limited(&t.account_id, Some(&normalized_target))
                        .await
                    {
                        non_limited.push(t.clone());
                    }
                }

                if let Some(selected) = self.select_with_p2c(
                    &non_limited,
                    &attempted,
                    &normalized_target,
                    quota_protection_enabled,
                ) {
                    tracing::debug!("  {} - SELECTED via P2C", selected.email);
                    target_token = Some(selected.clone());

                    if rotate {
                        tracing::debug!("Force Rotation: Switched to account: {}", selected.email);
                    }
                }
            }

            let mut token = match target_token {
                Some(t) => t,
                None => {
                    // Optimistic reset strategy: two-layer protection mechanism
                    // Compute the shortest wait time
                    let min_wait = tokens_snapshot
                        .iter()
                        .filter_map(|t| {
                            let wait = self
                                .rate_limit_tracker
                                .get_remaining_wait(&t.account_id, Some(&normalized_target));
                            if wait > 0 {
                                Some(wait)
                            } else {
                                None
                            }
                        })
                        .min();

                    // Layer 1: if the shortest wait time is <= 2 seconds, apply a buffer delay
                    if let Some(wait_sec) = min_wait {
                        if wait_sec <= 2 {
                            let wait_ms = (wait_sec as f64 * 1000.0) as u64;
                            tracing::warn!(
                                "All accounts rate-limited but shortest wait is {}s. Applying {}ms buffer for state sync...",
                                wait_sec, wait_ms
                            );

                            // Buffer delay
                            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;

                            // Retry selecting an account
                            let mut retry_token = None;
                            for token in &tokens_snapshot {
                                if attempted.contains(&token.account_id)
                                    || self
                                        .is_rate_limited(
                                            &token.account_id,
                                            Some(&normalized_target),
                                        )
                                        .await
                                    || (quota_protection_enabled
                                        && token.protected_models.contains(&normalized_target))
                                {
                                    continue;
                                }
                                retry_token = Some(token);
                                break;
                            }

                            if let Some(t) = retry_token {
                                tracing::info!(
                                    "✅ Buffer delay successful! Found available account: {}",
                                    t.email
                                );
                                t.clone()
                            } else {
                                // Layer 2: still no available account after the buffer, execute an optimistic reset
                                tracing::warn!(
                                    "Buffer delay failed. Executing optimistic reset for all {} accounts...",
                                    tokens_snapshot.len()
                                );

                                // Clear all rate limit records
                                self.rate_limit_tracker.clear_for_optimistic_reset();

                                // Try selecting an account again
                                let final_token = tokens_snapshot.iter().find(|t| {
                                    !attempted.contains(&t.account_id)
                                        && !(quota_protection_enabled
                                            && t.protected_models.contains(&normalized_target))
                                });

                                if let Some(t) = final_token {
                                    tracing::info!(
                                        "✅ Optimistic reset successful! Using account: {}",
                                        t.email
                                    );
                                    t.clone()
                                } else {
                                    return Err(
                                        "All accounts failed after optimistic reset.".to_string()
                                    );
                                }
                            }
                        } else {
                            return Err(format!("All accounts limited. Wait {}s.", wait_sec));
                        }
                    } else {
                        return Err("All accounts failed or unhealthy.".to_string());
                    }
                }
            };

            // Safety net: avoid selecting an account that has been disabled on disk but still
            // exists in the in-memory snapshot (e.g. stale cache + sticky session binding).
            match Self::get_account_state_on_disk(&token.account_path).await {
                OnDiskAccountState::Disabled => {
                    tracing::warn!(
                        "Selected account {} is disabled on disk, purging and retrying",
                        token.email
                    );
                    attempted.insert(token.account_id.clone());
                    self.remove_account(&token.account_id);
                    continue;
                }
                OnDiskAccountState::Unknown => {
                    tracing::warn!(
                        "Selected account {} state on disk is unavailable, skipping",
                        token.email
                    );
                    attempted.insert(token.account_id.clone());
                    continue;
                }
                OnDiskAccountState::Enabled => {}
            }

            // 3. [ENHANCED] Check whether the token has expired (300-second/5-minute smooth refresh ahead of expiry, to ensure high availability and support concurrent retries)
            let now = chrono::Utc::now().timestamp();
            const TOKEN_REFRESH_BUFFER_SECS: i64 = 300;
            if now >= token.timestamp - TOKEN_REFRESH_BUFFER_SECS {
                // [NEW] Double-checked locking logic (Double-Checked Locking)
                let refresh_mu = self
                    .refresh_locks
                    .entry(token.account_id.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                    .clone();

                let _guard = refresh_mu.lock().await;

                // Re-check the latest state
                let latest_token_opt = self.tokens.get(&token.account_id).map(|r| r.clone());
                if let Some(latest) = latest_token_opt {
                    if now < latest.timestamp - TOKEN_REFRESH_BUFFER_SECS {
                        token = latest.clone();
                        tracing::debug!("Account {} was already refreshed by a concurrent thread in the loop, skipping", token.email);
                    } else {
                        tracing::debug!(
                            "Account {}'s token is about to expire, performing a main-path refresh...",
                            token.email
                        );
                        // Call OAuth to refresh the token
                        match crate::modules::oauth::refresh_access_token(
                            &token.refresh_token,
                            Some(&token.account_id),
                        )
                        .await
                        {
                            Ok(token_response) => {
                                tracing::debug!("Token refresh succeeded!");
                                // After a successful refresh, reset this account's invalid_grant failure count
                                self.invalid_grant_failures.remove(&token.account_id);

                                token.access_token = token_response.access_token.clone();
                                token.expires_in = token_response.expires_in;
                                token.timestamp = now + token_response.expires_in;

                                if let Some(mut entry) = self.tokens.get_mut(&token.account_id) {
                                    entry.access_token = token.access_token.clone();
                                    entry.expires_in = token.expires_in;
                                    entry.timestamp = token.timestamp;
                                }
                                // [FIX] Backgrounded the disk write: memory has been updated, disk persistence is spawned onto the blocking thread pool
                                // Avoids timing out get_token's 5s window due to disk I/O or lock contention
                                {
                                    let write_path = token.account_path.clone();
                                    let access_token = token_response.access_token.clone();
                                    let expires_in = token_response.expires_in;
                                    let id_token = token_response.id_token.clone();
                                    let new_rt = token_response.refresh_token.clone();
                                    let write_ts = now + token_response.expires_in;
                                    tokio::task::spawn_blocking(move || {
                                        let Ok(_lk) = crate::modules::account::lock_account_file_updates() else { return; };
                                        let Ok(raw) = std::fs::read_to_string(&write_path) else { return; };
                                        let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&raw) else { return; };
                                        val["token"]["access_token"] = access_token.into();
                                        val["token"]["expires_in"] = expires_in.into();
                                        val["token"]["expiry_timestamp"] = write_ts.into();
                                        if let Some(it) = id_token { val["token"]["id_token"] = it.into(); }
                                        if let Some(rt) = new_rt { val["token"]["refresh_token"] = rt.into(); }
                                        if let Ok(s) = serde_json::to_string_pretty(&val) { let _ = std::fs::write(&write_path, s); }
                                    });
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Token refresh failed ({}): {}, trying the next account",
                                    token.email,
                                    e
                                );
                                let is_grant_error =
                                    e.contains("\"invalid_grant\"") || e.contains("invalid_grant");
                                if is_grant_error {
                                    let mut fail_count = self
                                        .invalid_grant_failures
                                        .entry(token.account_id.clone())
                                        .or_insert(0);
                                    *fail_count += 1;
                                    let current_fails = *fail_count;
                                    if current_fails >= 2 {
                                        tracing::error!(
                                            "Account {} confirmed invalid_grant {} times in a row, formally deactivating",
                                            token.email,
                                            current_fails
                                        );
                                        let _ = self
                                            .disable_account(
                                                &token.account_id,
                                                &format!("invalid_grant: {}", e),
                                            )
                                            .await;
                                        self.invalid_grant_failures.remove(&token.account_id);
                                    } else {
                                        tracing::warn!(
                                            "Account {} confirmed invalid_grant for the first time (count {}/2), not deactivating yet, skipping this scheduling round",
                                            token.email,
                                            current_fails
                                        );
                                    }
                                }
                                last_error = Some(format!("Token refresh failed: {}", e));
                                attempted.insert(token.account_id.clone());
                                if quota_group != "image_gen"
                                    && matches!(&last_used_account_id, Some((id, _)) if id == &token.account_id)
                                {
                                    need_update_last_used =
                                        Some((String::new(), std::time::Instant::now()));
                                }
                                continue;
                            }
                        }
                    }
                }
            }

            // 4. [ENHANCED] Ensure project_id is present (guard the fetch action with a lock)
            let project_id = if let Some(pid) = &token.project_id {
                if pid.is_empty() {
                    None
                } else {
                    Some(pid.clone())
                }
            } else {
                None
            };
            let project_id = if let Some(pid) = project_id {
                pid
            } else {
                // [NEW] Implement async coalescing for fetch_project_id based on SingleFlight
                // 1. Check whether there is already an inflight request
                let (_rx, is_new) = {
                    if let Some(existing_rx) = self.load_code_assist_inflight.get(&token.account_id)
                    {
                        (existing_rx.value().clone(), false)
                    } else {
                        // Create a new inflight channel
                        let (_tx, rx) = tokio::sync::watch::channel(None);
                        self.load_code_assist_inflight
                            .insert(token.account_id.clone(), rx.clone());
                        (rx, true)
                    }
                };

                if is_new {
                    // Only the "first discoverer" performs the real request
                    tracing::debug!("Account {} starting [SingleFlight] ProjectID probe...", token.email);

                    let _result =
                        match crate::proxy::project_resolver::fetch_project_id(&token.access_token)
                            .await
                        {
                            Ok(pid) => {
                                if let Some(mut entry) = self.tokens.get_mut(&token.account_id) {
                                    entry.project_id = Some(pid.clone());
                                }
                                let _ = self.save_project_id(&token.account_id, &pid).await;
                                Ok(pid)
                            }
                            Err(e) => Err(e),
                        };

                    // Broadcast the result and clean up inflight
                    if let Some(_entry) = self.load_code_assist_inflight.get_mut(&token.account_id)
                    {
                        // This is an rx, but can watch be operated on without a tx in Rust via some private path?
                        // Correction: we need to hold onto the tx. Redesign this: use a Mutex, or hold the tx outside the scope.
                        // Since DashMap can't store a non-Clone tx, we switch to a Mutex-guarded flow, or just execute directly inside the if is_new branch
                    }

                    // [Corrected implementation plan]: for this kind of high-frequency project_id probing, using the refresh_mu lock is still the most efficient,
                    // but we need to add "forced async wait" logic. Since the previous Mutex is already async,
                    // we just need to make sure the fetch_project_id call is wrapped inside the lock with a double-check.
                    // The previous code already does this.

                    // To fully align with agent-vibes' singleFlight semantics (i.e. not just locking, but also "result reuse"),
                    // I'll keep the previous logic but remove the unnecessary duplicate logging.

                    let refresh_mu = self
                        .refresh_locks
                        .entry(token.account_id.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                        .clone();
                    let _guard = refresh_mu.lock().await;

                    let project_state = self
                        .tokens
                        .get(&token.account_id)
                        .map(|entry| (entry.project_id.clone(), entry.access_token.clone()));
                    match project_state {
                        Some((Some(pid), _)) if !pid.is_empty() => pid,
                        Some((Some(_), access_token)) => {
                            match crate::proxy::project_resolver::fetch_project_id(&access_token)
                                .await
                            {
                                Ok(pid) => {
                                    if let Some(mut entry) = self.tokens.get_mut(&token.account_id)
                                    {
                                        entry.project_id = Some(pid.clone());
                                    }
                                    // [FIX] Backgrounded the disk write: project_id has already been written to memory, disk persistence doesn't block the hot path
                                    {
                                        let write_path = self.tokens.get(&token.account_id)
                                            .map(|e| e.account_path.clone())
                                            .unwrap_or_else(|| self.data_dir.join("accounts").join(format!("{}.json", token.account_id)));
                                        let pid_clone = pid.clone();
                                        tokio::task::spawn_blocking(move || {
                                            let Ok(_lk) = crate::modules::account::lock_account_file_updates() else { return; };
                                            let Ok(raw) = std::fs::read_to_string(&write_path) else { return; };
                                            let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&raw) else { return; };
                                            val["token"]["project_id"] = pid_clone.into();
                                            if let Ok(s) = serde_json::to_string_pretty(&val) { let _ = std::fs::write(&write_path, s); }
                                        });
                                    }
                                    pid
                                }
                                Err(_) => "bamboo-precept-lgxtn".to_string(),
                            }
                        }
                        _ => "bamboo-precept-lgxtn".to_string(),
                    }
                } else {
                    // If this isn't the first one, wait for the result (in Mutex mode rx isn't strictly needed, but we keep the lock for rigor)
                    let refresh_mu = self
                        .refresh_locks
                        .get(&token.account_id)
                        .map(|v| v.value().clone());
                    if let Some(mu) = refresh_mu {
                        let _guard = mu.lock().await;
                    }

                    self.tokens
                        .get(&token.account_id)
                        .and_then(|t| t.project_id.clone())
                        .unwrap_or_else(|| "bamboo-precept-lgxtn".to_string())
                }
            };

            // [Optimization] Before returning successfully, uniformly update last_used_account (if needed)
            if let Some((new_account_id, new_time)) = need_update_last_used {
                if quota_group != "image_gen" {
                    let mut last_used = self.last_used_account.lock().await;
                    if new_account_id.is_empty() {
                        // An empty string means the lock needs to be cleared
                        *last_used = None;
                    } else {
                        *last_used = Some((new_account_id, new_time));
                    }
                }
            }

            return Ok((
                token.access_token,
                project_id,
                token.email,
                token.account_id,
                0,
            ));
        }

        Err(last_error.unwrap_or_else(|| "All accounts failed".to_string()))
    }

    async fn disable_account(&self, account_id: &str, reason: &str) -> Result<(), String> {
        let path = if let Some(entry) = self.tokens.get(account_id) {
            entry.account_path.clone()
        } else {
            self.data_dir
                .join("accounts")
                .join(format!("{}.json", account_id))
        };

        let now = chrono::Utc::now().timestamp();
        let reason_owned = reason.to_string();
        update_account_json(&path, move |content| {
            content["disabled"] = serde_json::Value::Bool(true);
            content["disabled_at"] = serde_json::Value::Number(now.into());
            content["disabled_reason"] = serde_json::Value::String(truncate_reason(&reason_owned, 800));
        })
        .await?;

        // [Fix Issue #3] Remove the disabled account from memory, to prevent it from continuing to be used by the 60s lock logic
        self.remove_account(account_id);

        tracing::warn!("Account disabled: {} ({:?})", account_id, path);
        Ok(())
    }

    /// Save project_id to the account file
    async fn save_project_id(&self, account_id: &str, project_id: &str) -> Result<(), String> {
        let path = self
            .tokens
            .get(account_id)
            .ok_or("Account does not exist")?
            .account_path
            .clone();
        let project_id_owned = project_id.to_string();
        update_account_json(&path, move |content| {
            content["token"]["project_id"] = serde_json::Value::String(project_id_owned);
        })
        .await?;

        tracing::debug!("Saved project_id to account {}", account_id);
        Ok(())
    }

    /// Save the refreshed token to the account file
    async fn save_refreshed_token(
        &self,
        account_id: &str,
        token_response: &crate::modules::oauth::TokenResponse,
    ) -> Result<(), String> {
        let path = self
            .tokens
            .get(account_id)
            .ok_or("Account does not exist")?
            .account_path
            .clone();
        let now = chrono::Utc::now().timestamp();
        let access_token = token_response.access_token.clone();
        let expires_in = token_response.expires_in;
        let id_token = token_response.id_token.clone();
        let refresh_token = token_response.refresh_token.clone();
        let expiry_timestamp = now + expires_in;
        update_account_json(&path, move |content| {
            content["token"]["access_token"] = serde_json::Value::String(access_token);
            content["token"]["expires_in"] = serde_json::Value::Number(expires_in.into());
            content["token"]["expiry_timestamp"] = serde_json::Value::Number(expiry_timestamp.into());

            // If a new id_token was obtained, save it
            if let Some(it) = id_token {
                content["token"]["id_token"] = serde_json::Value::String(it);
            }

            // If a new refresh_token was obtained (token rotation), save it as well
            if let Some(rt) = refresh_token {
                content["token"]["refresh_token"] = serde_json::Value::String(rt);
            }
        })
        .await?;

        tracing::debug!("Saved the refreshed token to account {}", account_id);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Get the Token for a given account by email (used for scenarios like warmup that require a specific account)
    /// This method automatically refreshes an expired token
    pub async fn get_token_by_email(
        &self,
        email: &str,
    ) -> Result<(String, String, String, String, u64), String> {
        // Look up the account info
        let token_info = {
            let mut found = None;
            for entry in self.tokens.iter() {
                let token = entry.value();
                if token.email == email {
                    found = Some((
                        token.account_id.clone(),
                        token.access_token.clone(),
                        token.refresh_token.clone(),
                        token.timestamp,
                        token.expires_in,
                        chrono::Utc::now().timestamp(),
                        token.project_id.clone(),
                    ));
                    break;
                }
            }
            found
        };

        let (
            account_id,
            current_access_token,
            refresh_token,
            timestamp,
            expires_in,
            now,
            project_id_opt,
        ) = match token_info {
            Some(info) => info,
            None => return Err(format!("Account not found: {}", email)),
        };

        let project_id = project_id_opt
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "bamboo-precept-lgxtn".to_string());

        // Check whether it has expired (5-minute lead time)
        if now < timestamp + expires_in - 300 {
            return Ok((
                current_access_token,
                project_id,
                email.to_string(),
                account_id,
                0,
            ));
        }

        tracing::info!("[Warmup] Token for {} is expiring, refreshing...", email);

        // Call OAuth to refresh the token
        match crate::modules::oauth::refresh_access_token(&refresh_token, Some(&account_id)).await {
            Ok(token_response) => {
                tracing::info!("[Warmup] Token refresh successful for {}", email);
                let new_now = chrono::Utc::now().timestamp();

                // Update the cache
                if let Some(mut entry) = self.tokens.get_mut(&account_id) {
                    entry.access_token = token_response.access_token.clone();
                    entry.expires_in = token_response.expires_in;
                    entry.timestamp = new_now;
                }

                // Save to disk
                let _ = self
                    .save_refreshed_token(&account_id, &token_response)
                    .await;

                Ok((
                    token_response.access_token,
                    project_id,
                    email.to_string(),
                    account_id,
                    0,
                ))
            }
            Err(e) => Err(format!(
                "[Warmup] Token refresh failed for {}: {}",
                email, e
            )),
        }
    }

    // ===== Rate limit management methods =====

    /// Mark an account as rate limited (called externally, usually from a handler)
    /// The parameter is an email; it's automatically converted to account_id internally
    pub async fn mark_rate_limited(
        &self,
        email: &str,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
    ) {
        // [NEW] Check whether the circuit breaker is enabled (uses an in-memory cache, very fast)
        let config = self.circuit_breaker_config.read().await.clone();
        if !config.enabled {
            return;
        }

        // [Alternative] Convert email -> account_id
        let key = self
            .email_to_account_id(email)
            .unwrap_or_else(|| email.to_string());

        self.rate_limit_tracker.parse_from_error(
            &key,
            status,
            retry_after_header,
            error_body,
            None,
            &config.backoff_steps, // [NEW] the config passed in
        );
    }

    /// Check whether an account is rate limited (supports model level)
    pub async fn is_rate_limited(&self, account_id: &str, model: Option<&str>) -> bool {
        // [NEW] Check whether the circuit breaker is enabled
        let config = self.circuit_breaker_config.read().await;
        if !config.enabled {
            return false;
        }
        self.rate_limit_tracker.is_rate_limited(account_id, model)
    }

    /// Get how many seconds remain until the rate limit resets
    #[allow(dead_code)]
    pub fn get_rate_limit_reset_seconds(&self, account_id: &str) -> Option<u64> {
        self.rate_limit_tracker.get_reset_seconds(account_id)
    }

    /// Clear expired rate limit records
    #[allow(dead_code)]
    pub fn clean_expired_rate_limits(&self) {
        self.rate_limit_tracker.cleanup_expired();
    }

    /// [Alternative] Look up the corresponding account_id by email
    /// Used to convert the email passed in by handlers into the account_id used by the tracker
    fn email_to_account_id(&self, email: &str) -> Option<String> {
        self.tokens
            .iter()
            .find(|entry| entry.value().email == email)
            .map(|entry| entry.value().account_id.clone())
    }

    /// Clear the rate limit record for a given account
    pub fn clear_rate_limit(&self, account_id: &str) -> bool {
        let cleared = self.rate_limit_tracker.clear(account_id);
        let persisted_cleared = self.clear_all_persisted_live_limits(account_id);
        cleared || persisted_cleared
    }

    pub fn clear_rate_limit_memory(&self, account_id: &str) -> bool {
        self.rate_limit_tracker.clear(account_id)
    }

    /// Clear all rate limit records
    pub fn clear_all_rate_limits(&self) {
        self.rate_limit_tracker.clear_all();
        let accounts_dir = self.data_dir.join("accounts");
        if let Ok(entries) = std::fs::read_dir(accounts_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
                    if let Some(account_id) =
                        entry.path().file_stem().and_then(|value| value.to_str())
                    {
                        self.clear_all_persisted_live_limits(account_id);
                    }
                }
            }
        }
    }

    fn clear_all_persisted_live_limits(&self, account_id: &str) -> bool {
        let path = self
            .data_dir
            .join("accounts")
            .join(format!("{}.json", account_id));
        let Ok(_account_write) = crate::modules::account::lock_account_file_updates() else {
            return false;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(mut content) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return false;
        };
        let Some(live_limits) = content
            .get_mut("live_limited_models")
            .and_then(|value| value.as_object_mut())
        else {
            return false;
        };
        if live_limits.is_empty() {
            return false;
        }
        live_limits.clear();
        let Ok(serialized) = serde_json::to_string_pretty(&content) else {
            return false;
        };
        std::fs::write(path, serialized).is_ok()
    }

    /// Mark the account's request as successful, resetting the consecutive failure count
    ///
    /// Called after a request completes successfully, zeroing this account's failure count,
    /// so the next failure starts from the shortest lockout time (smart rate limiting).
    pub fn mark_account_success(&self, account_id: &str) {
        self.rate_limit_tracker.mark_success(account_id);
    }

    /// Check whether there is an available Google account
    ///
    /// Used for the smart decision in "fallback-only" mode: only use an external provider when all Google accounts are unavailable.
    ///
    /// # Parameters
    /// - `quota_group`: the quota group ("claude" or "gemini"), currently unused but kept for future extension
    /// - `target_model`: the target model name (already normalized), used for the quota protection check
    ///
    /// # Return value
    /// - `true`: at least one account is available (not rate limited and not quota-protected)
    /// - `false`: all accounts are unavailable (rate limited or quota-protected)
    ///
    /// # Example
    /// ```ignore
    /// // Check whether there's an available account to handle a claude-sonnet request
    /// let has_available = token_manager.has_available_account("claude", "claude-sonnet-4-20250514").await;
    /// if !has_available {
    ///     // Switch to an external provider
    /// }
    /// ```
    pub async fn has_available_account(&self, _quota_group: &str, target_model: &str) -> bool {
        // Check whether quota protection is enabled
        let quota_protection_enabled = crate::modules::config::load_app_config()
            .map(|cfg| cfg.quota_protection.enabled)
            .unwrap_or(false);

        // Iterate over all accounts, checking whether any are available
        for entry in self.tokens.iter() {
            let token = entry.value();

            // 1. Check whether it's rate limited
            if self.is_rate_limited(&token.account_id, None).await {
                tracing::debug!(
                    "[Fallback Check] Account {} is rate-limited, skipping",
                    token.email
                );
                continue;
            }

            // 2. Check whether it's quota-protected (if enabled)
            if quota_protection_enabled && token.protected_models.contains(target_model) {
                tracing::debug!(
                    "[Fallback Check] Account {} is quota-protected for model {}, skipping",
                    token.email,
                    target_model
                );
                continue;
            }

            // Found at least one available account
            tracing::debug!(
                "[Fallback Check] Found available account: {} for model {}",
                token.email,
                target_model
            );
            return true;
        }

        // All accounts are unavailable
        tracing::info!(
            "[Fallback Check] No available Google accounts for model {}, fallback should be triggered",
            target_model
        );
        false
    }

    /// Get the quota refresh time from the account file
    ///
    /// Returns this account's most recent quota refresh time string (ISO 8601 format)
    ///
    /// # Parameters
    /// - `account_id`: the account ID (used to locate the account file)
    pub fn get_quota_reset_time(&self, account_id: &str) -> Option<String> {
        // Look up the account file directly by account_id (the filename is {account_id}.json)
        let account_path = self
            .data_dir
            .join("accounts")
            .join(format!("{}.json", account_id));

        let content = std::fs::read_to_string(&account_path).ok()?;
        let account: serde_json::Value = serde_json::from_str(&content).ok()?;

        // Get the earliest reset_time among quota.models (the most conservative lockout strategy)
        account
            .get("quota")
            .and_then(|q| q.get("models"))
            .and_then(|m| m.as_array())
            .and_then(|models| {
                models
                    .iter()
                    .filter_map(|m| m.get("reset_time").and_then(|r| r.as_str()))
                    .filter(|s| !s.is_empty())
                    .min()
                    .map(|s| s.to_string())
            })
    }

    /// Precisely lock the account using its quota refresh time
    ///
    /// When the API returns 429 without a quotaResetDelay, try to use the account's quota refresh time
    ///
    /// # Parameters
    /// - `account_id`: the account ID
    /// - `reason`: the rate limit reason (QuotaExhausted/ServerError etc.)
    /// - `model`: an optional model name, used for model-level rate limiting
    pub fn set_precise_lockout(
        &self,
        account_id: &str,
        reason: crate::proxy::rate_limit::RateLimitReason,
        model: Option<String>,
    ) -> bool {
        // [FIX #2209] Uniformly normalize the model name
        let normalized_model = model
            .as_deref()
            .and_then(|m| crate::proxy::common::model_mapping::normalize_to_standard_id(m));
        let model_to_lock = normalized_model.or(model);

        if let Some(reset_time_str) = self.get_quota_reset_time(account_id) {
            tracing::info!("Found account {}'s quota refresh time: {}", account_id, reset_time_str);
            self.rate_limit_tracker.set_lockout_until_iso(
                account_id,
                &reset_time_str,
                reason,
                model_to_lock,
            )
        } else {
            tracing::debug!(
                "Could not find account {}'s quota refresh time, using the default backoff strategy",
                account_id
            );
            false
        }
    }

    /// Refresh quota in real time and precisely lock the account
    ///
    /// Call this method when a 429 occurs:
    /// 1. Call the quota refresh API in real time to get the latest reset_time
    /// 2. Use the latest reset_time to precisely lock the account
    /// 3. If the fetch fails, return false so the caller falls back to its own strategy
    ///
    /// # Parameters
    /// - `model`: an optional model name, used for model-level rate limiting
    pub async fn fetch_and_lock_with_realtime_quota(
        &self,
        email: &str,
        reason: crate::proxy::rate_limit::RateLimitReason,
        model: Option<String>,
    ) -> bool {
        // 1. Get this account's access_token and account_id from tokens
        // Also get account_id, to ensure the lock key matches the check key
        let (access_token, account_id) = {
            let mut found: Option<(String, String)> = None;
            for entry in self.tokens.iter() {
                if entry.value().email == email {
                    found = Some((
                        entry.value().access_token.clone(),
                        entry.value().account_id.clone(),
                    ));
                    break;
                }
            }
            found
        }
        .unzip();

        let (access_token, account_id) = match (access_token, account_id) {
            (Some(token), Some(id)) => (token, id),
            _ => {
                tracing::warn!("Could not find account {}'s access_token, cannot refresh quota in real time", email);
                return false;
            }
        };

        // 2. Call the quota refresh API
        tracing::info!("Account {} is refreshing quota in real time...", email);
        match crate::modules::quota::fetch_quota(&access_token, email, Some(&account_id)).await {
            Ok((quota_data, _project_id)) => {
                // 3. Extract reset_time from the latest quota
                let earliest_reset = quota_data
                    .models
                    .iter()
                    .filter_map(|m| {
                        if !m.reset_time.is_empty() {
                            Some(m.reset_time.as_str())
                        } else {
                            None
                        }
                    })
                    .min();

                if let Some(reset_time_str) = earliest_reset {
                    tracing::info!(
                        "Account {} real-time quota refresh succeeded, reset_time: {}",
                        email,
                        reset_time_str
                    );

                    // [FIX #2209] Uniformly normalize the model name
                    let normalized_model = model.as_deref().and_then(|m| {
                        crate::proxy::common::model_mapping::normalize_to_standard_id(m)
                    });
                    let model_to_lock = normalized_model.or(model);

                    // [FIX] Use account_id as the key, to stay consistent with the is_rate_limited check
                    self.rate_limit_tracker.set_lockout_until_iso(
                        &account_id,
                        reset_time_str,
                        reason,
                        model_to_lock,
                    )
                } else {
                    tracing::warn!("Account {} quota refresh succeeded but no reset_time was found", email);
                    false
                }
            }
            Err(e) => {
                tracing::warn!("Account {} real-time quota refresh failed: {:?}", email, e);
                false
            }
        }
    }

    fn has_explicit_retry_time(
        parser_mode: TrackerParserMode,
        retry_after_header: Option<&str>,
        error_body: &str,
    ) -> bool {
        match parser_mode {
            TrackerParserMode::Current => {
                crate::proxy::upstream::retry::parse_retry_delay(error_body, retry_after_header)
                    .is_some()
            }
            TrackerParserMode::Baseline => {
                retry_after_header.is_some() || error_body.contains("quotaResetDelay")
            }
        }
    }

    fn parse_rate_limit_with_mode(
        &self,
        account_id: &str,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
        model: Option<&str>,
        backoff_steps: &[u64],
        parser_mode: TrackerParserMode,
    ) -> Option<crate::proxy::rate_limit::RateLimitInfo> {
        match parser_mode {
            TrackerParserMode::Current => self.rate_limit_tracker.parse_from_error(
                account_id,
                status,
                retry_after_header,
                error_body,
                model.map(str::to_string),
                backoff_steps,
            ),
            TrackerParserMode::Baseline => self.rate_limit_tracker.parse_from_error_baseline(
                account_id,
                status,
                retry_after_header,
                error_body,
                model.map(str::to_string),
                backoff_steps,
            ),
        }
    }

    fn record_rate_limit_atomic(
        &self,
        account_id: &str,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
        model: Option<&str>,
        backoff_steps: &[u64],
        parser_mode: TrackerParserMode,
    ) -> Option<crate::proxy::rate_limit::RateLimitInfo> {
        if model
            .and_then(crate::proxy::rate_limit::normalize_image_model_id)
            .is_some()
        {
            match crate::modules::account::lock_account_file_updates() {
                Ok(_account_write) => {
                    let info = self.parse_rate_limit_with_mode(
                        account_id,
                        status,
                        retry_after_header,
                        error_body,
                        model,
                        backoff_steps,
                        parser_mode,
                    );
                    if parser_mode == TrackerParserMode::Current {
                        if let Some(ref info) = info {
                            self.persist_live_limit_locked(
                                account_id,
                                model,
                                status,
                                retry_after_header,
                                error_body,
                                info,
                            );
                        }
                    }
                    return info;
                }
                Err(error) => {
                    tracing::debug!(
                        "Failed to serialize live limit update for {}: {}",
                        account_id,
                        error
                    );
                }
            }
        }

        self.parse_rate_limit_with_mode(
            account_id,
            status,
            retry_after_header,
            error_body,
            model,
            backoff_steps,
            parser_mode,
        )
    }

    /// Register the in-memory image exclusion before releasing its account permit.
    /// Returns whether a slower quota refresh is still useful after the permit is released.
    pub async fn mark_rate_limited_fast(
        &self,
        email: &str,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
        model: Option<&str>,
    ) -> bool {
        let normalized_model =
            model.and_then(crate::proxy::common::model_mapping::normalize_to_standard_id);
        let model_to_track = normalized_model.as_deref().or(model);
        let config = self.circuit_breaker_config.read().await.clone();
        if !config.enabled {
            return false;
        }

        let account_id = self
            .email_to_account_id(email)
            .unwrap_or_else(|| email.to_string());
        let has_explicit_retry_time = Self::has_explicit_retry_time(
            TrackerParserMode::Current,
            retry_after_header,
            error_body,
        );
        let reason = classify_rate_limit_reason(error_body);
        let recorded = self.record_rate_limit_atomic(
            &account_id,
            status,
            retry_after_header,
            error_body,
            model_to_track,
            &config.backoff_steps,
            TrackerParserMode::Current,
        );

        status == 429
            && recorded.is_some()
            && !has_explicit_retry_time
            && reason == crate::proxy::rate_limit::RateLimitReason::QuotaExhausted
    }

    pub async fn refresh_quota_lock_after_fast_mark(&self, email: &str, model: Option<&str>) {
        let normalized_model =
            model.and_then(crate::proxy::common::model_mapping::normalize_to_standard_id);
        let model_to_track = normalized_model.as_deref().or(model);
        let account_id = self
            .email_to_account_id(email)
            .unwrap_or_else(|| email.to_string());
        let reason = crate::proxy::rate_limit::RateLimitReason::QuotaExhausted;

        if self
            .fetch_and_lock_with_realtime_quota(email, reason, model_to_track.map(str::to_string))
            .await
        {
            tracing::info!("Account {} has been precisely locked using real-time quota", email);
            return;
        }
        if self.set_precise_lockout(&account_id, reason, model_to_track.map(str::to_string)) {
            tracing::info!("Account {} has been locked using the locally cached quota", account_id);
        }
    }

    pub async fn mark_rate_limited_async(
        &self,
        email: &str,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
        model: Option<&str>,
    ) {
        self.mark_rate_limited_async_with_mode(
            email,
            status,
            retry_after_header,
            error_body,
            model,
            TrackerParserMode::Current,
        )
        .await;
    }

    pub async fn mark_rate_limited_async_baseline(
        &self,
        email: &str,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
        model: Option<&str>,
    ) {
        self.mark_rate_limited_async_with_mode(
            email,
            status,
            retry_after_header,
            error_body,
            model,
            TrackerParserMode::Baseline,
        )
        .await;
    }

    async fn mark_rate_limited_async_with_mode(
        &self,
        email: &str,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
        model: Option<&str>,
        parser_mode: TrackerParserMode,
    ) {
        let normalized_model =
            model.and_then(crate::proxy::common::model_mapping::normalize_to_standard_id);
        let model_to_track = normalized_model.as_deref().or(model);
        let config = self.circuit_breaker_config.read().await.clone();
        if !config.enabled {
            return;
        }

        let account_id = self
            .email_to_account_id(email)
            .unwrap_or_else(|| email.to_string());
        if Self::has_explicit_retry_time(parser_mode, retry_after_header, error_body) {
            self.record_rate_limit_atomic(
                &account_id,
                status,
                retry_after_header,
                error_body,
                model_to_track,
                &config.backoff_steps,
                parser_mode,
            );
            return;
        }

        let reason = classify_rate_limit_reason(error_body);
        if reason != crate::proxy::rate_limit::RateLimitReason::QuotaExhausted {
            self.record_rate_limit_atomic(
                &account_id,
                status,
                retry_after_header,
                error_body,
                model_to_track,
                &config.backoff_steps,
                parser_mode,
            );
            return;
        }

        if self
            .fetch_and_lock_with_realtime_quota(email, reason, model_to_track.map(str::to_string))
            .await
        {
            tracing::info!("Account {} has been precisely locked using real-time quota", email);
            return;
        }
        if self.set_precise_lockout(&account_id, reason, model_to_track.map(str::to_string)) {
            tracing::info!("Account {} has been locked using the locally cached quota", account_id);
            return;
        }

        tracing::warn!("Account {} could not obtain a quota refresh time, using the exponential backoff strategy", account_id);
        self.record_rate_limit_atomic(
            &account_id,
            status,
            retry_after_header,
            error_body,
            model_to_track,
            &config.backoff_steps,
            parser_mode,
        );
    }

    fn persist_live_limit_locked(
        &self,
        account_id: &str,
        model: Option<&str>,
        status: u16,
        retry_after_header: Option<&str>,
        error_body: &str,
        info: &crate::proxy::rate_limit::RateLimitInfo,
    ) {
        let Some(model_key) = model.and_then(crate::proxy::rate_limit::normalize_image_model_id)
        else {
            return;
        };
        let Some(explicit_delay_ms) =
            crate::proxy::upstream::retry::parse_retry_delay(error_body, retry_after_header)
        else {
            return;
        };
        if status != 429
            || info.reason != crate::proxy::rate_limit::RateLimitReason::QuotaExhausted
            || info.retry_after_sec <= 300
            || !crate::proxy::rate_limit::has_explicit_quota_exhausted(error_body)
        {
            return;
        }
        let (Some(until), Some(detected_at)) = (
            unix_timestamp_ceil(info.reset_time),
            unix_timestamp_ceil(info.detected_at),
        ) else {
            return;
        };

        let path = if let Some(entry) = self.tokens.get(account_id) {
            entry.account_path.clone()
        } else {
            self.data_dir
                .join("accounts")
                .join(format!("{}.json", account_id))
        };

        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut content) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };

        if !content
            .get("live_limited_models")
            .and_then(|v| v.as_object())
            .is_some()
        {
            content["live_limited_models"] = serde_json::Value::Object(serde_json::Map::new());
        }

        content["live_limited_models"][&model_key] = serde_json::json!({
            "model": model_key,
            "status": status,
            "reason": format!("{:?}", info.reason),
            "until": until,
            "detected_at": detected_at,
            "message": format!(
                "QUOTA_EXHAUSTED; retry after {}ms; {}",
                explicit_delay_ms,
                truncate_reason(error_body, 400)
            ),
        });

        let Ok(serialized) = serde_json::to_string_pretty(&content) else {
            return;
        };
        if let Err(e) = std::fs::write(&path, serialized) {
            tracing::debug!("Failed to persist live limit for {}: {}", account_id, e);
        }
    }

    pub fn clear_persisted_live_limit(&self, account_id: &str, model: Option<&str>) {
        let Some(raw_model) = model.filter(|m| !m.is_empty()) else {
            return;
        };
        let model_key = crate::proxy::common::model_mapping::normalize_to_standard_id(raw_model)
            .unwrap_or_else(|| raw_model.to_string());

        let path = if let Some(entry) = self.tokens.get(account_id) {
            entry.account_path.clone()
        } else {
            self.data_dir
                .join("accounts")
                .join(format!("{}.json", account_id))
        };

        let Ok(_account_write) = crate::modules::account::lock_account_file_updates() else {
            return;
        };

        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.rate_limit_tracker.clear_model(account_id, &model_key);
                return;
            }
            Err(error) => {
                tracing::debug!("Failed to read live limit for {}: {}", account_id, error);
                return;
            }
        };
        let Ok(mut content) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };

        let Some(live_limits) = content
            .get_mut("live_limited_models")
            .and_then(|value| value.as_object_mut())
        else {
            self.rate_limit_tracker.clear_model(account_id, &model_key);
            return;
        };

        let mut changed = live_limits.remove(&model_key).is_some();
        if model_key != raw_model {
            changed |= live_limits.remove(raw_model).is_some();
        }

        if changed {
            let Ok(serialized) = serde_json::to_string_pretty(&content) else {
                return;
            };
            if let Err(error) = std::fs::write(&path, serialized) {
                tracing::debug!("Failed to clear live limit for {}: {}", account_id, error);
                return;
            }
        }
        self.rate_limit_tracker.clear_model(account_id, &model_key);
    }

    // ===== Scheduling config related methods =====

    /// Get the current scheduling config
    pub async fn get_sticky_config(&self) -> StickySessionConfig {
        self.sticky_config.read().await.clone()
    }

    /// Update the scheduling config
    pub async fn update_sticky_config(&self, new_config: StickySessionConfig) {
        let mut config = self.sticky_config.write().await;
        *config = new_config;
        tracing::debug!("Scheduling configuration updated: {:?}", *config);
    }

    /// [NEW] Update the circuit breaker config
    pub async fn update_circuit_breaker_config(&self, config: crate::models::CircuitBreakerConfig) {
        let mut lock = self.circuit_breaker_config.write().await;
        *lock = config;
        tracing::debug!("Circuit breaker configuration updated");
    }

    /// [NEW] Get the circuit breaker config
    pub async fn get_circuit_breaker_config(&self) -> crate::models::CircuitBreakerConfig {
        self.circuit_breaker_config.read().await.clone()
    }

    /// Clear the sticky mapping for a specific session
    #[allow(dead_code)]
    pub fn clear_session_binding(&self, session_id: &str) {
        self.session_accounts.remove(session_id);
    }

    /// Clear the sticky mapping for all sessions
    pub fn clear_all_sessions(&self) {
        self.session_accounts.clear();
    }

    // ===== [FIX #820] Fixed account mode related methods =====

    /// Set the preferred account ID (fixed account mode)
    /// Pass Some(account_id) to enable fixed account mode, pass None to restore round-robin mode
    pub async fn set_preferred_account(&self, account_id: Option<String>) {
        let mut preferred = self.preferred_account_id.write().await;
        if let Some(ref id) = account_id {
            tracing::info!("🔒 [FIX #820] Fixed account mode enabled: {}", id);
        } else {
            tracing::info!("🔄 [FIX #820] Round-robin mode enabled (no preferred account)");
        }
        *preferred = account_id;
    }

    /// Get the currently preferred account ID
    pub async fn get_preferred_account(&self) -> Option<String> {
        self.preferred_account_id.read().await.clone()
    }

    /// Exchange an Authorization Code for a Refresh Token (Web OAuth)
    pub async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<String, String> {
        crate::modules::oauth::exchange_code(code, redirect_uri)
            .await
            .and_then(|t| {
                t.refresh_token
                    .ok_or_else(|| "No refresh token returned by Google".to_string())
            })
    }

    /// Get the OAuth URL (supports a custom Redirect URI)
    pub fn get_oauth_url_with_redirect(&self, redirect_uri: &str, state: &str) -> String {
        crate::modules::oauth::get_auth_url(redirect_uri, state)
    }

    /// Get user info (email, etc.)
    pub async fn get_user_info(
        &self,
        refresh_token: &str,
    ) -> Result<crate::modules::oauth::UserInfo, String> {
        // First get the Access Token
        let token = crate::modules::oauth::refresh_access_token(refresh_token, None)
            .await
            .map_err(|e| format!("Failed to refresh Access Token: {}", e))?;

        crate::modules::oauth::get_user_info(&token.access_token, None).await
    }

    /// Add a new account (pure backend implementation, no dependency on the Tauri AppHandle)
    pub async fn add_account(&self, email: &str, refresh_token: &str) -> Result<(), String> {
        // 1. Get the Access Token (validates that refresh_token works)
        let token_info = crate::modules::oauth::refresh_access_token(refresh_token, None)
            .await
            .map_err(|e| format!("Invalid refresh token: {}", e))?;

        // 2. Get the Project ID
        let project_id = crate::proxy::project_resolver::fetch_project_id(&token_info.access_token)
            .await
            .unwrap_or_else(|_| "bamboo-precept-lgxtn".to_string()); // Fallback

        // 3. Delegate to modules::account::add_account (handles file writing, index updates, locking)
        let email_clone = email.to_string();
        let refresh_token_clone = refresh_token.to_string();

        tokio::task::spawn_blocking(move || {
            let token_data = crate::models::TokenData::new(
                token_info.access_token,
                refresh_token_clone,
                token_info.expires_in,
                Some(email_clone.clone()),
                Some(project_id),
                None,  // session_id
                false, // Off by default
                token_info.id_token,
            )
            .with_oauth_client_key(token_info.oauth_client_key.clone());

            crate::modules::account::upsert_account(email_clone, None, token_data)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        .map_err(|e| format!("Failed to save account: {}", e))?;

        // 4. Reload (update memory)
        self.reload_all_accounts().await.map(|_| ())
    }

    /// Record a successful request, increasing the health score
    pub fn record_success(&self, account_id: &str) {
        self.health_scores
            .entry(account_id.to_string())
            .and_modify(|s| *s = (*s + 0.05).min(1.0))
            .or_insert(1.0);
        tracing::debug!("📈 Health score increased for account {}", account_id);
    }

    /// Record a failed request, lowering the health score
    pub fn record_failure(&self, account_id: &str) {
        self.health_scores
            .entry(account_id.to_string())
            .and_modify(|s| *s = (*s - 0.2).max(0.0))
            .or_insert(0.8);
        tracing::warn!("📉 Health score decreased for account {}", account_id);
    }

    /// [NEW] Extract the most recent refresh timestamp from the account's quota info
    ///
    /// Claude models (sonnet/opus) share the same refresh time, so we just need the claude family's reset_time
    /// Returns a Unix timestamp (seconds), used for comparison during sorting
    fn extract_earliest_reset_time(&self, account: &serde_json::Value) -> Option<i64> {
        let models = account
            .get("quota")
            .and_then(|q| q.get("models"))
            .and_then(|m| m.as_array())?;

        let mut earliest_ts: Option<i64> = None;

        for model in models {
            // Prefer the claude family's reset_time (shared by sonnet/opus)
            let model_name = model.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if !model_name.contains("claude") {
                continue;
            }

            if let Some(reset_time_str) = model.get("reset_time").and_then(|r| r.as_str()) {
                if reset_time_str.is_empty() {
                    continue;
                }
                // Parse the ISO 8601 time string into a timestamp
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(reset_time_str) {
                    let ts = dt.timestamp();
                    if earliest_ts.is_none() || ts < earliest_ts.unwrap() {
                        earliest_ts = Some(ts);
                    }
                }
            }
        }

        // If there is no claude model time, try to use the most recent time of any model
        if earliest_ts.is_none() {
            for model in models {
                if let Some(reset_time_str) = model.get("reset_time").and_then(|r| r.as_str()) {
                    if reset_time_str.is_empty() {
                        continue;
                    }
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(reset_time_str) {
                        let ts = dt.timestamp();
                        if earliest_ts.is_none() || ts < earliest_ts.unwrap() {
                            earliest_ts = Some(ts);
                        }
                    }
                }
            }
        }

        earliest_ts
    }

    /// Get the set of all officially issued dynamic models collected across all currently available accounts
    pub fn get_all_collected_models(&self) -> std::collections::HashSet<String> {
        let mut all_models = std::collections::HashSet::new();
        for entry in self.tokens.iter() {
            let token = entry.value();

            // Keep the raw quota model IDs for /v1/models discovery. `model_quotas`
            // intentionally stores normalized protection buckets (e.g. gemini-3-flash),
            // but clients need concrete usable IDs such as gemini-3-flash-agent.
            if let Some(raw_models) = Self::get_available_models_from_json(&token.account_path) {
                for model_id in raw_models {
                    all_models.insert(model_id);
                }
            }

            // Also keep normalized bucket IDs for existing quota/protection behavior.
            for model_id in token.model_quotas.keys() {
                all_models.insert(model_id.clone());
            }
        }
        all_models
    }

    /// [NEW] Get a specific model's max_output_tokens from a given account's dynamic quota data
    ///
    /// # Returns
    /// - `Some(u64)`: dynamic limit data was found
    /// - `None`: the account doesn't exist or has no data for this model (the caller should fall back to the static default table)
    pub fn get_model_output_limit_for_account(
        &self,
        account_id: &str,
        model_name: &str,
    ) -> Option<u64> {
        self.tokens
            .get(account_id)
            .and_then(|token| token.model_limits.get(model_name).copied())
    }

    /// Helper to find account ID by email
    pub fn get_account_id_by_email(&self, email: &str) -> Option<String> {
        for entry in self.tokens.iter() {
            if entry.value().email == email {
                return Some(entry.key().clone());
            }
        }
        None
    }

    /// Set validation blocked status for an account (internal)
    pub async fn set_validation_block(
        &self,
        account_id: &str,
        block_until: i64,
        reason: &str,
    ) -> Result<(), String> {
        // 1. Update memory
        if let Some(mut token) = self.tokens.get_mut(account_id) {
            token.validation_blocked = true;
            token.validation_blocked_until = block_until;
        }

        // 2. Persist to disk
        let path = self
            .data_dir
            .join("accounts")
            .join(format!("{}.json", account_id));
        if !path.exists() {
            return Err(format!("Account file not found: {:?}", path));
        }

        // [NEW] Try to extract a validation link from the message (#1522)
        let extracted_url = if let Ok(parsed_json) =
            serde_json::from_str::<serde_json::Value>(reason)
        {
            // Try to extract it from the specific Google RPC error structure
            let mut url = None;
            if let Some(details) = parsed_json.pointer("/error/details") {
                if let Some(arr) = details.as_array() {
                    for detail in arr {
                        if let Some(meta) = detail.get("metadata") {
                            if let Some(v_url) = meta.get("validation_url").and_then(|v| v.as_str())
                            {
                                url = Some(v_url.to_string());
                                break;
                            }
                            if let Some(a_url) = meta.get("appeal_url").and_then(|v| v.as_str()) {
                                url = Some(a_url.to_string());
                                break;
                            }
                        }
                    }
                }
            }
            url
        } else {
            // Fallback: use a stricter regex and decode a possible \u0026 via deserialization
            let url_regex = regex::Regex::new(r#"https://[^\s"'\\]+"#).unwrap();
            url_regex.find(reason).map(|m| {
                let raw_url = m.as_str().to_string();
                raw_url.replace("\\u0026", "&")
            })
        };

        if let Some(ref url) = extracted_url {
            if let Some(mut token) = self.tokens.get_mut(account_id) {
                token.validation_url = Some(url.clone());
            }
        }

        let reason_owned = reason.to_string();
        update_account_json(&path, move |account| {
            account["validation_blocked"] = serde_json::Value::Bool(true);
            account["validation_blocked_until"] =
                serde_json::Value::Number(serde_json::Number::from(block_until));
            account["validation_blocked_reason"] = serde_json::Value::String(reason_owned);
            if let Some(url) = extracted_url {
                account["validation_url"] = serde_json::Value::String(url);
            }
        })
        .await?;

        // Clear sticky session if blocked
        self.session_accounts.retain(|_, v| *v != account_id);

        tracing::info!(
            "🚫 Account {} validation blocked until {} (reason: {})",
            account_id,
            block_until,
            reason
        );

        Ok(())
    }

    /// Public method to set validation block (called from handlers)
    pub async fn set_validation_block_public(
        &self,
        account_id: &str,
        block_until: i64,
        reason: &str,
    ) -> Result<(), String> {
        self.set_validation_block(account_id, block_until, reason)
            .await
    }

    /// Set is_forbidden status for an account (called when proxy encounters 403)
    pub async fn set_forbidden(&self, account_id: &str, reason: &str) -> Result<(), String> {
        // [FIX] Call the wrapped module function, to ensure the account file and index are updated thread-safely
        crate::modules::account::mark_account_forbidden(account_id, reason)?;

        // Clear sticky session if forbidden
        self.session_accounts.retain(|_, v| *v != account_id);

        // [FIX] Remove the account from the in-memory pool, to avoid it being selected again on retry
        self.remove_account(account_id);

        tracing::warn!(
            "🚫 Account {} marked as forbidden (403): {}",
            account_id,
            truncate_reason(reason, 1000)
        );

        Ok(())
    }
}

/// Truncate an overly long reason string
fn truncate_reason(reason: &str, max_len: usize) -> String {
    if reason.len() <= max_len {
        reason.to_string()
    } else {
        // [FIX] Ensure character truncation happens at a valid boundary, to prevent a panic
        let end = reason
            .char_indices()
            .map(|(i, _)| i)
            .filter(|&i| i <= max_len - 3)
            .last()
            .unwrap_or(0);
        format!("{}...", &reason[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn test_build_dynamic_model_candidates_agent() {
        let candidates = TokenManager::build_dynamic_model_candidates("gemini-pro-agent").unwrap();
        assert_eq!(candidates[0], "gemini-pro-agent");
        assert!(candidates.contains(&"gemini-3.1-pro-low".to_string()));

        let candidates_high =
            TokenManager::build_dynamic_model_candidates("gemini-3.1-pro-high").unwrap();
        assert_eq!(candidates_high[0], "gemini-3.1-pro-high");
        assert!(candidates_high.contains(&"gemini-pro-agent".to_string()));
    }

    #[tokio::test]
    async fn task_reload_account_preserves_live_limit_and_syncs_disabled_state() {
        let tmp_root = std::env::temp_dir().join(format!(
            "antigravity-token-manager-test-{}",
            uuid::Uuid::new_v4()
        ));
        let accounts_dir = tmp_root.join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap();

        let account_id = "acc1";
        let model = "gemini-3-pro-image";
        let email = "a@test.com";
        let now = chrono::Utc::now().timestamp();
        let account_path = accounts_dir.join(format!("{}.json", account_id));

        let account_json = serde_json::json!({
            "id": account_id,
            "email": email,
            "token": {
                "access_token": "atk",
                "refresh_token": "rtk",
                "expires_in": 3600,
                "expiry_timestamp": now + 3600
            },
            "disabled": false,
            "proxy_disabled": false,
            "created_at": now,
            "last_used": now,
            "live_limited_models": {
                model: {
                    "model": model,
                    "status": 429,
                    "reason": "QuotaExhausted",
                    "until": now + 7200,
                    "detected_at": now,
                    "message": "{\"error\":{\"details\":[{\"reason\":\"QUOTA_EXHAUSTED\",\"metadata\":{\"quotaResetDelay\":\"2h\"}}]}}"
                },
                "gemini-3.1-flash-image": {
                    "model": "gemini-3.1-flash-image",
                    "status": 429,
                    "reason": "QuotaExhausted",
                    "until": now + 7200,
                    "detected_at": now,
                    "message": "QUOTA_EXHAUSTED"
                },
                "gemini-2.5-pro": {
                    "model": "gemini-2.5-pro",
                    "status": 429,
                    "reason": "QuotaExhausted",
                    "until": now + 7200,
                    "detected_at": now,
                    "message": "QUOTA_EXHAUSTED; reset after 2h"
                }
            }
        });
        std::fs::write(
            &account_path,
            serde_json::to_string_pretty(&account_json).unwrap(),
        )
        .unwrap();

        let manager = TokenManager::new(tmp_root.clone());
        manager.load_accounts().await.unwrap();
        assert!(manager.tokens.get(account_id).is_some());
        assert!(manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some(model)));
        assert!(!manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some("gemini-3.1-flash-image")));
        assert!(!manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some("gemini-2.5-pro")));

        let temporary_model = "gemini-3.1-flash-image";
        manager.rate_limit_tracker.parse_from_error(
            account_id,
            429,
            Some("60"),
            "temporary rate limit",
            Some(temporary_model.to_string()),
            &[60, 300],
        );
        assert!(manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some(temporary_model)));
        assert!(manager.clear_rate_limit_memory(account_id));
        assert!(!manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some(model)));

        manager.reload_account(account_id).await.unwrap();
        assert!(manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some(model)));
        assert!(!manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some(temporary_model)));

        // Prime extra caches to ensure remove_account() is really called.
        manager
            .session_accounts
            .insert("sid1".to_string(), account_id.to_string());
        {
            let mut preferred = manager.preferred_account_id.write().await;
            *preferred = Some(account_id.to_string());
        }

        // Mark account as proxy-disabled on disk (manual disable).
        let mut disabled_json = account_json.clone();
        disabled_json["proxy_disabled"] = serde_json::Value::Bool(true);
        disabled_json["proxy_disabled_reason"] = serde_json::Value::String("manual".to_string());
        disabled_json["proxy_disabled_at"] = serde_json::Value::Number(now.into());
        std::fs::write(
            &account_path,
            serde_json::to_string_pretty(&disabled_json).unwrap(),
        )
        .unwrap();

        manager.reload_account(account_id).await.unwrap();

        assert!(manager.tokens.get(account_id).is_none());
        assert!(manager.session_accounts.get("sid1").is_none());
        assert!(manager.preferred_account_id.read().await.is_none());

        disabled_json["proxy_disabled"] = serde_json::Value::Bool(false);
        std::fs::write(
            &account_path,
            serde_json::to_string_pretty(&disabled_json).unwrap(),
        )
        .unwrap();
        manager.reload_account(account_id).await.unwrap();
        assert!(manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some(model)));

        let restarted = TokenManager::new(tmp_root.clone());
        restarted.load_accounts().await.unwrap();
        assert!(restarted
            .rate_limit_tracker
            .is_rate_limited(account_id, Some(model)));

        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    #[tokio::test]
    async fn task_account_json_update_preserves_live_limits() {
        let tmp_root = std::env::temp_dir().join(format!(
            "antigravity-account-update-test-{}",
            uuid::Uuid::new_v4()
        ));
        let accounts_dir = tmp_root.join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap();
        let account_id = "acc-update";
        let account_path = accounts_dir.join(format!("{}.json", account_id));
        let live_limit = serde_json::json!({
            "model": "gemini-3-pro-image",
            "status": 429,
            "reason": "QuotaExhausted",
            "until": chrono::Utc::now().timestamp() + 7200,
            "detected_at": chrono::Utc::now().timestamp(),
            "message": "QUOTA_EXHAUSTED; reset after 2h"
        });
        std::fs::write(
            &account_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": account_id,
                "live_limited_models": {
                    "gemini-3-pro-image": live_limit
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let manager = TokenManager::new(tmp_root.clone());
        manager.disable_account(account_id, "test").await.unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&account_path).unwrap()).unwrap();
        assert_eq!(
            updated["live_limited_models"]["gemini-3-pro-image"],
            live_limit
        );
        assert_eq!(updated["disabled"], true);

        let mut account_snapshot = updated;
        assert!(manager
            .trigger_quota_protection(
                &mut account_snapshot,
                account_id,
                &account_path,
                0,
                10,
                "gemini-3-flash",
            )
            .await
            .unwrap());
        assert!(manager
            .restore_quota_protection(
                &mut account_snapshot,
                account_id,
                &account_path,
                "gemini-3-flash",
            )
            .await
            .unwrap());

        account_snapshot["proxy_disabled"] = serde_json::Value::Bool(true);
        account_snapshot["proxy_disabled_reason"] =
            serde_json::Value::String("quota_protection".to_string());
        let quota = serde_json::json!({
            "models": [{ "name": "gemini-3-flash", "percentage": 0 }]
        });
        let config = crate::models::QuotaProtectionConfig {
            enabled: true,
            threshold_percentage: 10,
            monitored_models: vec!["gemini-3-flash".to_string()],
        };
        manager
            .check_and_restore_quota(&mut account_snapshot, &account_path, &quota, &config)
            .await;

        update_account_json(&account_path, |latest| {
            latest["validation_blocked"] = serde_json::Value::Bool(true);
            latest["validation_blocked_until"] =
                serde_json::Value::Number((chrono::Utc::now().timestamp() - 1).into());
            latest["validation_blocked_reason"] = serde_json::Value::String("expired".to_string());
        })
        .await
        .unwrap();
        manager.load_single_account(&account_path).await.unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&account_path).unwrap()).unwrap();
        assert_eq!(
            updated["live_limited_models"]["gemini-3-pro-image"],
            live_limit
        );
        assert_eq!(updated["validation_blocked"], false);
        assert_eq!(
            updated["protected_models"],
            serde_json::json!(["gemini-3-flash"])
        );

        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    #[tokio::test]
    async fn task_image_selection_respects_queue_deadline() {
        let result = wait_for_image_token_selection(
            tokio::time::Instant::now() + std::time::Duration::from_millis(20),
            std::future::pending::<()>(),
        )
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn task_image_queue_reselects_without_scheduler_notification() {
        let (_sender, mut changes) = tokio::sync::watch::channel(0);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_image_account_change(&mut changes, std::time::Duration::from_secs(1)),
        )
        .await;
        assert_eq!(result, Ok(true));
    }

    #[tokio::test]
    async fn task_short_limit_buffer_reselects_without_blocking_runtime() {
        let tmp_root = std::env::temp_dir().join(format!(
            "antigravity-short-limit-test-{}",
            uuid::Uuid::new_v4()
        ));
        let accounts_dir = tmp_root.join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap();
        let account_id = "acc-short-limit";
        let model = "gemini-3-flash";
        let now = chrono::Utc::now().timestamp();
        std::fs::write(
            accounts_dir.join(format!("{}.json", account_id)),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": account_id,
                "email": "short-limit@test.com",
                "token": {
                    "access_token": "atk",
                    "refresh_token": "rtk",
                    "expires_in": 3600,
                    "expiry_timestamp": now + 3600,
                    "project_id": "pid"
                },
                "quota": {
                    "models": [{ "name": model, "percentage": 100 }]
                },
                "disabled": false,
                "proxy_disabled": false,
                "created_at": now,
                "last_used": now
            }))
            .unwrap(),
        )
        .unwrap();

        let manager = TokenManager::new(tmp_root.clone());
        manager.load_accounts().await.unwrap();
        manager.rate_limit_tracker.parse_from_error(
            account_id,
            429,
            Some("1"),
            r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#,
            Some(model.to_string()),
            &[60, 300],
        );

        let selected = tokio::time::timeout(
            std::time::Duration::from_secs(4),
            manager.get_token("gemini", false, None, model),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(selected.3, account_id);
        assert!(!manager.is_rate_limited(account_id, Some(model)).await);

        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    #[tokio::test]
    async fn task_concurrent_image_limits_persist_and_clear_exact_bucket() {
        let tmp_root = std::env::temp_dir().join(format!(
            "antigravity-live-limit-test-{}",
            uuid::Uuid::new_v4()
        ));
        let accounts_dir = tmp_root.join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap();
        let account_id = "acc-concurrent";
        let account_path = accounts_dir.join(format!("{}.json", account_id));
        std::fs::write(
            &account_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": account_id,
                "email": "concurrent@test.com",
                "live_limited_models": {}
            }))
            .unwrap(),
        )
        .unwrap();

        let manager = TokenManager::new(tmp_root.clone());
        let body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED","metadata":{"quotaResetDelay":"72h"}}]}}"#;
        assert!(!manager
            .mark_rate_limited_fast(
                account_id,
                429,
                None,
                body,
                Some("gemini-3.1-flash-image"),
            )
            .await);
        assert!(manager
            .rate_limit_tracker
            .is_rate_limited(account_id, Some("gemini-3.1-flash-image")));
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&account_path).unwrap()).unwrap();
        let flash_limit = &persisted["live_limited_models"]["gemini-3.1-flash-image"];
        assert_eq!(flash_limit["status"], 429);
        assert!(
            flash_limit["until"].as_i64().unwrap() - chrono::Utc::now().timestamp() > 71 * 3600
        );

        manager.clear_persisted_live_limit(account_id, Some("gemini-3.1-flash-image-4k"));
        std::thread::scope(|scope| {
            for model in ["gemini-3.1-flash-image", "gemini-3-pro-image"] {
                let manager = &manager;
                scope.spawn(move || {
                    manager.record_rate_limit_atomic(
                        account_id,
                        429,
                        None,
                        body,
                        Some(model),
                        &[60, 300],
                        TrackerParserMode::Current,
                    );
                });
            }
        });

        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&account_path).unwrap()).unwrap();
        let limits = persisted["live_limited_models"].as_object().unwrap();
        assert!(limits.contains_key("gemini-3.1-flash-image"));
        assert!(limits.contains_key("gemini-3-pro-image"));

        manager.clear_persisted_live_limit(account_id, Some("gemini-3.1-flash-image-4k"));
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&account_path).unwrap()).unwrap();
        let limits = persisted["live_limited_models"].as_object().unwrap();
        assert!(!limits.contains_key("gemini-3.1-flash-image"));
        assert!(limits.contains_key("gemini-3-pro-image"));

        std::thread::scope(|scope| {
            scope.spawn(|| {
                manager.record_rate_limit_atomic(
                    account_id,
                    429,
                    None,
                    body,
                    Some("gemini-3-pro-image"),
                    &[60, 300],
                    TrackerParserMode::Current,
                );
            });
            scope.spawn(|| {
                manager.clear_persisted_live_limit(account_id, Some("gemini-3-pro-image"));
            });
        });
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&account_path).unwrap()).unwrap();
        let disk_has_limit = persisted["live_limited_models"]
            .as_object()
            .unwrap()
            .contains_key("gemini-3-pro-image");
        assert_eq!(
            disk_has_limit,
            manager
                .rate_limit_tracker
                .is_rate_limited(account_id, Some("gemini-3-pro-image"))
        );

        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    #[tokio::test]
    async fn test_fixed_account_mode_skips_preferred_when_disabled_on_disk_without_reload() {
        let tmp_root = std::env::temp_dir().join(format!(
            "antigravity-token-manager-test-fixed-mode-{}",
            uuid::Uuid::new_v4()
        ));
        let accounts_dir = tmp_root.join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap();

        let now = chrono::Utc::now().timestamp();

        let write_account = |id: &str, email: &str, proxy_disabled: bool| {
            let account_path = accounts_dir.join(format!("{}.json", id));
            let json = serde_json::json!({
                "id": id,
                "email": email,
                "token": {
                    "access_token": format!("atk-{}", id),
                    "refresh_token": format!("rtk-{}", id),
                    "expires_in": 3600,
                    "expiry_timestamp": now + 3600,
                    "project_id": format!("pid-{}", id)
                },
                "quota": {
                    "models": [
                        { "name": "gemini-1.5-flash", "percentage": 100 }
                    ]
                },
                "disabled": false,
                "proxy_disabled": proxy_disabled,
                "proxy_disabled_reason": if proxy_disabled { "manual" } else { "" },
                "quota": {
                    "models": [
                        { "name": "gemini-3-flash", "percentage": 100 }
                    ]
                },
                "created_at": now,
                "last_used": now
            });
            std::fs::write(&account_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        };

        // Two accounts in pool.
        write_account("acc1", "a@test.com", false);
        write_account("acc2", "b@test.com", false);

        let manager = TokenManager::new(tmp_root.clone());
        manager.load_accounts().await.unwrap();

        // Enable fixed account mode for acc1.
        manager
            .set_preferred_account(Some("acc1".to_string()))
            .await;

        // Disable acc1 on disk WITHOUT reloading the in-memory pool (simulates stale cache).
        write_account("acc1", "a@test.com", true);

        let (_token, _project_id, email, account_id, _wait_ms) = manager
            .get_token("gemini", false, Some("sid1"), "gemini-1.5-flash")
            .await
            .unwrap();

        // Should fall back to another account instead of using the disabled preferred one.
        assert_eq!(account_id, "acc2");
        assert_eq!(email, "b@test.com");
        assert!(manager.tokens.get("acc1").is_none());
        assert!(manager.get_preferred_account().await.is_none());

        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    #[tokio::test]
    async fn test_collected_models_preserve_raw_quota_model_names_for_model_listing() {
        let tmp_root = std::env::temp_dir().join(format!(
            "antigravity-token-manager-test-raw-models-{}",
            uuid::Uuid::new_v4()
        ));
        let accounts_dir = tmp_root.join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap();

        let now = chrono::Utc::now().timestamp();
        let account_path = accounts_dir.join("acc1.json");
        let account_json = serde_json::json!({
            "id": "acc1",
            "email": "a@test.com",
            "token": {
                "access_token": "atk",
                "refresh_token": "rtk",
                "expires_in": 3600,
                "expiry_timestamp": now + 3600
            },
            "quota": {
                "models": [
                    { "name": "gemini-3-flash-agent", "percentage": 88 }
                ]
            },
            "disabled": false,
            "proxy_disabled": false,
            "created_at": now,
            "last_used": now
        });
        std::fs::write(
            &account_path,
            serde_json::to_string_pretty(&account_json).unwrap(),
        )
        .unwrap();

        let manager = TokenManager::new(tmp_root.clone());
        manager.load_accounts().await.unwrap();

        let collected_models = manager.get_all_collected_models();
        assert!(collected_models.contains("gemini-3-flash-agent"));
        // Keep the normalized quota bucket too; it is used for quota/protection checks.
        assert!(collected_models.contains("gemini-3-flash"));

        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    #[tokio::test]
    async fn test_sticky_session_skips_bound_account_when_disabled_on_disk_without_reload() {
        let tmp_root = std::env::temp_dir().join(format!(
            "antigravity-token-manager-test-sticky-disabled-{}",
            uuid::Uuid::new_v4()
        ));
        let accounts_dir = tmp_root.join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap();

        let now = chrono::Utc::now().timestamp();

        let write_account = |id: &str, email: &str, percentage: i64, proxy_disabled: bool| {
            let account_path = accounts_dir.join(format!("{}.json", id));
            let json = serde_json::json!({
                "id": id,
                "email": email,
                "token": {
                    "access_token": format!("atk-{}", id),
                    "refresh_token": format!("rtk-{}", id),
                    "expires_in": 3600,
                    "expiry_timestamp": now + 3600,
                    "project_id": format!("pid-{}", id)
                },
                "quota": {
                    "models": [
                        { "name": "gemini-1.5-flash", "percentage": percentage }
                    ]
                },
                "disabled": false,
                "proxy_disabled": proxy_disabled,
                "proxy_disabled_reason": if proxy_disabled { "manual" } else { "" },
                "created_at": now,
                "last_used": now
            });
            std::fs::write(&account_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        };

        // Two accounts in pool. acc1 has higher quota -> should be selected and bound first.
        write_account("acc1", "a@test.com", 90, false);
        write_account("acc2", "b@test.com", 10, false);

        let manager = TokenManager::new(tmp_root.clone());
        manager.load_accounts().await.unwrap();

        // Prime: first request should bind the session to acc1.
        let (_token, _project_id, _email, account_id, _wait_ms) = manager
            .get_token("gemini", false, Some("sid1"), "gemini-1.5-flash")
            .await
            .unwrap();
        assert_eq!(account_id, "acc1");
        assert_eq!(
            manager.session_accounts.get("sid1").map(|v| v.clone()),
            Some("acc1".to_string())
        );

        // Disable acc1 on disk WITHOUT reloading the in-memory pool (simulates stale cache).
        write_account("acc1", "a@test.com", 90, true);

        let (_token, _project_id, email, account_id, _wait_ms) = manager
            .get_token("gemini", false, Some("sid1"), "gemini-1.5-flash")
            .await
            .unwrap();

        // Should fall back to another account instead of reusing the disabled bound one.
        assert_eq!(account_id, "acc2");
        assert_eq!(email, "b@test.com");
        assert!(manager.tokens.get("acc1").is_none());
        assert_ne!(
            manager.session_accounts.get("sid1").map(|v| v.clone()),
            Some("acc1".to_string())
        );

        let _ = std::fs::remove_dir_all(&tmp_root);
    }

    /// Create a ProxyToken for testing
    fn create_test_token(
        email: &str,
        tier: Option<&str>,
        health_score: f32,
        reset_time: Option<i64>,
        remaining_quota: Option<i32>,
    ) -> ProxyToken {
        ProxyToken {
            account_id: email.to_string(),
            access_token: "test_token".to_string(),
            refresh_token: "test_refresh".to_string(),
            expires_in: 3600,
            timestamp: chrono::Utc::now().timestamp() + 3600,
            email: email.to_string(),
            account_path: PathBuf::from("/tmp/test"),
            project_id: None,
            subscription_tier: tier.map(|s| s.to_string()),
            remaining_quota,
            protected_models: HashSet::new(),
            health_score,
            reset_time,
            validation_blocked: false,
            validation_blocked_until: 0,
            validation_url: None,
            model_quotas: HashMap::new(),
            model_limits: HashMap::new(),
        }
    }

    /// Test the sort comparison function (matches the logic in get_token_internal)
    fn compare_tokens(a: &ProxyToken, b: &ProxyToken) -> Ordering {
        const RESET_TIME_THRESHOLD_SECS: i64 = 600; // 10-minute threshold

        let tier_priority = |tier: &Option<String>| {
            let t = tier.as_deref().unwrap_or("").to_lowercase();
            if t.contains("ultra") {
                0
            } else if t.contains("pro") {
                1
            } else if t.contains("free") {
                2
            } else {
                3
            }
        };

        // First: compare by subscription tier
        let tier_cmp =
            tier_priority(&a.subscription_tier).cmp(&tier_priority(&b.subscription_tier));
        if tier_cmp != Ordering::Equal {
            return tier_cmp;
        }

        // Second: compare by health score (higher is better)
        let health_cmp = b
            .health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(Ordering::Equal);
        if health_cmp != Ordering::Equal {
            return health_cmp;
        }

        // Third: compare by reset time (earlier/closer is better)
        let reset_a = a.reset_time.unwrap_or(i64::MAX);
        let reset_b = b.reset_time.unwrap_or(i64::MAX);
        let reset_diff = (reset_a - reset_b).abs();

        if reset_diff >= RESET_TIME_THRESHOLD_SECS {
            let reset_cmp = reset_a.cmp(&reset_b);
            if reset_cmp != Ordering::Equal {
                return reset_cmp;
            }
        }

        // Fourth: compare by remaining quota percentage (higher is better)
        let quota_a = a.remaining_quota.unwrap_or(0);
        let quota_b = b.remaining_quota.unwrap_or(0);
        quota_b.cmp(&quota_a)
    }

    #[test]
    fn test_sorting_tier_priority() {
        // ULTRA > PRO > FREE
        let ultra = create_test_token("ultra@test.com", Some("ULTRA"), 1.0, None, Some(50));
        let pro = create_test_token("pro@test.com", Some("PRO"), 1.0, None, Some(50));
        let free = create_test_token("free@test.com", Some("FREE"), 1.0, None, Some(50));

        assert_eq!(compare_tokens(&ultra, &pro), Ordering::Less);
        assert_eq!(compare_tokens(&pro, &free), Ordering::Less);
        assert_eq!(compare_tokens(&ultra, &free), Ordering::Less);
        assert_eq!(compare_tokens(&free, &ultra), Ordering::Greater);
    }

    #[test]
    fn test_sorting_health_score_priority() {
        // Within the same tier, a higher health score takes priority
        let high_health = create_test_token("high@test.com", Some("PRO"), 1.0, None, Some(50));
        let low_health = create_test_token("low@test.com", Some("PRO"), 0.5, None, Some(50));

        assert_eq!(compare_tokens(&high_health, &low_health), Ordering::Less);
        assert_eq!(compare_tokens(&low_health, &high_health), Ordering::Greater);
    }

    #[test]
    fn test_sorting_reset_time_priority() {
        let now = chrono::Utc::now().timestamp();

        // A sooner refresh time (30 minutes) takes priority over a farther one (5 hours)
        let soon_reset = create_test_token(
            "soon@test.com",
            Some("PRO"),
            1.0,
            Some(now + 1800),
            Some(50),
        ); // 30 minutes later
        let late_reset = create_test_token(
            "late@test.com",
            Some("PRO"),
            1.0,
            Some(now + 18000),
            Some(50),
        ); // 5 hours later

        assert_eq!(compare_tokens(&soon_reset, &late_reset), Ordering::Less);
        assert_eq!(compare_tokens(&late_reset, &soon_reset), Ordering::Greater);
    }

    #[test]
    fn test_sorting_reset_time_threshold() {
        let now = chrono::Utc::now().timestamp();

        // A difference under 10 minutes (600 seconds) is treated as equal priority; sort by quota in that case
        let reset_a = create_test_token("a@test.com", Some("PRO"), 1.0, Some(now + 1800), Some(80)); // 30 minutes later, 80% quota
        let reset_b = create_test_token("b@test.com", Some("PRO"), 1.0, Some(now + 2100), Some(50)); // 35 minutes later, 50% quota

        // A 5-minute difference < the 10-minute threshold, treated as equal, sorted by quota (80% > 50%)
        assert_eq!(compare_tokens(&reset_a, &reset_b), Ordering::Less);
    }

    #[test]
    fn test_sorting_reset_time_beyond_threshold() {
        let now = chrono::Utc::now().timestamp();

        // A difference over 10 minutes is sorted by refresh time (quota ignored)
        let soon_low_quota = create_test_token(
            "soon@test.com",
            Some("PRO"),
            1.0,
            Some(now + 1800),
            Some(20),
        ); // 30 minutes later, 20%
        let late_high_quota = create_test_token(
            "late@test.com",
            Some("PRO"),
            1.0,
            Some(now + 18000),
            Some(90),
        ); // 5 hours later, 90%

        // A 4.5-hour difference > 10 minutes, refresh time takes priority, 30 minutes < 5 hours
        assert_eq!(
            compare_tokens(&soon_low_quota, &late_high_quota),
            Ordering::Less
        );
    }

    #[test]
    fn test_sorting_quota_fallback() {
        // With other conditions equal, higher quota takes priority
        let high_quota = create_test_token("high@test.com", Some("PRO"), 1.0, None, Some(80));
        let low_quota = create_test_token("low@test.com", Some("PRO"), 1.0, None, Some(20));

        assert_eq!(compare_tokens(&high_quota, &low_quota), Ordering::Less);
        assert_eq!(compare_tokens(&low_quota, &high_quota), Ordering::Greater);
    }

    #[test]
    fn test_sorting_missing_reset_time() {
        let now = chrono::Utc::now().timestamp();

        // An account without a reset_time should sort after one that has a reset_time
        let with_reset = create_test_token(
            "with@test.com",
            Some("PRO"),
            1.0,
            Some(now + 1800),
            Some(50),
        );
        let without_reset = create_test_token("without@test.com", Some("PRO"), 1.0, None, Some(50));

        assert_eq!(compare_tokens(&with_reset, &without_reset), Ordering::Less);
    }

    #[test]
    fn test_full_sorting_integration() {
        let now = chrono::Utc::now().timestamp();

        let mut tokens = vec![
            create_test_token(
                "free_high@test.com",
                Some("FREE"),
                1.0,
                Some(now + 1800),
                Some(90),
            ),
            create_test_token(
                "pro_low_health@test.com",
                Some("PRO"),
                0.5,
                Some(now + 1800),
                Some(90),
            ),
            create_test_token(
                "pro_soon@test.com",
                Some("PRO"),
                1.0,
                Some(now + 1800),
                Some(50),
            ), // 30 minutes later
            create_test_token(
                "pro_late@test.com",
                Some("PRO"),
                1.0,
                Some(now + 18000),
                Some(90),
            ), // 5 hours later
            create_test_token(
                "ultra@test.com",
                Some("ULTRA"),
                1.0,
                Some(now + 36000),
                Some(10),
            ),
        ];

        tokens.sort_by(compare_tokens);

        // Expected order:
        // 1. ULTRA (highest tier, even with the farthest refresh time)
        // 2. PRO + high health score + refresh in 30 minutes
        // 3. PRO + high health score + refresh in 5 hours
        // 4. PRO + low health score
        // 5. FREE (lowest tier, even with the highest quota)
        assert_eq!(tokens[0].email, "ultra@test.com");
        assert_eq!(tokens[1].email, "pro_soon@test.com");
        assert_eq!(tokens[2].email, "pro_late@test.com");
        assert_eq!(tokens[3].email, "pro_low_health@test.com");
        assert_eq!(tokens[4].email, "free_high@test.com");
    }

    #[test]
    fn test_realistic_scenario() {
        // Simulate the scenario described by the user:
        // account a's claude refreshes in 4h55m
        // account b's claude refreshes in 31m
        // b should be preferred (refreshes in 31 minutes)
        let now = chrono::Utc::now().timestamp();

        let account_a = create_test_token(
            "a@test.com",
            Some("PRO"),
            1.0,
            Some(now + 295 * 60),
            Some(80),
        ); // 4h55m
        let account_b = create_test_token(
            "b@test.com",
            Some("PRO"),
            1.0,
            Some(now + 31 * 60),
            Some(30),
        ); // 31m

        // b should sort before a (sooner refresh time)
        assert_eq!(compare_tokens(&account_b, &account_a), Ordering::Less);

        let mut tokens = vec![account_a.clone(), account_b.clone()];
        tokens.sort_by(compare_tokens);

        assert_eq!(tokens[0].email, "b@test.com");
        assert_eq!(tokens[1].email, "a@test.com");
    }

    #[test]
    fn test_extract_earliest_reset_time() {
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        // Test reset_time extraction with a claude model included
        let account_with_claude = serde_json::json!({
            "quota": {
                "models": [
                    {"name": "gemini-flash", "reset_time": "2025-01-31T10:00:00Z"},
                    {"name": "claude-sonnet", "reset_time": "2025-01-31T08:00:00Z"},
                    {"name": "claude-opus", "reset_time": "2025-01-31T08:00:00Z"}
                ]
            }
        });

        let result = manager.extract_earliest_reset_time(&account_with_claude);
        assert!(result.is_some());
        // Should return claude's time (08:00), not gemini's (10:00)
        let expected_ts = chrono::DateTime::parse_from_rfc3339("2025-01-31T08:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(result.unwrap(), expected_ts);
    }

    #[test]
    fn test_extract_reset_time_no_claude() {
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        // With no claude model, should take the nearest time from any model
        let account_no_claude = serde_json::json!({
            "quota": {
                "models": [
                    {"name": "gemini-flash", "reset_time": "2025-01-31T10:00:00Z"},
                    {"name": "gemini-pro", "reset_time": "2025-01-31T08:00:00Z"}
                ]
            }
        });

        let result = manager.extract_earliest_reset_time(&account_no_claude);
        assert!(result.is_some());
        let expected_ts = chrono::DateTime::parse_from_rfc3339("2025-01-31T08:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(result.unwrap(), expected_ts);
    }

    #[test]
    fn test_extract_reset_time_missing_quota() {
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        // Should return None when there is no quota field
        let account_no_quota = serde_json::json!({
            "email": "test@test.com"
        });

        assert!(manager
            .extract_earliest_reset_time(&account_no_quota)
            .is_none());
    }

    // ===== P2C algorithm tests =====

    /// Create a test Token with protected_models
    fn create_test_token_with_protected(
        email: &str,
        remaining_quota: Option<i32>,
        protected_models: HashSet<String>,
    ) -> ProxyToken {
        ProxyToken {
            account_id: email.to_string(),
            access_token: "test_token".to_string(),
            refresh_token: "test_refresh".to_string(),
            expires_in: 3600,
            timestamp: chrono::Utc::now().timestamp() + 3600,
            email: email.to_string(),
            account_path: PathBuf::from("/tmp/test"),
            project_id: None,
            subscription_tier: Some("PRO".to_string()),
            remaining_quota,
            protected_models,
            health_score: 1.0,
            reset_time: None,
            validation_blocked: false,
            validation_blocked_until: 0,
            validation_url: None,
            model_quotas: HashMap::new(),
            model_limits: HashMap::new(),
        }
    }

    #[test]
    fn test_p2c_selects_higher_quota() {
        // P2C should select the account with the higher quota
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        let low_quota = create_test_token("low@test.com", Some("PRO"), 1.0, None, Some(20));
        let high_quota = create_test_token("high@test.com", Some("PRO"), 1.0, None, Some(80));

        let candidates = vec![low_quota, high_quota];
        let attempted: HashSet<String> = HashSet::new();

        // Run multiple times to make sure the high-quota account is selected
        for _ in 0..10 {
            let result = manager.select_with_p2c(&candidates, &attempted, "claude-sonnet", false);
            assert!(result.is_some());
            // P2C picks the higher-quota one from two candidates
            // Since there are only two candidates, high_quota should always be selected
            assert_eq!(result.unwrap().email, "high@test.com");
        }
    }

    #[test]
    fn test_p2c_skips_attempted() {
        // P2C should skip already-attempted accounts
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        let token_a = create_test_token("a@test.com", Some("PRO"), 1.0, None, Some(80));
        let token_b = create_test_token("b@test.com", Some("PRO"), 1.0, None, Some(50));

        let candidates = vec![token_a, token_b];
        let mut attempted: HashSet<String> = HashSet::new();
        attempted.insert("a@test.com".to_string());

        let result = manager.select_with_p2c(&candidates, &attempted, "claude-sonnet", false);
        assert!(result.is_some());
        assert_eq!(result.unwrap().email, "b@test.com");
    }

    #[test]
    fn test_p2c_skips_protected_models() {
        // P2C should skip accounts that are protected for the target model (quota_protection_enabled = true)
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        let mut protected = HashSet::new();
        protected.insert("claude-sonnet".to_string());

        let protected_account =
            create_test_token_with_protected("protected@test.com", Some(90), protected);
        let normal_account =
            create_test_token_with_protected("normal@test.com", Some(50), HashSet::new());

        let candidates = vec![protected_account, normal_account];
        let attempted: HashSet<String> = HashSet::new();

        let result = manager.select_with_p2c(&candidates, &attempted, "claude-sonnet", true);
        assert!(result.is_some());
        assert_eq!(result.unwrap().email, "normal@test.com");
    }

    #[test]
    fn test_p2c_single_candidate() {
        // Returns directly when there's a single candidate
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        let token = create_test_token("single@test.com", Some("PRO"), 1.0, None, Some(50));
        let candidates = vec![token];
        let attempted: HashSet<String> = HashSet::new();

        let result = manager.select_with_p2c(&candidates, &attempted, "claude-sonnet", false);
        assert!(result.is_some());
        assert_eq!(result.unwrap().email, "single@test.com");
    }

    #[test]
    fn test_p2c_empty_candidates() {
        // Returns None for empty candidates
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        let candidates: Vec<ProxyToken> = vec![];
        let attempted: HashSet<String> = HashSet::new();

        let result = manager.select_with_p2c(&candidates, &attempted, "claude-sonnet", false);
        assert!(result.is_none());
    }

    #[test]
    fn test_p2c_all_attempted() {
        // Returns None when all accounts have already been attempted
        let manager = TokenManager::new(PathBuf::from("/tmp/test"));

        let token_a = create_test_token("a@test.com", Some("PRO"), 1.0, None, Some(80));
        let token_b = create_test_token("b@test.com", Some("PRO"), 1.0, None, Some(50));

        let candidates = vec![token_a, token_b];
        let mut attempted: HashSet<String> = HashSet::new();
        attempted.insert("a@test.com".to_string());
        attempted.insert("b@test.com".to_string());

        let result = manager.select_with_p2c(&candidates, &attempted, "claude-sonnet", false);
        assert!(result.is_none());
    }

    // ===== Ultra priority logic tests =====

    /// Test the is_ultra_required_model helper function
    #[test]
    fn test_is_ultra_required_model() {
        // Premium models that require an Ultra account
        const ULTRA_REQUIRED_MODELS: &[&str] = &["claude-opus-4-6", "claude-opus-4-5", "opus"];

        fn is_ultra_required_model(model: &str) -> bool {
            let lower = model.to_lowercase();
            ULTRA_REQUIRED_MODELS.iter().any(|m| lower.contains(m))
        }

        // Should be recognized as a premium model
        assert!(is_ultra_required_model("claude-opus-4-6"));
        assert!(is_ultra_required_model("claude-opus-4-5"));
        assert!(is_ultra_required_model("Claude-Opus-4-6")); // Case-insensitive
        assert!(is_ultra_required_model("CLAUDE-OPUS-4-5")); // Case-insensitive
        assert!(is_ultra_required_model("opus")); // Wildcard match
        assert!(is_ultra_required_model("opus-4-6-latest"));
        assert!(is_ultra_required_model("models/claude-opus-4-6"));

        // Should be recognized as a regular model
        assert!(!is_ultra_required_model("claude-sonnet-4-5"));
        assert!(!is_ultra_required_model("claude-sonnet"));
        assert!(!is_ultra_required_model("gemini-1.5-flash"));
        assert!(!is_ultra_required_model("gemini-2.0-pro"));
        assert!(!is_ultra_required_model("claude-haiku"));
    }

    /// Test premium model sorting: Ultra accounts take priority over Pro accounts (even with a higher Pro quota)
    #[test]
    fn test_ultra_priority_for_high_end_models() {
        const RESET_TIME_THRESHOLD_SECS: i64 = 600;

        // Simulate the premium model sorting logic
        fn compare_tokens_for_model(
            a: &ProxyToken,
            b: &ProxyToken,
            target_model: &str,
        ) -> Ordering {
            const ULTRA_REQUIRED_MODELS: &[&str] = &["claude-opus-4-6", "claude-opus-4-5", "opus"];
            let requires_ultra = {
                let lower = target_model.to_lowercase();
                ULTRA_REQUIRED_MODELS.iter().any(|m| lower.contains(m))
            };

            let tier_priority = |tier: &Option<String>| {
                let t = tier.as_deref().unwrap_or("").to_lowercase();
                if t.contains("ultra") {
                    0
                } else if t.contains("pro") {
                    1
                } else if t.contains("free") {
                    2
                } else {
                    3
                }
            };

            // Priority 0: for premium models, subscription tier takes priority
            if requires_ultra {
                let tier_cmp =
                    tier_priority(&a.subscription_tier).cmp(&tier_priority(&b.subscription_tier));
                if tier_cmp != Ordering::Equal {
                    return tier_cmp;
                }
            }

            // Priority 1: Quota (higher is better)
            let quota_a = a.remaining_quota.unwrap_or(0);
            let quota_b = b.remaining_quota.unwrap_or(0);
            let quota_cmp = quota_b.cmp(&quota_a);
            if quota_cmp != Ordering::Equal {
                return quota_cmp;
            }

            // Priority 2: Health score
            let health_cmp = b
                .health_score
                .partial_cmp(&a.health_score)
                .unwrap_or(Ordering::Equal);
            if health_cmp != Ordering::Equal {
                return health_cmp;
            }

            // Priority 3: Tier (for non-high-end models)
            if !requires_ultra {
                let tier_cmp =
                    tier_priority(&a.subscription_tier).cmp(&tier_priority(&b.subscription_tier));
                if tier_cmp != Ordering::Equal {
                    return tier_cmp;
                }
            }

            Ordering::Equal
        }

        // Create test accounts: Ultra low quota vs Pro high quota
        let ultra_low_quota =
            create_test_token("ultra@test.com", Some("ULTRA"), 1.0, None, Some(20));
        let pro_high_quota = create_test_token("pro@test.com", Some("PRO"), 1.0, None, Some(80));

        // Premium model (Opus 4.6): Ultra should take priority, even with a lower quota
        assert_eq!(
            compare_tokens_for_model(&ultra_low_quota, &pro_high_quota, "claude-opus-4-6"),
            Ordering::Less, // Ultra sorts first
            "Opus 4.6 should prefer Ultra account over Pro even with lower quota"
        );

        // Premium model (Opus 4.5): Ultra should take priority
        assert_eq!(
            compare_tokens_for_model(&ultra_low_quota, &pro_high_quota, "claude-opus-4-5"),
            Ordering::Less,
            "Opus 4.5 should prefer Ultra account over Pro"
        );

        // Regular model (Sonnet): high-quota Pro should take priority
        assert_eq!(
            compare_tokens_for_model(&ultra_low_quota, &pro_high_quota, "claude-sonnet-4-5"),
            Ordering::Greater, // Pro (high quota) sorts first
            "Sonnet should prefer high-quota Pro over low-quota Ultra"
        );

        // Regular model (Flash): high-quota Pro should take priority
        assert_eq!(
            compare_tokens_for_model(&ultra_low_quota, &pro_high_quota, "gemini-1.5-flash"),
            Ordering::Greater,
            "Flash should prefer high-quota Pro over low-quota Ultra"
        );
    }

    /// Test sorting: when both accounts are Ultra, sort by quota
    #[test]
    fn test_ultra_accounts_sorted_by_quota() {
        fn compare_tokens_for_model(
            a: &ProxyToken,
            b: &ProxyToken,
            target_model: &str,
        ) -> Ordering {
            const ULTRA_REQUIRED_MODELS: &[&str] = &["claude-opus-4-6", "claude-opus-4-5", "opus"];
            let requires_ultra = {
                let lower = target_model.to_lowercase();
                ULTRA_REQUIRED_MODELS.iter().any(|m| lower.contains(m))
            };

            let tier_priority = |tier: &Option<String>| {
                let t = tier.as_deref().unwrap_or("").to_lowercase();
                if t.contains("ultra") {
                    0
                } else if t.contains("pro") {
                    1
                } else if t.contains("free") {
                    2
                } else {
                    3
                }
            };

            if requires_ultra {
                let tier_cmp =
                    tier_priority(&a.subscription_tier).cmp(&tier_priority(&b.subscription_tier));
                if tier_cmp != Ordering::Equal {
                    return tier_cmp;
                }
            }

            let quota_a = a.remaining_quota.unwrap_or(0);
            let quota_b = b.remaining_quota.unwrap_or(0);
            quota_b.cmp(&quota_a)
        }

        let ultra_high =
            create_test_token("ultra_high@test.com", Some("ULTRA"), 1.0, None, Some(80));
        let ultra_low = create_test_token("ultra_low@test.com", Some("ULTRA"), 1.0, None, Some(20));

        // Opus 4.6: both Ultra, higher quota takes priority
        assert_eq!(
            compare_tokens_for_model(&ultra_high, &ultra_low, "claude-opus-4-6"),
            Ordering::Less, // ultra_high sorts first
            "Among Ultra accounts, higher quota should come first"
        );
    }

    /// Test the full sorting scenario: a mixed account pool
    #[test]
    fn test_full_sorting_mixed_accounts() {
        fn sort_tokens_for_model(tokens: &mut Vec<ProxyToken>, target_model: &str) {
            const ULTRA_REQUIRED_MODELS: &[&str] = &["claude-opus-4-6", "claude-opus-4-5", "opus"];
            let requires_ultra = {
                let lower = target_model.to_lowercase();
                ULTRA_REQUIRED_MODELS.iter().any(|m| lower.contains(m))
            };

            tokens.sort_by(|a, b| {
                let tier_priority = |tier: &Option<String>| {
                    let t = tier.as_deref().unwrap_or("").to_lowercase();
                    if t.contains("ultra") {
                        0
                    } else if t.contains("pro") {
                        1
                    } else if t.contains("free") {
                        2
                    } else {
                        3
                    }
                };

                if requires_ultra {
                    let tier_cmp = tier_priority(&a.subscription_tier)
                        .cmp(&tier_priority(&b.subscription_tier));
                    if tier_cmp != Ordering::Equal {
                        return tier_cmp;
                    }
                }

                let quota_a = a.remaining_quota.unwrap_or(0);
                let quota_b = b.remaining_quota.unwrap_or(0);
                let quota_cmp = quota_b.cmp(&quota_a);
                if quota_cmp != Ordering::Equal {
                    return quota_cmp;
                }

                if !requires_ultra {
                    let tier_cmp = tier_priority(&a.subscription_tier)
                        .cmp(&tier_priority(&b.subscription_tier));
                    if tier_cmp != Ordering::Equal {
                        return tier_cmp;
                    }
                }

                Ordering::Equal
            });
        }

        // Create a mixed account pool
        let ultra_high =
            create_test_token("ultra_high@test.com", Some("ULTRA"), 1.0, None, Some(80));
        let ultra_low = create_test_token("ultra_low@test.com", Some("ULTRA"), 1.0, None, Some(20));
        let pro_high = create_test_token("pro_high@test.com", Some("PRO"), 1.0, None, Some(90));
        let pro_low = create_test_token("pro_low@test.com", Some("PRO"), 1.0, None, Some(30));
        let free = create_test_token("free@test.com", Some("FREE"), 1.0, None, Some(100));

        // Premium model (Opus 4.6) sorting
        let mut tokens_opus = vec![
            pro_high.clone(),
            free.clone(),
            ultra_low.clone(),
            pro_low.clone(),
            ultra_high.clone(),
        ];
        sort_tokens_for_model(&mut tokens_opus, "claude-opus-4-6");

        let emails_opus: Vec<&str> = tokens_opus.iter().map(|t| t.email.as_str()).collect();
        // Expected order: Ultra(high quota) > Ultra(low quota) > Pro(high quota) > Pro(low quota) > Free
        assert_eq!(
            emails_opus,
            vec![
                "ultra_high@test.com",
                "ultra_low@test.com",
                "pro_high@test.com",
                "pro_low@test.com",
                "free@test.com"
            ],
            "Opus 4.6 should sort Ultra first, then by quota within each tier"
        );

        // Regular model (Sonnet) sorting
        let mut tokens_sonnet = vec![
            pro_high.clone(),
            free.clone(),
            ultra_low.clone(),
            pro_low.clone(),
            ultra_high.clone(),
        ];
        sort_tokens_for_model(&mut tokens_sonnet, "claude-sonnet-4-5");

        let emails_sonnet: Vec<&str> = tokens_sonnet.iter().map(|t| t.email.as_str()).collect();
        // Expected order: Free(100%) > Pro(90%) > Ultra(80%) > Pro(30%) > Ultra(20%) - quota takes priority
        assert_eq!(
            emails_sonnet,
            vec![
                "free@test.com",
                "pro_high@test.com",
                "ultra_high@test.com",
                "pro_low@test.com",
                "ultra_low@test.com"
            ],
            "Sonnet should sort by quota first, then by tier as tiebreaker"
        );
    }
}
