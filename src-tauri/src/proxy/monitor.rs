use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{RwLock, Semaphore};

// Admission happens before spawn_blocking, so queued tasks cannot retain unlimited bodies.
static LOG_WRITERS: Semaphore = Semaphore::const_new(4);

/// 因写盘闸门繁忙而丢弃的请求日志计数（仅用于限流上报，不影响业务）
static LOG_DROPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRequestLog {
    pub id: String,
    pub timestamp: i64,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub duration: u64,                // ms
    pub model: Option<String>,        // Model name requested by the client
    pub mapped_model: Option<String>, // Model name actually used after routing
    #[serde(default)]
    pub upstream_model: Option<String>, // Physical upstream model version
    pub account_email: Option<String>,
    pub client_ip: Option<String>, // Client IP address
    pub error: Option<String>,
    pub request_body: Option<String>,
    pub upstream_request_body: Option<String>, // 网关转出给上游(Antigravity)的报文
    pub response_body: Option<String>,
    #[serde(default)]
    pub request_headers: Option<String>,
    #[serde(default)]
    pub upstream_request_headers: Option<String>,
    #[serde(default)]
    pub response_headers: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cached_tokens: Option<u32>,
    #[serde(default)]
    pub total_tokens: Option<u32>,
    #[serde(default)]
    pub physical_requests: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub protocol: Option<String>, // Protocol type: "openai", "anthropic", "gemini"
    pub username: Option<String>, // User token username
    #[serde(default)]
    pub session_id: Option<String>, // 会话标识 (如 sess-xxx)
}

#[cfg(test)]
pub(crate) mod prompt_log_tests {
    use super::*;

    pub(crate) fn sample_log(id: &str, bytes: usize) -> ProxyRequestLog {
        serde_json::from_value(serde_json::json!({
            "id": id, "timestamp": chrono::Utc::now().timestamp_millis(),
            "method": "POST", "url": "/v1/chat/completions", "status": 500, "duration": 10,
            "request_body": "q".repeat(bytes), "response_body": "错".repeat(bytes),
            "error": "错".repeat(2048)
        }))
        .unwrap()
    }

    pub(crate) fn test_monitor() -> ProxyMonitor {
        ProxyMonitor {
            logs: RwLock::new(VecDeque::new()),
            stats: RwLock::new(ProxyStats::default()),
            max_logs: 100,
            enabled: Arc::new(AtomicBool::new(true)),
            capture_health_logs: Arc::new(AtomicBool::new(false)),
            app_handle: None,
        }
    }

    // Tests using ABV_DATA_DIR run serially and restore the previous process setting.
    // Unified with crate::modules::account::SHARED_TEST_ENV_LOCK to prevent split-brain mutex race.
    thread_local! {
        static REENTRANCY_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    pub(crate) struct TestDataDir {
        _guard: Option<std::sync::MutexGuard<'static, ()>>,
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }
    impl TestDataDir {
        pub(crate) fn new() -> Self {
            let depth = REENTRANCY_DEPTH.get();
            let guard = if depth == 0 {
                Some(
                    crate::modules::account::SHARED_TEST_ENV_LOCK
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()),
                )
            } else {
                None
            };
            REENTRANCY_DEPTH.set(depth + 1);

            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("ABV_DATA_DIR");
            std::env::set_var("ABV_DATA_DIR", dir.path());
            Self {
                _guard: guard,
                _dir: dir,
                previous,
            }
        }
    }
    impl Drop for TestDataDir {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var("ABV_DATA_DIR", previous);
            } else {
                std::env::remove_var("ABV_DATA_DIR");
            }
            let depth = REENTRANCY_DEPTH.get();
            if depth > 0 {
                REENTRANCY_DEPTH.set(depth - 1);
            }
        }
    }

    #[tokio::test]
    async fn prompt_log_memory_summary_and_database_detail() {
        let _dir = TestDataDir::new();
        crate::modules::proxy_db::init_db().unwrap();
        let monitor = ProxyMonitor {
            logs: RwLock::new(VecDeque::new()),
            stats: RwLock::new(ProxyStats::default()),
            max_logs: 2,
            enabled: Arc::new(AtomicBool::new(true)),
            capture_health_logs: Arc::new(AtomicBool::new(false)),
            app_handle: None,
        };
        let mut log = sample_log("detail", 4096);
        log.input_tokens = Some(100);
        log.output_tokens = Some(2);
        log.cached_tokens = Some(0);
        log.physical_requests = Some(3);
        log.total_tokens = Some(123);
        let response = log.response_body.clone();
        monitor.log_request(log).await;
        let _finished = LOG_WRITERS.acquire_many(4).await.unwrap();
        let logs = monitor.logs.read().await;
        assert!(logs[0].request_body.is_none() && logs[0].response_body.is_none());
        assert_eq!(logs[0].error.as_ref().unwrap().chars().count(), 1024);
        let detail = crate::modules::proxy_db::get_log_detail("detail").unwrap();
        assert_eq!(detail.response_body, response);
        for record in [
            detail.clone(),
            crate::modules::proxy_db::get_logs_summary(10, 0)
                .unwrap()
                .remove(0),
            crate::modules::proxy_db::get_logs_filtered("", false, 10, 0)
                .unwrap()
                .remove(0),
            crate::modules::proxy_db::get_all_logs_for_export()
                .unwrap()
                .remove(0),
        ] {
            assert_eq!(record.cached_tokens, Some(0));
            assert_eq!(record.physical_requests, Some(3));
            assert_eq!(record.total_tokens, Some(123));
            assert_eq!(record.input_tokens, Some(100));
        }
        assert_eq!(
            detail.request_body.as_deref(),
            Some("q".repeat(4096).as_str())
        );
        monitor.set_enabled(false);
        monitor.log_request(sample_log("disabled", 4096)).await;
        assert!(crate::modules::proxy_db::get_log_detail("disabled").is_err());
    }

    #[test]
    fn test_proxy_request_log_upstream_model_serialization() {
        let json_data = serde_json::json!({
            "id": "test-123",
            "timestamp": 1234567890,
            "method": "POST",
            "url": "/v1/chat/completions",
            "status": 200,
            "duration": 50,
            "model": "gpt-4o",
            "mapped_model": "gemini-2.5-pro",
            "upstream_model": "gemini-2.5-pro-preview-05-06"
        });

        let log: ProxyRequestLog = serde_json::from_value(json_data).unwrap();
        assert_eq!(
            log.upstream_model.as_deref(),
            Some("gemini-2.5-pro-preview-05-06")
        );

        // Summary retains upstream_model
        let summary = log.summary();
        assert_eq!(
            summary.upstream_model.as_deref(),
            Some("gemini-2.5-pro-preview-05-06")
        );

        // Round-trip serialization
        let serialized = serde_json::to_string(&log).unwrap();
        let deserialized: ProxyRequestLog = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized.upstream_model.as_deref(),
            Some("gemini-2.5-pro-preview-05-06")
        );

        // Default when omitted
        let json_no_upstream = serde_json::json!({
            "id": "test-456",
            "timestamp": 1234567890,
            "method": "POST",
            "url": "/v1/chat/completions",
            "status": 200,
            "duration": 50
        });
        let log_default: ProxyRequestLog = serde_json::from_value(json_no_upstream).unwrap();
        assert_eq!(log_default.upstream_model, None);
    }
}

impl ProxyRequestLog {
    pub(crate) fn summary(&self) -> Self {
        Self {
            id: self.id.clone(),
            timestamp: self.timestamp,
            method: self.method.clone(),
            url: self.url.clone(),
            status: self.status,
            duration: self.duration,
            model: self.model.clone(),
            mapped_model: self.mapped_model.clone(),
            upstream_model: self.upstream_model.clone(),
            account_email: self.account_email.clone(),
            client_ip: self.client_ip.clone(),
            error: self
                .error
                .as_ref()
                .map(|error| error.chars().take(1024).collect()),
            request_body: None,
            upstream_request_body: None,
            response_body: None,
            request_headers: None,
            upstream_request_headers: None,
            response_headers: self.response_headers.clone(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_tokens: self.cached_tokens,
            total_tokens: self.total_tokens,
            physical_requests: self.physical_requests,
            reasoning_tokens: self.reasoning_tokens,
            protocol: self.protocol.clone(),
            username: self.username.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

#[derive(Default)]
pub(crate) struct UpstreamCapture {
    body: Option<String>,
    headers: Option<String>,
    physical_requests: Option<u32>,
    diagnostics: Option<(crate::proxy::config::DebugLoggingConfig, String)>,
    usage: Option<serde_json::Map<String, serde_json::Value>>,
    attempts: Option<Vec<crate::proxy::upstream::client::FallbackAttemptLog>>,
}

#[derive(Clone, Default)]
pub struct UpstreamRequestBodyHolder(pub std::sync::Arc<std::sync::Mutex<UpstreamCapture>>);

tokio::task_local! {
    pub static CURRENT_UPSTREAM_CAPTURE: UpstreamRequestBodyHolder;
}

/// Keep request limits and observability attached while a lazy response body is polled.
pub(crate) async fn scope_upstream_work<F: std::future::Future>(
    capture: UpstreamRequestBodyHolder,
    budget: Option<std::sync::Arc<crate::proxy::pipeline::budget::RequestContext>>,
    work: F,
) -> F::Output {
    let work = CURRENT_UPSTREAM_CAPTURE.scope(capture, work);
    match budget {
        Some(budget) => crate::proxy::pipeline::budget::scope(budget, work).await,
        None => work.await,
    }
}

impl UpstreamRequestBodyHolder {
    pub(crate) fn configure_diagnostics(
        &self,
        cfg: crate::proxy::config::DebugLoggingConfig,
        id: String,
    ) {
        if cfg.metadata_only {
            if let Ok(mut capture) = self.0.lock() {
                capture.diagnostics = Some((cfg, id));
            }
        }
    }
    pub(crate) fn diagnostic_context(
        &self,
    ) -> Option<crate::proxy::debug_logger::UpstreamDiagnosticContext> {
        let capture = self.0.lock().ok()?;
        let (cfg, request_id) = capture.diagnostics.as_ref()?;
        Some(crate::proxy::debug_logger::UpstreamDiagnosticContext {
            cfg: cfg.clone(),
            request_id: request_id.clone(),
            physical_request: capture.physical_requests?,
        })
    }
    pub fn record_dispatch(&self) {
        if let Ok(mut capture) = self.0.lock() {
            capture.physical_requests = Some(capture.physical_requests.unwrap_or(0) + 1);
            // 同一 holder 只保留当前物理响应，不能拼接不同响应的部分用量。
            capture.usage = Some(serde_json::Map::new());
        }
    }

    pub fn physical_requests(&self) -> Option<u32> {
        self.0
            .lock()
            .ok()
            .and_then(|capture| capture.physical_requests)
    }

    pub fn record_usage(&self, usage: &serde_json::Value) {
        if let Some(reported) = usage.as_object() {
            if let Ok(mut capture) = self.0.lock() {
                let current = capture.usage.get_or_insert_with(serde_json::Map::new);
                for (field, value) in reported {
                    if !value.is_null() || !current.contains_key(field) {
                        current.insert(field.clone(), value.clone());
                    }
                }
            }
        }
    }

    pub fn usage(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.0.lock().ok().and_then(|capture| capture.usage.clone())
    }

    pub fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            UpstreamCapture::default(),
        )))
    }

    pub fn set(&self, body: String) {
        if let Ok(mut lock) = self.0.lock() {
            lock.body = Some(body);
        }
    }

    pub fn set_headers_json(&self, headers: String) {
        if let Ok(mut lock) = self.0.lock() {
            lock.headers = Some(headers);
        }
    }

    pub fn set_value(&self, val: &serde_json::Value) {
        let clean = sanitize_upstream_debug_value(val);
        if let Ok(s) = serde_json::to_string(&clean) {
            self.set(s);
        }
    }

    pub fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut g| g.body.take())
    }

    pub fn take_headers(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut g| g.headers.take())
    }

    pub fn set_attempts(&self, attempts: Vec<crate::proxy::upstream::client::FallbackAttemptLog>) {
        if let Ok(mut lock) = self.0.lock() {
            lock.attempts = Some(attempts);
        }
    }

    pub fn take_attempts(&self) -> Option<Vec<crate::proxy::upstream::client::FallbackAttemptLog>> {
        self.0.lock().ok().and_then(|mut g| g.attempts.take())
    }
}

pub(crate) fn record_upstream_usage(raw: &serde_json::Value) {
    let raw = raw.get("response").unwrap_or(raw);
    let usage = raw.get("usageMetadata").unwrap_or(raw);
    let _ = CURRENT_UPSTREAM_CAPTURE.try_with(|capture| capture.record_usage(usage));
}

fn sanitize_upstream_debug_value(val: &serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::String(s)
            if s.starts_with("data:image/") || s.starts_with("data:audio/") =>
        {
            serde_json::Value::String(format!("[inline data omitted: {} chars]", s.len()))
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sanitize_upstream_debug_value).collect())
        }
        serde_json::Value::Object(map) => {
            let is_inline = map
                .get("mimeType")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && map
                    .get("data")
                    .and_then(serde_json::Value::as_str)
                    .is_some();
            serde_json::Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        let v = if is_inline && k == "data" {
                            serde_json::Value::String(format!(
                                "[inline data omitted: {} chars]",
                                v.as_str().map(str::len).unwrap_or_default()
                            ))
                        } else {
                            sanitize_upstream_debug_value(v)
                        };
                        (k.clone(), v)
                    })
                    .collect(),
            )
        }
        _ => val.clone(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStats {
    pub total_requests: u64,
    pub success_count: u64,
    pub error_count: u64,
}

pub struct ProxyMonitor {
    pub logs: RwLock<VecDeque<ProxyRequestLog>>,
    pub stats: RwLock<ProxyStats>,
    pub max_logs: usize,
    pub enabled: Arc<AtomicBool>,
    pub capture_health_logs: Arc<AtomicBool>,
    app_handle: Option<tauri::AppHandle>,
}

impl ProxyMonitor {
    pub fn new(max_logs: usize, app_handle: Option<tauri::AppHandle>) -> Self {
        // Initialize DB
        if let Err(e) = crate::modules::proxy_db::init_db() {
            tracing::error!("Failed to initialize proxy DB: {}", e);
        }

        let thinking_days = crate::proxy::config::get_thinking_retention_days() as i64;
        let retention = crate::modules::config::load_app_config()
            .map(|config| config.proxy.log_retention)
            .unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            match crate::modules::proxy_db::apply_retention(&retention) {
                Ok((cleared, deleted)) => {
                    if cleared > 0 || deleted > 0 {
                        tracing::info!(
                            "Proxy log retention: cleared {} bodies, deleted {} rows",
                            cleared,
                            deleted
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup old logs: {}", e);
                }
            }
            match crate::modules::proxy_db::cleanup_old_thinking_records(thinking_days) {
                Ok(deleted) => {
                    if deleted > 0 {
                        tracing::info!(
                            "Auto cleanup: removed {} old thinking/signature records (>{} days)",
                            deleted,
                            thinking_days
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup thinking records: {}", e);
                }
            }
            crate::modules::logger::sync_internal_error_log_budget_from_config();
            if let Err(e) = crate::modules::logger::apply_internal_error_log_retention() {
                tracing::error!("Failed to apply internal error log retention: {}", e);
            }
        });

        tokio::spawn(async {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            interval.tick().await;
            loop {
                interval.tick().await;
                let thinking_days = crate::proxy::config::get_thinking_retention_days() as i64;
                let retention = crate::modules::config::load_app_config()
                    .map(|config| config.proxy.log_retention)
                    .unwrap_or_default();
                let result = tokio::task::spawn_blocking(move || {
                    let retention_res = crate::modules::proxy_db::apply_retention(&retention);
                    let thinking_res =
                        crate::modules::proxy_db::cleanup_old_thinking_records(thinking_days);
                    crate::modules::logger::sync_internal_error_log_budget_from_config();
                    let error_log_res =
                        crate::modules::logger::apply_internal_error_log_retention();
                    (retention_res, thinking_res, error_log_res)
                })
                .await;
                match result {
                    Ok((retention_res, thinking_res, error_log_res)) => {
                        match retention_res {
                            Ok((cleared, deleted)) => {
                                if cleared > 0 || deleted > 0 {
                                    tracing::info!(
                                        "Proxy log retention: cleared {} bodies, deleted {} rows",
                                        cleared,
                                        deleted
                                    );
                                }
                            }
                            Err(error) => {
                                tracing::error!("Failed to apply proxy log retention: {}", error)
                            }
                        }
                        if let Ok(deleted) = thinking_res {
                            if deleted > 0 {
                                tracing::info!(
                                    "Auto cleanup: removed {} old thinking/signature records",
                                    deleted
                                );
                            }
                        } else if let Err(e) = thinking_res {
                            tracing::error!("Failed to cleanup thinking records: {}", e);
                        }
                        if let Err(e) = error_log_res {
                            tracing::error!("Failed to apply internal error log retention: {}", e);
                        }
                    }
                    Err(error) => tracing::error!("Proxy log retention task failed: {}", error),
                }
            }
        });

        Self {
            logs: RwLock::new(VecDeque::with_capacity(max_logs)),
            stats: RwLock::new(ProxyStats::default()),
            max_logs,
            enabled: Arc::new(AtomicBool::new(false)), // Default to disabled
            capture_health_logs: Arc::new(AtomicBool::new(false)), // Default to false
            app_handle,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_capture_health_logs(&self, enabled: bool) {
        self.capture_health_logs.store(enabled, Ordering::Relaxed);
    }

    pub fn is_capture_health_logs(&self) -> bool {
        self.capture_health_logs.load(Ordering::Relaxed)
    }

    pub async fn log_request(&self, log: ProxyRequestLog) {
        if let Some(account) = &log.account_email {
            let model = log.model.clone().unwrap_or_else(|| "unknown".to_string());
            let account = account.clone();
            let cached = log.cached_tokens;
            let input = log.input_tokens;
            let output = log.output_tokens;
            let reasoning = log.reasoning_tokens;
            let total = log.total_tokens;
            tokio::task::spawn_blocking(move || {
                if let Err(e) = crate::modules::token_stats::record_usage(
                    &account, &model, input, output, cached, reasoning, total,
                ) {
                    tracing::debug!("Failed to record token stats: {}", e);
                }
            });
        }

        if !self.is_enabled() {
            return;
        }
        tracing::info!("[Monitor] Logging request: {} {}", log.method, log.url);
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.total_requests += 1;
            if log.status >= 200 && log.status < 400 {
                stats.success_count += 1;
            } else {
                stats.error_count += 1;
            }
        }

        let summary = log.summary();
        // Add only the same summary used by the frontend to memory.
        {
            let mut logs = self.logs.write().await;
            if logs.len() >= self.max_logs {
                logs.pop_back();
            }
            logs.push_front(summary.clone());
        }

        if let Some(app) = &self.app_handle {
            let _ = app.emit("proxy://request", &summary);
        }

        let Ok(permit) = LOG_WRITERS.try_acquire() else {
            // 日志写盘繁忙时会被丢弃：这直接解释了“后台查不到这次请求”。
            // 用计数限流上报，避免日志风暴，但保证这种“静默丢日志”可见。
            let dropped = LOG_DROPS.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped % 100 == 1 {
                // ERROR 级：丢弃请求日志 = 事后无法回溯，必须进 error.log
                tracing::error!(
                    "[Monitor] 日志落库繁忙，已丢弃 {} 条请求日志（不会出现在 proxy_logs.db）",
                    dropped
                );
            }
            return;
        };
        // Save to DB (respect server-side simple/full storage mode)
        let mut log_to_save = log;
        let storage_mode = crate::proxy::config::get_payload_storage_mode();
        log_to_save.request_body = crate::proxy::payload_audit::apply_storage_mode_to_body(
            log_to_save.request_body,
            &storage_mode,
        );
        log_to_save.upstream_request_body = crate::proxy::payload_audit::apply_storage_mode_to_body(
            log_to_save.upstream_request_body,
            &storage_mode,
        );
        log_to_save.response_body = crate::proxy::payload_audit::apply_storage_mode_to_body(
            log_to_save.response_body,
            &storage_mode,
        );

        let enabled = Arc::clone(&self.enabled);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if !enabled.load(Ordering::Relaxed) {
                return;
            }
            if let Err(e) = crate::modules::proxy_db::save_log(log_to_save) {
                tracing::error!("Failed to save proxy log to DB: {}", e);
            }

            // Sync to Security DB (IpAccessLogs) so it appears in Security Monitor
            if let Some(ip) = &summary.client_ip {
                let security_log = crate::modules::security_db::IpAccessLog {
                    id: uuid::Uuid::new_v4().to_string(),
                    client_ip: ip.clone(),
                    timestamp: summary.timestamp / 1000, // ms to s
                    method: Some(summary.method.clone()),
                    path: Some(summary.url.clone()),
                    user_agent: None, // We don't have UA in ProxyRequestLog easily accessible here without plumbing
                    status: Some(summary.status as i32),
                    duration: Some(summary.duration as i64),
                    api_key_hash: None,
                    blocked: false, // This comes from monitor, so it wasn't blocked by IP filter
                    block_reason: None,
                    username: summary.username.clone(),
                };

                if let Err(e) = crate::modules::security_db::save_ip_access_log(&security_log) {
                    tracing::error!("Failed to save security log: {}", e);
                }
            }
        });
    }

    pub async fn get_logs(&self, limit: usize) -> Vec<ProxyRequestLog> {
        // Try to get from DB first for true history
        let db_result =
            tokio::task::spawn_blocking(move || crate::modules::proxy_db::get_logs(limit)).await;

        match db_result {
            Ok(Ok(logs)) => logs,
            Ok(Err(e)) => {
                tracing::error!("Failed to get logs from DB: {}", e);
                // Fallback to memory
                let logs = self.logs.read().await;
                logs.iter().take(limit).cloned().collect()
            }
            Err(e) => {
                tracing::error!("Spawn blocking failed for get_logs: {}", e);
                let logs = self.logs.read().await;
                logs.iter().take(limit).cloned().collect()
            }
        }
    }

    pub async fn get_stats(&self) -> ProxyStats {
        let db_result = tokio::task::spawn_blocking(|| crate::modules::proxy_db::get_stats()).await;

        match db_result {
            Ok(Ok(stats)) => stats,
            Ok(Err(e)) => {
                tracing::error!("Failed to get stats from DB: {}", e);
                self.stats.read().await.clone()
            }
            Err(e) => {
                tracing::error!("Spawn blocking failed for get_stats: {}", e);
                self.stats.read().await.clone()
            }
        }
    }

    pub async fn get_logs_filtered(
        &self,
        page: usize,
        page_size: usize,
        search_text: Option<String>,
        level: Option<String>,
    ) -> Result<Vec<ProxyRequestLog>, String> {
        let offset = (page.max(1) - 1) * page_size;
        let errors_only = level.as_deref() == Some("error");
        let search = search_text.unwrap_or_default();

        let res = tokio::task::spawn_blocking(move || {
            crate::modules::proxy_db::get_logs_filtered(&search, errors_only, page_size, offset)
        })
        .await;

        match res {
            Ok(r) => r,
            Err(e) => Err(format!("Spawn blocking failed: {}", e)),
        }
    }

    pub async fn clear(&self) {
        let mut logs = self.logs.write().await;
        logs.clear();
        let mut stats = self.stats.write().await;
        *stats = ProxyStats::default();

        let _ = tokio::task::spawn_blocking(|| {
            if let Err(e) = crate::modules::proxy_db::clear_logs() {
                tracing::error!("Failed to clear logs in DB: {}", e);
            }
        })
        .await;
    }
}

#[cfg(test)]
mod lazy_request_context_tests {
    use super::*;
    use crate::proxy::pipeline::budget;

    #[tokio::test]
    async fn lazy_body_keeps_original_deadline_replay_limit_and_dispatch_capture() {
        let deadline = budget::deadline_now();
        let context = budget::RequestContext::new(deadline);
        let capture = UpstreamRequestBodyHolder::new();
        let (body, captured_context) =
            scope_upstream_work(capture.clone(), Some(context), async move {
                assert_eq!(
                    budget::register_upstream_call(budget::REPLAY_GUARD_BODY_BYTES),
                    Ok(())
                );
                CURRENT_UPSTREAM_CAPTURE.with(|capture| capture.record_dispatch());
                let context = budget::current();
                let body = axum::body::Body::from_stream(futures::stream::once(async move {
                    assert_eq!(budget::current_deadline(), Some(deadline));
                    assert_eq!(
                        budget::register_upstream_call(budget::REPLAY_GUARD_BODY_BYTES),
                        Ok(())
                    );
                    CURRENT_UPSTREAM_CAPTURE.with(|capture| capture.record_dispatch());
                    assert_eq!(
                        budget::register_upstream_call(budget::REPLAY_GUARD_BODY_BYTES),
                        Err(3)
                    );
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: done\n\n"))
                }));
                (body, context)
            })
            .await;
        assert!(budget::current().is_none());
        let polled = tokio::spawn(scope_upstream_work(
            capture.clone(),
            captured_context,
            axum::body::to_bytes(body, 1024),
        ))
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&polled[..], b"data: done\n\n");
        assert_eq!(capture.physical_requests(), Some(2));
        // The next WebSocket turn starts with a fresh request context and capture.
        let next_capture = UpstreamRequestBodyHolder::new();
        scope_upstream_work(
            next_capture.clone(),
            Some(budget::RequestContext::new(budget::deadline_now())),
            async {
                assert_eq!(
                    budget::register_upstream_call(budget::REPLAY_GUARD_BODY_BYTES),
                    Ok(())
                );
                CURRENT_UPSTREAM_CAPTURE.with(|capture| capture.record_dispatch());
            },
        )
        .await;
        assert_eq!(next_capture.physical_requests(), Some(1));
        assert_eq!(capture.physical_requests(), Some(2));
    }
}
