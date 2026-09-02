use crate::proxy::monitor::{ProxyMonitor, ProxyRequestLog, ProxyStats};
use crate::proxy::{ProxyConfig, ProxyPoolConfig, TokenManager};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;
use tokio::time::Duration;

/// Reverse proxy service status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub active_accounts: usize,
}

/// Reverse proxy service global state
#[derive(Clone)]
pub struct ProxyServiceState {
    pub instance: Arc<RwLock<Option<ProxyServiceInstance>>>,
    pub monitor: Arc<RwLock<Option<Arc<ProxyMonitor>>>>,
    pub admin_server: Arc<RwLock<Option<AdminServerInstance>>>, // [NEW] Persistent admin server
    pub starting: Arc<AtomicBool>, // [NEW] Flags whether currently starting, to prevent deadlocks
}

pub struct AdminServerInstance {
    pub axum_server: crate::proxy::AxumServer,
    #[allow(dead_code)] // Keep the handle to support explicit shutdown/diagnostics in the future
    pub server_handle: tokio::task::JoinHandle<()>,
}

/// Reverse proxy service instance
pub struct ProxyServiceInstance {
    pub config: ProxyConfig,
    pub token_manager: Arc<TokenManager>,
    pub axum_server: crate::proxy::AxumServer,
}

impl ProxyServiceState {
    pub fn new() -> Self {
        Self {
            instance: Arc::new(RwLock::new(None)),
            monitor: Arc::new(RwLock::new(None)),
            admin_server: Arc::new(RwLock::new(None)),
            starting: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Start the reverse proxy service (Tauri command)
#[tauri::command]
pub async fn start_proxy_service(
    config: ProxyConfig,
    state: State<'_, ProxyServiceState>,
    cf_state: State<'_, crate::commands::cloudflared::CloudflaredState>,
    app_handle: tauri::AppHandle,
) -> Result<ProxyStatus, String> {
    internal_start_proxy_service(
        config,
        &state,
        crate::modules::integration::SystemManager::Desktop(app_handle),
        Arc::new(cf_state.inner().clone()),
    )
    .await
}

struct StartingGuard(Arc<AtomicBool>);
impl Drop for StartingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Internal reverse proxy service startup logic (decoupled version)
pub async fn internal_start_proxy_service(
    config: ProxyConfig,
    state: &ProxyServiceState,
    integration: crate::modules::integration::SystemManager,
    cloudflared_state: Arc<crate::commands::cloudflared::CloudflaredState>,
) -> Result<ProxyStatus, String> {
    // 1. Check state and acquire the lock
    {
        let instance_lock = state.instance.read().await;
        if instance_lock.is_some() {
            return Err("Service is already running".to_string());
        }
    }

    // 2. Check whether it's already starting (prevent deadlocks & concurrent starts)
    if state
        .starting
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Service is starting, please wait...".to_string());
    }

    // Use a custom Drop guard to ensure the starting state is reset regardless of success or failure
    let _starting_guard = StartingGuard(state.starting.clone());

    // Ensure monitor exists
    {
        let mut monitor_lock = state.monitor.write().await;
        if monitor_lock.is_none() {
            let app_handle =
                if let crate::modules::integration::SystemManager::Desktop(ref h) = integration {
                    Some(h.clone())
                } else {
                    None
                };
            *monitor_lock = Some(Arc::new(ProxyMonitor::new(1000, app_handle)));
        }
        // Sync enabled state from config
        if let Some(monitor) = monitor_lock.as_ref() {
            monitor.set_enabled(config.enable_logging);
        }
    }

    let _monitor = state.monitor.read().await.as_ref().unwrap().clone();

    // Check and start the admin server (if not already running)
    ensure_admin_server(
        config.clone(),
        state,
        integration.clone(),
        cloudflared_state.clone(),
    )
    .await?;

    // 2. [FIX] Reuse the admin server's Token manager (single instance, resolves hot-reload sync issues)
    let token_manager = {
        let admin_lock = state.admin_server.read().await;
        admin_lock
            .as_ref()
            .unwrap()
            .axum_server
            .token_manager
            .clone()
    };

    // Sync the config to the running TokenManager
    token_manager.start_auto_cleanup().await;
    token_manager
        .update_sticky_config(config.scheduling.clone())
        .await;

    // [NEW] Load the circuit breaker config (loaded from the main config)
    let app_config = crate::modules::config::load_app_config()
        .unwrap_or_else(|_| crate::models::AppConfig::new());
    token_manager
        .update_circuit_breaker_config(app_config.circuit_breaker)
        .await;

    // 🆕 [FIX #820] Restore the fixed account mode setting
    if let Some(ref account_id) = config.preferred_account_id {
        token_manager
            .set_preferred_account(Some(account_id.clone()))
            .await;
        tracing::info!("🔒 [FIX #820] Fixed account mode restored: {}", account_id);
    }

    // 3. Load accounts
    let active_accounts = token_manager.load_accounts().await.unwrap_or(0);

    if active_accounts == 0 {
        let zai_enabled = config.zai.enabled
            && !matches!(config.zai.dispatch_mode, crate::proxy::ZaiDispatchMode::Off);
        if !zai_enabled {
            tracing::warn!("No accounts available, reverse proxy logic will pause; add one via the admin interface.");
            return Ok(ProxyStatus {
                running: false,
                port: config.port,
                base_url: format!("http://127.0.0.1:{}", config.port),
                active_accounts: 0,
            });
        }
    }

    let mut instance_lock = state.instance.write().await;
    let admin_lock = state.admin_server.read().await;
    let axum_server = admin_lock
        .as_ref()
        .expect("admin server must exist after ensure_admin_server")
        .axum_server
        .clone();

    // Create the service instance (logical startup). No longer keeping a fake server_handle:
    // The real handle for the listening task is now held by AdminServerInstance (see ensure_admin_server).
    let instance = ProxyServiceInstance {
        config: config.clone(),
        token_manager: token_manager.clone(),
        axum_server: axum_server.clone(),
    };

    // [FIX] Ensure the server is logically running
    axum_server.set_running(true).await;

    *instance_lock = Some(instance);

    // After a successful start, it's OK for the guard to end here and reset starting
    // But we could also just drop it manually, or trust the guard
    Ok(ProxyStatus {
        running: true,
        port: config.port,
        base_url: format!("http://127.0.0.1:{}", config.port),
        active_accounts,
    })
}

/// Ensure the admin server is running
pub async fn ensure_admin_server(
    config: ProxyConfig,
    state: &ProxyServiceState,
    integration: crate::modules::integration::SystemManager,
    cloudflared_state: Arc<crate::commands::cloudflared::CloudflaredState>,
) -> Result<(), String> {
    let mut admin_lock = state.admin_server.write().await;
    if admin_lock.is_some() {
        return Ok(());
    }

    // Ensure monitor exists
    let monitor = {
        let mut monitor_lock = state.monitor.write().await;
        if monitor_lock.is_none() {
            let app_handle =
                if let crate::modules::integration::SystemManager::Desktop(ref h) = integration {
                    Some(h.clone())
                } else {
                    None
                };
            *monitor_lock = Some(Arc::new(ProxyMonitor::new(1000, app_handle)));
        }
        monitor_lock.as_ref().unwrap().clone()
    };

    // Default empty TokenManager used for the admin interface
    let app_data_dir = crate::modules::account::get_data_dir()?;
    let token_manager = Arc::new(TokenManager::new(app_data_dir));
    // [NEW] Load account data, otherwise the admin interface stats show 0
    let _ = token_manager.load_accounts().await;

    let (axum_server, server_handle) = match crate::proxy::AxumServer::start(
        config.get_bind_address().to_string(),
        config.port,
        token_manager,
        config.custom_mapping.clone(),
        config.request_timeout,
        config.upstream_proxy.clone(),
        config.user_agent_override.clone(),
        crate::proxy::ProxySecurityConfig::from_proxy_config(&config),
        config.zai.clone(),
        monitor,
        config.experimental.clone(),
        config.debug_logging.clone(),
        integration.clone(),
        cloudflared_state,
        config.proxy_pool.clone(),
        config.only_raw_quota_models,
        config.image_scheduler.clone(),
    )
    .await
    {
        Ok((server, handle)) => (server, handle),
        Err(e) => return Err(format!("Failed to start admin server: {}", e)),
    };

    *admin_lock = Some(AdminServerInstance {
        axum_server,
        server_handle,
    });

    // [NEW] Initialize the global Thinking Budget config
    crate::proxy::update_thinking_budget_config(config.thinking_budget.clone());
    // [NEW] Initialize the global system prompt config
    crate::proxy::update_global_system_prompt_config(config.global_system_prompt.clone());
    // [NEW] Initialize the global image thinking mode config
    crate::proxy::update_image_thinking_mode(config.image_thinking_mode.clone());
    // [NEW] Initialize the global compression level config
    crate::proxy::config::update_global_compression_level(
        config.experimental.compression_level.clone(),
        config.experimental.enable_usage_scaling,
    );

    Ok(())
}

/// Stop the reverse proxy service
#[tauri::command]
pub async fn stop_proxy_service(state: State<'_, ProxyServiceState>) -> Result<(), String> {
    let mut instance_lock = state.instance.write().await;

    if instance_lock.is_none() {
        return Err("Service is not running".to_string());
    }

    // Stop the Axum server (logical stop only, does not kill the process)
    if let Some(instance) = instance_lock.take() {
        instance.token_manager.abort_background_tasks().await;
        instance.axum_server.set_running(false).await;
        // The instance.axum_server.stop() call has been removed, to avoid killing the Admin Server
    }

    Ok(())
}

/// Get the reverse proxy service status
#[tauri::command]
pub async fn get_proxy_status(state: State<'_, ProxyServiceState>) -> Result<ProxyStatus, String> {
    // Check the starting flag first, to avoid being blocked by the write lock
    if state.starting.load(Ordering::SeqCst) {
        return Ok(ProxyStatus {
            running: false, // Not logically running yet
            port: 0,
            base_url: "starting".to_string(), // Marker for the frontend
            active_accounts: 0,
        });
    }

    // Use try_read to avoid causing queuing delays in this command
    let lock_res = state.instance.try_read();

    match lock_res {
        Ok(instance_lock) => match instance_lock.as_ref() {
            Some(instance) => Ok(ProxyStatus {
                running: true,
                port: instance.config.port,
                base_url: format!("http://127.0.0.1:{}", instance.config.port),
                active_accounts: instance.token_manager.len(),
            }),
            None => Ok(ProxyStatus {
                running: false,
                port: 0,
                base_url: String::new(),
                active_accounts: 0,
            }),
        },
        Err(_) => {
            // If the lock can't be acquired, a write operation is in progress (possibly starting or stopping)
            Ok(ProxyStatus {
                running: false,
                port: 0,
                base_url: "busy".to_string(),
                active_accounts: 0,
            })
        }
    }
}

/// Get reverse proxy service statistics
#[tauri::command]
pub async fn get_proxy_stats(state: State<'_, ProxyServiceState>) -> Result<ProxyStats, String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        Ok(monitor.get_stats().await)
    } else {
        Ok(ProxyStats::default())
    }
}

/// Get reverse proxy request logs
#[tauri::command]
pub async fn get_proxy_logs(
    state: State<'_, ProxyServiceState>,
    limit: Option<usize>,
) -> Result<Vec<ProxyRequestLog>, String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        Ok(monitor.get_logs(limit.unwrap_or(100)).await)
    } else {
        Ok(Vec::new())
    }
}

/// Set the monitoring enabled state
#[tauri::command]
pub async fn set_proxy_monitor_enabled(
    state: State<'_, ProxyServiceState>,
    enabled: bool,
) -> Result<(), String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        monitor.set_enabled(enabled);
    }
    Ok(())
}

/// Clear reverse proxy request logs
#[tauri::command]
pub async fn clear_proxy_logs(state: State<'_, ProxyServiceState>) -> Result<(), String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        monitor.clear().await;
    }
    Ok(())
}

/// Get reverse proxy request logs (paginated)
#[tauri::command]
pub async fn get_proxy_logs_paginated(
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<ProxyRequestLog>, String> {
    crate::modules::proxy_db::get_logs_summary(limit.unwrap_or(20), offset.unwrap_or(0))
}

/// Get the full detail of a single log entry
#[tauri::command]
pub async fn get_proxy_log_detail(log_id: String) -> Result<ProxyRequestLog, String> {
    crate::modules::proxy_db::get_log_detail(&log_id)
}

/// Get the total log count
#[tauri::command]
pub async fn get_proxy_logs_count() -> Result<u64, String> {
    crate::modules::proxy_db::get_logs_count()
}

/// Export all logs to the specified file
#[tauri::command]
pub async fn export_proxy_logs(file_path: String) -> Result<usize, String> {
    let validated_path = super::validate_user_json_path(&file_path, false)?;

    let logs = crate::modules::proxy_db::get_all_logs_for_export()?;
    let count = logs.len();

    let json = serde_json::to_string_pretty(&logs)
        .map_err(|e| format!("Failed to serialize logs: {}", e))?;

    std::fs::write(&validated_path, json).map_err(|e| format!("Failed to write file: {}", e))?;

    Ok(count)
}

/// Export the specified logs JSON to a file
#[tauri::command]
pub async fn export_proxy_logs_json(file_path: String, json_data: String) -> Result<usize, String> {
    let validated_path = super::validate_user_json_path(&file_path, false)?;

    // Parse to count items
    let logs: Vec<serde_json::Value> =
        serde_json::from_str(&json_data).map_err(|e| format!("Failed to parse JSON: {}", e))?;
    let count = logs.len();

    // Pretty print
    let pretty_json =
        serde_json::to_string_pretty(&logs).map_err(|e| format!("Failed to serialize: {}", e))?;

    std::fs::write(&validated_path, pretty_json)
        .map_err(|e| format!("Failed to write file: {}", e))?;

    Ok(count)
}

/// Get the log count with search filters applied
#[tauri::command]
pub async fn get_proxy_logs_count_filtered(
    filter: String,
    errors_only: bool,
) -> Result<u64, String> {
    crate::modules::proxy_db::get_logs_count_filtered(&filter, errors_only)
}

/// Get paginated logs with search filters applied
#[tauri::command]
pub async fn get_proxy_logs_filtered(
    filter: String,
    errors_only: bool,
    limit: usize,
    offset: usize,
) -> Result<Vec<crate::proxy::monitor::ProxyRequestLog>, String> {
    crate::modules::proxy_db::get_logs_filtered(&filter, errors_only, limit, offset)
}

/// Generate an API Key
#[tauri::command]
pub fn generate_api_key() -> String {
    format!("sk-{}", uuid::Uuid::new_v4().simple())
}

/// Reload accounts (called when the main app adds/removes accounts)
#[tauri::command]
pub async fn reload_proxy_accounts(state: State<'_, ProxyServiceState>) -> Result<usize, String> {
    let instance_lock = state.instance.read().await;

    if let Some(instance) = instance_lock.as_ref() {
        // [FIX #820] Clear stale session bindings before reloading accounts
        // This ensures that after switching accounts in the UI, API requests
        // won't be routed to the previously bound (wrong) account
        instance.token_manager.clear_all_sessions();

        // Reload accounts
        let count = instance
            .token_manager
            .load_accounts()
            .await
            .map_err(|e| format!("Failed to reload accounts: {}", e))?;
        Ok(count)
    } else {
        Err("Service is not running".to_string())
    }
}

/// Update the model mapping table (hot reload)
#[tauri::command]
pub async fn update_model_mapping(
    config: ProxyConfig,
    state: State<'_, ProxyServiceState>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;

    // 1. If the service is running, immediately update the in-memory mapping (currently this only updates the anthropic_mapping RwLock,
    // in the future resolve_model_route could be made to read the full config directly if needed)
    if let Some(instance) = instance_lock.as_ref() {
        instance.axum_server.update_mapping(&config).await;
        tracing::debug!("Backend service has received the full model mapping config");
    }

    // 2. Regardless of whether it's running, persist it to the global config
    let mut app_config = crate::modules::config::load_app_config().map_err(|e| e)?;
    app_config.proxy.custom_mapping = config.custom_mapping;
    crate::modules::config::save_app_config(&app_config).map_err(|e| e)?;

    Ok(())
}

fn join_base_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };
    format!("{}{}", base, path)
}

fn extract_model_ids(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();

    fn push_from_item(out: &mut Vec<String>, item: &serde_json::Value) {
        match item {
            serde_json::Value::String(s) => out.push(s.to_string()),
            serde_json::Value::Object(map) => {
                if let Some(id) = map.get("id").and_then(|v| v.as_str()) {
                    out.push(id.to_string());
                } else if let Some(name) = map.get("name").and_then(|v| v.as_str()) {
                    out.push(name.to_string());
                }
            }
            _ => {}
        }
    }

    match value {
        serde_json::Value::Array(arr) => {
            for item in arr {
                push_from_item(&mut out, item);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(data) = map.get("data") {
                if let serde_json::Value::Array(arr) = data {
                    for item in arr {
                        push_from_item(&mut out, item);
                    }
                }
            }
            if let Some(models) = map.get("models") {
                match models {
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            push_from_item(&mut out, item);
                        }
                    }
                    other => push_from_item(&mut out, other),
                }
            }
        }
        _ => {}
    }

    out
}

/// Fetch available models from the configured z.ai Anthropic-compatible API (`/v1/models`).
#[tauri::command]
pub async fn fetch_zai_models(
    zai: crate::proxy::ZaiConfig,
    upstream_proxy: crate::proxy::config::UpstreamProxyConfig,
    request_timeout: u64,
) -> Result<Vec<String>, String> {
    if zai.base_url.trim().is_empty() {
        return Err("z.ai base_url is empty".to_string());
    }
    if zai.api_key.trim().is_empty() {
        return Err("z.ai api_key is not set".to_string());
    }

    let url = join_base_url(&zai.base_url, "/v1/models");

    let mut builder =
        reqwest::Client::builder().timeout(Duration::from_secs(request_timeout.max(5)));
    if upstream_proxy.enabled && !upstream_proxy.url.is_empty() {
        let proxy = reqwest::Proxy::all(&upstream_proxy.url)
            .map_err(|e| format!("Invalid upstream proxy url: {}", e))?;
        builder = builder.proxy(proxy);
    }
    let client = builder
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", zai.api_key))
        .header("x-api-key", zai.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Upstream request failed: {}", e))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    if !status.is_success() {
        let preview = if text.len() > 4000 {
            &text[..4000]
        } else {
            &text
        };
        return Err(format!("Upstream returned {}: {}", status, preview));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid JSON response: {}", e))?;
    let mut models = extract_model_ids(&json);
    models.retain(|s| !s.trim().is_empty());
    models.sort();
    models.dedup();
    Ok(models)
}

/// Get the current scheduling configuration
#[tauri::command]
pub async fn get_proxy_scheduling_config(
    state: State<'_, ProxyServiceState>,
) -> Result<crate::proxy::sticky_config::StickySessionConfig, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        Ok(instance.token_manager.get_sticky_config().await)
    } else {
        Ok(crate::proxy::sticky_config::StickySessionConfig::default())
    }
}

/// Update the scheduling configuration
#[tauri::command]
pub async fn update_proxy_scheduling_config(
    state: State<'_, ProxyServiceState>,
    config: crate::proxy::sticky_config::StickySessionConfig,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.token_manager.update_sticky_config(config).await;
        Ok(())
    } else {
        Err("Service is not running, unable to update live configuration".to_string())
    }
}

/// Clear all session sticky bindings
#[tauri::command]
pub async fn clear_proxy_session_bindings(
    state: State<'_, ProxyServiceState>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.token_manager.clear_all_sessions();
        Ok(())
    } else {
        Err("Service is not running".to_string())
    }
}

// ===== [FIX #820] Fixed account mode commands =====

/// Set the preferred account (fixed account mode)
/// Pass account_id to enable fixed mode, pass null/empty string to restore round-robin mode
#[tauri::command]
pub async fn set_preferred_account(
    state: State<'_, ProxyServiceState>,
    account_id: Option<String>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        // Treat an empty string as None
        let cleaned_id = account_id.filter(|s| !s.trim().is_empty());

        // 1. Update in-memory state
        instance
            .token_manager
            .set_preferred_account(cleaned_id.clone())
            .await;

        // 2. Persist to the config file (fixes Issue #820 auto-off problem)
        let mut app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        app_config.proxy.preferred_account_id = cleaned_id.clone();
        crate::modules::config::save_app_config(&app_config)
            .map_err(|e| format!("Failed to save config: {}", e))?;

        if let Some(ref id) = cleaned_id {
            tracing::info!(
                "🔒 [FIX #820] Fixed account mode enabled and persisted: {}",
                id
            );
        } else {
            tracing::info!("🔄 [FIX #820] Round-robin mode enabled and persisted");
        }

        Ok(())
    } else {
        Err("Service is not running".to_string())
    }
}

/// Get the currently preferred account ID
#[tauri::command]
pub async fn get_preferred_account(
    state: State<'_, ProxyServiceState>,
) -> Result<Option<String>, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        Ok(instance.token_manager.get_preferred_account().await)
    } else {
        Ok(None)
    }
}

/// Clear the rate limit record for the specified account
#[tauri::command]
pub async fn clear_proxy_rate_limit(
    state: State<'_, ProxyServiceState>,
    account_id: String,
) -> Result<bool, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        Ok(instance.token_manager.clear_rate_limit(&account_id))
    } else {
        Err("Service is not running".to_string())
    }
}

/// Clear all rate limit records
#[tauri::command]
pub async fn clear_all_proxy_rate_limits(
    state: State<'_, ProxyServiceState>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.token_manager.clear_all_rate_limits();
        Ok(())
    } else {
        Err("Service is not running".to_string())
    }
}

/// Trigger a health check for all proxies, and return the updated config
#[tauri::command]
pub async fn check_proxy_health(
    state: State<'_, ProxyServiceState>,
) -> Result<ProxyPoolConfig, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        let pool_state = instance.axum_server.proxy_pool_state.clone();
        let manager = crate::proxy::proxy_pool::ProxyPoolManager::new(pool_state.clone());

        manager.health_check().await?;

        // Return the updated config from memory
        let config = pool_state.read().await;
        Ok(config.clone())
    } else {
        Err("Service is not running".to_string())
    }
}

/// Get the current in-memory proxy pool state
#[tauri::command]
pub async fn get_proxy_pool_config(
    state: State<'_, ProxyServiceState>,
) -> Result<ProxyPoolConfig, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        let config = instance.axum_server.proxy_pool_state.read().await;
        Ok(config.clone())
    } else {
        Err("Service is not running".to_string())
    }
}
