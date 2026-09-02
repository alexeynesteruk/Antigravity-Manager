use crate::models::{Account, AppConfig, QuotaData};
use crate::modules;
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

// Export proxy commands
pub mod proxy;
// Export autostart commands
pub mod autostart;
// Export cloudflared commands
pub mod cloudflared;
// Export security commands (IP monitoring)
pub mod security;
// Export proxy_pool commands
pub mod proxy_pool;
// Export user_token commands
pub mod user_token;
// Export patch commands
pub mod patch;
pub use patch::*;

/// List all accounts
#[tauri::command]
pub async fn list_accounts(
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
) -> Result<Vec<Account>, String> {
    let mut accounts = tokio::task::spawn_blocking(move || modules::list_accounts())
        .await
        .unwrap_or_else(|_| Err("Task panicked".to_string()))?;

    // [FIX] Blend in-memory TokenManager rate limit status into the UI quota display
    let instance_lock = proxy_state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        for account in &mut accounts {
            if let Some(reset_secs) = instance
                .token_manager
                .get_rate_limit_reset_seconds(&account.id)
            {
                if reset_secs > 0 {
                    if let Some(ref mut quota_data) = account.quota {
                        for model in &mut quota_data.models {
                            model.percentage = 0;
                            model.reset_time =
                                (chrono::Utc::now().timestamp() + reset_secs as i64).to_string();
                        }
                        // Optionally, add a UI flag if we want it to look completely blocked
                        // quota_data.is_forbidden = true;
                        // quota_data.forbidden_reason = Some(format!("Quota exhausted or rate limited (resets in {}s)", reset_secs));
                    }
                }
            }
        }
    }

    Ok(accounts)
}

/// Add an account
#[tauri::command]
pub async fn add_account(
    app: tauri::AppHandle,
    _email: String,
    refresh_token: String,
) -> Result<Account, String> {
    let service = modules::account_service::AccountService::new(
        crate::modules::integration::SystemManager::Desktop(app.clone()),
    );

    let mut account = service.add_account(&refresh_token).await?;

    // Automatically refresh quota
    let _ = internal_refresh_account_quota(&app, &mut account).await;

    // Reload the account pool
    let _ = crate::commands::proxy::reload_proxy_accounts(
        app.state::<crate::commands::proxy::ProxyServiceState>(),
    )
    .await;

    Ok(account)
}

/// Delete an account
/// Delete an account
#[tauri::command]
pub async fn delete_account(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    account_id: String,
) -> Result<(), String> {
    let service = modules::account_service::AccountService::new(
        crate::modules::integration::SystemManager::Desktop(app.clone()),
    );
    service.delete_account(&account_id)?;

    // Reload token pool
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;

    Ok(())
}

/// Batch delete accounts
#[tauri::command]
pub async fn delete_accounts(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    account_ids: Vec<String>,
) -> Result<(), String> {
    modules::logger::log_info(&format!(
        "Received batch delete request, {} account(s) total",
        account_ids.len()
    ));
    modules::account::delete_accounts(&account_ids).map_err(|e| {
        modules::logger::log_error(&format!("Batch delete failed: {}", e));
        e
    })?;

    // Force-sync the tray
    crate::modules::tray::update_tray_menus(&app);

    // Reload token pool
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;

    Ok(())
}

/// Reorder the account list
/// Update the account ordering based on the order of the passed-in account ID array
#[tauri::command]
pub async fn reorder_accounts(
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    account_ids: Vec<String>,
) -> Result<(), String> {
    modules::logger::log_info(&format!(
        "Received account reorder request, {} account(s) total",
        account_ids.len()
    ));
    modules::account::reorder_accounts(&account_ids).map_err(|e| {
        modules::logger::log_error(&format!("Account reorder failed: {}", e));
        e
    })?;

    // Reload pool to reflect new order if running
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;
    Ok(())
}

/// Switch account
#[tauri::command]
pub async fn switch_account(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    account_id: String,
    target_ide: Option<String>,
) -> Result<(), String> {
    let service = modules::account_service::AccountService::new(
        crate::modules::integration::SystemManager::Desktop(app.clone()),
    );

    service
        .switch_account(&account_id, target_ide.as_deref())
        .await?;

    // Sync the tray
    crate::modules::tray::update_tray_menus(&app);

    // [FIX #820] Notify proxy to clear stale session bindings and reload accounts
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;

    Ok(())
}

/// Get the current account
#[tauri::command]
pub async fn get_current_account() -> Result<Option<Account>, String> {
    // println!("🚀 Backend Command: get_current_account called"); // Commented out to reduce noise for frequent calls, relies on frontend log for frequency
    // Actually user WANTS to see it.
    modules::logger::log_info("Backend Command: get_current_account called");

    let account_id = modules::get_current_account_id()?;

    if let Some(id) = account_id {
        // modules::logger::log_info(&format!("   Found current account ID: {}", id));
        modules::load_account(&id).map(Some)
    } else {
        modules::logger::log_info("   No current account set");
        Ok(None)
    }
}

/// Export accounts (includes refresh_token)
use crate::models::AccountExportResponse;

#[tauri::command]
pub async fn export_accounts(account_ids: Vec<String>) -> Result<AccountExportResponse, String> {
    tokio::task::spawn_blocking(move || modules::account::export_accounts_by_ids(&account_ids))
        .await
        .unwrap_or_else(|_| Err("Task panicked".to_string()))
}

/// Internal helper: automatically refresh quota once after adding or importing an account
async fn internal_refresh_account_quota(
    app: &tauri::AppHandle,
    account: &mut Account,
) -> Result<QuotaData, String> {
    modules::logger::log_info(&format!("Automatically triggering a quota refresh: {}", account.email));

    // Use the query with retry (shared logic)
    match modules::account::fetch_quota_with_retry(account).await {
        Ok(quota) => {
            // Update the account quota
            let _ = modules::update_account_quota(&account.id, quota.clone());
            // Update the tray menu
            crate::modules::tray::update_tray_menus(app);
            Ok(quota)
        }
        Err(e) => {
            modules::logger::log_warn(&format!("Automatic quota refresh failed ({}): {}", account.email, e));
            Err(e.to_string())
        }
    }
}

/// Query account quota
#[tauri::command]
pub async fn fetch_account_quota(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    account_id: String,
) -> crate::error::AppResult<QuotaData> {
    modules::logger::log_info(&format!("Manual quota refresh request: {}", account_id));
    let mut account =
        modules::load_account(&account_id).map_err(crate::error::AppError::Account)?;

    // Use the query with retry (shared logic)
    let mut quota = modules::account::fetch_quota_with_retry(&mut account).await?;

    // 4. Update the account quota
    modules::update_account_quota(&account_id, quota.clone())
        .map_err(crate::error::AppError::Account)?;

    crate::modules::tray::update_tray_menus(&app);

    // 5. Sync to the running reverse proxy service (if started)
    let instance_lock = proxy_state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        if quota.models.iter().any(|model| model.percentage > 0) {
            instance.token_manager.clear_rate_limit_memory(&account_id);
        }
        let _ = instance.token_manager.reload_account(&account_id).await;

        // Blend TokenManager lockout state only for models that are still 0%
        if let Some(reset_secs) = instance
            .token_manager
            .get_rate_limit_reset_seconds(&account_id)
        {
            if reset_secs > 0 {
                for model in &mut quota.models {
                    if model.percentage == 0 {
                        model.reset_time =
                            (chrono::Utc::now().timestamp() + reset_secs as i64).to_string();
                    }
                }
            }
        }
    }

    Ok(quota)
}

pub use modules::account::RefreshStats;

/// Refresh quota for all accounts (internal implementation)
pub async fn refresh_all_quotas_internal(
    proxy_state: &crate::commands::proxy::ProxyServiceState,
    app_handle: Option<tauri::AppHandle>,
) -> Result<RefreshStats, String> {
    let stats = modules::account::refresh_all_quotas_logic().await?;

    // Sync to the running reverse proxy service (if started)
    let instance_lock = proxy_state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        let _ = instance.token_manager.reload_all_accounts().await;
    }

    // Send a global refresh event to the UI (if needed)
    if let Some(handle) = app_handle {
        use tauri::Emitter;
        let _ = handle.emit("accounts://refreshed", ());
    }

    Ok(stats)
}

/// Refresh quota for all accounts (Tauri Command)
#[tauri::command]
pub async fn refresh_all_quotas(
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    app_handle: tauri::AppHandle,
) -> Result<RefreshStats, String> {
    refresh_all_quotas_internal(&proxy_state, Some(app_handle)).await
}
/// Get the device fingerprint (current storage.json + account binding)
#[tauri::command]
pub async fn get_device_profiles(
    account_id: String,
) -> Result<modules::account::DeviceProfiles, String> {
    modules::get_device_profiles(&account_id)
}

/// Bind a device fingerprint (capture: collect the current one; generate: generate a new fingerprint), and write it to storage.json
#[tauri::command]
pub async fn bind_device_profile(
    account_id: String,
    mode: String,
) -> Result<crate::models::DeviceProfile, String> {
    modules::bind_device_profile(&account_id, &mode)
}

/// Preview-generate a fingerprint (not persisted to disk)
#[tauri::command]
pub async fn preview_generate_profile() -> Result<crate::models::DeviceProfile, String> {
    Ok(crate::modules::device::generate_profile())
}

/// Bind directly using the given fingerprint
#[tauri::command]
pub async fn bind_device_profile_with_profile(
    account_id: String,
    profile: crate::models::DeviceProfile,
) -> Result<crate::models::DeviceProfile, String> {
    modules::bind_device_profile_with_profile(&account_id, profile, Some("generated".to_string()))
}

/// Apply the account's already-bound fingerprint to storage.json
#[tauri::command]
pub async fn apply_device_profile(
    account_id: String,
) -> Result<crate::models::DeviceProfile, String> {
    modules::apply_device_profile(&account_id)
}

/// Restore the earliest storage.json backup (approximates the "original" state)
#[tauri::command]
pub async fn restore_original_device() -> Result<String, String> {
    modules::restore_original_device()
}

/// List fingerprint versions
#[tauri::command]
pub async fn list_device_versions(
    account_id: String,
) -> Result<modules::account::DeviceProfiles, String> {
    modules::list_device_versions(&account_id)
}

/// Restore a fingerprint by version
#[tauri::command]
pub async fn restore_device_version(
    account_id: String,
    version_id: String,
) -> Result<crate::models::DeviceProfile, String> {
    modules::restore_device_version(&account_id, &version_id)
}

/// Delete a historical fingerprint (the baseline cannot be deleted)
#[tauri::command]
pub async fn delete_device_version(account_id: String, version_id: String) -> Result<(), String> {
    modules::delete_device_version(&account_id, &version_id)
}

/// Open the device storage directory
#[tauri::command]
pub async fn open_device_folder(app: tauri::AppHandle) -> Result<(), String> {
    let dir = modules::device::get_storage_dir()?;
    let dir_str = dir
        .to_str()
        .ok_or("Unable to resolve the storage directory path as a string")?
        .to_string();
    app.opener()
        .open_path(dir_str, None::<&str>)
        .map_err(|e| format!("Failed to open directory: {}", e))
}

/// Load configuration
#[tauri::command]
pub async fn load_config() -> Result<AppConfig, String> {
    modules::load_app_config()
}

/// Save configuration
#[tauri::command]
pub async fn save_config(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    config: AppConfig,
) -> Result<(), String> {
    modules::save_app_config(&config)?;

    // Notify the tray that the configuration has been updated
    let _ = app.emit("config://updated", ());

    // Hot-reload the running service
    let instance_lock = proxy_state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        // Update the model mapping
        instance.axum_server.update_mapping(&config.proxy).await;
        // Update the toggle for exposing only real quota models
        instance
            .axum_server
            .update_only_raw_quota_models(config.proxy.only_raw_quota_models)
            .await;
        // Update the upstream proxy
        instance
            .axum_server
            .update_proxy(config.proxy.upstream_proxy.clone())
            .await;
        // Update the security policy (auth)
        instance.axum_server.update_security(&config.proxy).await;
        // Update the z.ai config
        instance.axum_server.update_zai(&config.proxy).await;
        // Update experimental config
        instance
            .axum_server
            .update_experimental(&config.proxy)
            .await;
        // Update the debug logging config
        instance
            .axum_server
            .update_debug_logging(&config.proxy)
            .await;
        // [NEW] Update the User-Agent config
        instance.axum_server.update_user_agent(&config.proxy).await;
        // Update the Thinking Budget config
        crate::proxy::update_thinking_budget_config(config.proxy.thinking_budget.clone());
        // [NEW] Update the global system prompt config
        crate::proxy::update_global_system_prompt_config(config.proxy.global_system_prompt.clone());
        // [NEW] Update the global image thinking mode config
        crate::proxy::update_image_thinking_mode(config.proxy.image_thinking_mode.clone());
        // [NEW] Update the global compression level config
        crate::proxy::config::update_global_compression_level(
            config.proxy.experimental.compression_level.clone(),
            config.proxy.experimental.enable_usage_scaling,
        );
        crate::proxy::config::update_global_thresholds(
            config.proxy.experimental.context_compression_threshold_l1,
            config.proxy.experimental.context_compression_threshold_l2,
            config.proxy.experimental.context_compression_threshold_l3,
        );
        // Update the proxy pool config
        instance
            .axum_server
            .update_proxy_pool(config.proxy.proxy_pool.clone())
            .await;
        // Update the circuit breaker config
        instance
            .token_manager
            .update_circuit_breaker_config(config.circuit_breaker.clone())
            .await;
        tracing::debug!("Reverse proxy service config hot-reload synced");
    }

    Ok(())
}

// --- OAuth commands ---

#[tauri::command]
pub async fn start_oauth_login(
    app_handle: tauri::AppHandle,
    oauth_client_key: Option<String>,
) -> Result<Account, String> {
    modules::logger::log_info("Starting OAuth authorization flow...");
    let service = modules::account_service::AccountService::new(
        crate::modules::integration::SystemManager::Desktop(app_handle.clone()),
    );

    let mut account = service.start_oauth_login(oauth_client_key).await?;

    // Automatically trigger a quota refresh
    let _ = internal_refresh_account_quota(&app_handle, &mut account).await;

    // Reload token pool
    let _ = crate::commands::proxy::reload_proxy_accounts(
        app_handle.state::<crate::commands::proxy::ProxyServiceState>(),
    )
    .await;

    Ok(account)
}

/// Complete OAuth authorization (does not auto-open a browser)
#[tauri::command]
pub async fn complete_oauth_login(app_handle: tauri::AppHandle) -> Result<Account, String> {
    modules::logger::log_info("Completing OAuth authorization flow (manual)...");
    let service = modules::account_service::AccountService::new(
        crate::modules::integration::SystemManager::Desktop(app_handle.clone()),
    );

    let mut account = service.complete_oauth_login().await?;

    // Automatically trigger a quota refresh
    let _ = internal_refresh_account_quota(&app_handle, &mut account).await;

    // Reload token pool
    let _ = crate::commands::proxy::reload_proxy_accounts(
        app_handle.state::<crate::commands::proxy::ProxyServiceState>(),
    )
    .await;

    Ok(account)
}

/// Pre-generate the OAuth authorization link (does not open a browser)
#[tauri::command]
pub async fn prepare_oauth_url(
    app_handle: tauri::AppHandle,
    oauth_client_key: Option<String>,
) -> Result<String, String> {
    let service = modules::account_service::AccountService::new(
        crate::modules::integration::SystemManager::Desktop(app_handle.clone()),
    );
    service.prepare_oauth_url(oauth_client_key).await
}

#[tauri::command]
pub async fn cancel_oauth_login() -> Result<(), String> {
    modules::oauth_server::cancel_oauth_flow();
    Ok(())
}

/// Manually submit the OAuth Code (used when Docker/remote environments can't auto-callback)
#[tauri::command]
pub async fn submit_oauth_code(code: String, state: Option<String>) -> Result<(), String> {
    modules::logger::log_info("Received manual OAuth Code submission request");
    modules::oauth_server::submit_oauth_code(code, state).await
}

#[tauri::command]
pub async fn list_oauth_clients(
) -> Result<Vec<crate::modules::oauth::OAuthClientDescriptor>, String> {
    crate::modules::oauth::list_oauth_clients()
}

#[tauri::command]
pub async fn get_active_oauth_client() -> Result<String, String> {
    crate::modules::oauth::get_active_oauth_client_key()
}

#[tauri::command]
pub async fn set_active_oauth_client(client_key: String) -> Result<(), String> {
    crate::modules::oauth::set_active_oauth_client_key(&client_key)
}

// --- Import commands ---

#[tauri::command]
pub async fn import_v1_accounts(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
) -> Result<Vec<Account>, String> {
    let accounts = modules::migration::import_from_v1().await?;

    // Try refreshing all imported accounts
    for mut account in accounts.clone() {
        let _ = internal_refresh_account_quota(&app, &mut account).await;
    }

    // Reload token pool
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;

    Ok(accounts)
}

#[tauri::command]
pub async fn import_from_db(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    target_ide: Option<String>,
) -> Result<Vec<Account>, String> {
    let imported_accounts =
        modules::migration::import_all_local_accounts(target_ide.as_deref()).await?;

    if let Some(first_acc) = imported_accounts.first() {
        let account_id = first_acc.id.clone();
        let _ = modules::account::set_current_account_id_with_target(
            &account_id,
            target_ide.as_deref(),
        );
    }

    for mut account in imported_accounts.clone() {
        let _ = internal_refresh_account_quota(&app, &mut account).await;
    }

    crate::modules::tray::update_tray_menus(&app);
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;

    Ok(imported_accounts)
}

#[tauri::command]
#[allow(dead_code)]
pub async fn import_custom_db(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    path: String,
) -> Result<Account, String> {
    // Call the refactored custom import function
    let mut account = modules::migration::import_from_custom_db_path(path).await?;

    // Automatically set as the current account
    let account_id = account.id.clone();
    modules::account::set_current_account_id(&account_id)?;

    // Automatically trigger a quota refresh
    let _ = internal_refresh_account_quota(&app, &mut account).await;

    // Refresh the tray icon display
    crate::modules::tray::update_tray_menus(&app);

    // Reload token pool
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;

    Ok(account)
}

#[tauri::command]
pub async fn sync_account_from_db(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
) -> Result<Option<Account>, String> {
    // Check if the current target is one we should not sync (like agy CLI)
    let index = modules::account::load_account_index()?;
    let current_target = index.current_target_ide.as_deref();
    if current_target == Some("agy") {
        modules::logger::log_info("Auto-sync skipped: current target is agy CLI");
        return Ok(None);
    }

    // 1. Get the Refresh Token from the DB
    let db_refresh_token = match modules::migration::get_refresh_token_from_db(current_target) {
        Ok(token) => token,
        Err(e) => {
            modules::logger::log_info(&format!("Auto-sync skipped: {}", e));
            return Ok(None);
        }
    };

    // 2. Get the Manager's current account
    let curr_account = modules::account::get_current_account()?;

    // 3. Compare: if the Refresh Token is the same, the account hasn't changed, no import needed
    if let Some(acc) = curr_account {
        if acc.token.refresh_token == db_refresh_token {
            // The account hasn't changed; since this is already a periodic task, we could optionally refresh the quota, or just return directly
            // Here we return directly to save API traffic
            return Ok(None);
        }
        modules::logger::log_info(&format!(
            "Account switch detected ({} -> new DB account), syncing...",
            acc.email
        ));
    } else {
        modules::logger::log_info("New login account detected, auto-syncing...");
    }

    // 4. Perform the full import
    let mut account = modules::migration::import_from_db(current_target).await?;

    // Since this is imported from the database, automatically set it as the Manager's current account and keep the current target
    let account_id = account.id.clone();
    modules::account::set_current_account_id_with_target(&account_id, current_target)?;

    // Automatically trigger a quota refresh
    let _ = internal_refresh_account_quota(&app, &mut account).await;

    // Refresh the tray icon display
    crate::modules::tray::update_tray_menus(&app);

    // Reload token pool
    let _ = crate::commands::proxy::reload_proxy_accounts(proxy_state).await;

    Ok(Some(account))
}

fn resolve_existing_or_parent(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return path
            .canonicalize()
            .map_err(|e| format!("failed_to_resolve_path: {}", e));
    }

    let parent = path
        .parent()
        .ok_or_else(|| "invalid_path: missing parent directory".to_string())?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("failed_to_resolve_parent: {}", e))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| "invalid_path: missing file name".to_string())?;
    Ok(canonical_parent.join(file_name))
}

fn is_sensitive_path(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    let sensitive_prefixes = [
        "/etc/",
        "/var/spool/cron",
        "/root/",
        "/proc/",
        "/sys/",
        "/dev/",
        "c:\\windows",
        "c:\\program files",
        "c:\\program files (x86)",
        "c:\\users\\administrator",
        "c:\\pagefile.sys",
    ];

    sensitive_prefixes
        .iter()
        .any(|prefix| lower == *prefix || lower.starts_with(prefix))
}

pub(crate) fn validate_user_json_path(path: &str, must_exist: bool) -> Result<PathBuf, String> {
    let requested = PathBuf::from(path);
    if requested.as_os_str().is_empty() {
        return Err("invalid_path: empty path".to_string());
    }
    if !requested.is_absolute() {
        return Err("invalid_path: absolute path is required".to_string());
    }

    let resolved = resolve_existing_or_parent(&requested)?;
    if is_sensitive_path(&resolved) {
        return Err("security_denied: sensitive system path is not allowed".to_string());
    }

    let is_json = resolved
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if !is_json {
        return Err("invalid_path: only .json files are allowed".to_string());
    }

    if must_exist {
        let metadata = std::fs::metadata(&resolved)
            .map_err(|e| format!("failed_to_read_file_metadata: {}", e))?;
        if !metadata.is_file() {
            return Err("invalid_path: expected a regular file".to_string());
        }
    }

    Ok(resolved)
}

/// Save a text file (bypasses the frontend Scope restriction)
#[tauri::command]
pub async fn save_text_file(path: String, content: String) -> Result<(), String> {
    let path = validate_user_json_path(&path, false)?;
    std::fs::write(&path, content).map_err(|e| format!("Failed to write file: {}", e))
}

/// Read a text file (bypasses the frontend Scope restriction)
#[tauri::command]
pub async fn read_text_file(path: String) -> Result<String, String> {
    let path = validate_user_json_path(&path, true)?;
    std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {}", e))
}

/// Clear the log cache
#[tauri::command]
pub async fn clear_log_cache() -> Result<(), String> {
    modules::logger::clear_logs()
}

/// Clear the Antigravity app cache
/// Used to resolve login failures, version validation errors, and similar issues
#[tauri::command]
pub async fn clear_antigravity_cache() -> Result<modules::cache::ClearResult, String> {
    modules::cache::clear_antigravity_cache(None)
}

/// Get the list of Antigravity cache paths (used for preview)
#[tauri::command]
pub async fn get_antigravity_cache_paths() -> Result<Vec<String>, String> {
    Ok(modules::cache::get_existing_cache_paths()
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect())
}

/// Open the data directory
#[tauri::command]
pub async fn open_data_folder() -> Result<(), String> {
    let path = modules::account::get_data_dir()?;

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        use crate::utils::command::CommandExtWrapper;
        std::process::Command::new("explorer")
            .creation_flags_windows()
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

/// Get the absolute path of the data directory
#[tauri::command]
pub async fn get_data_dir_path() -> Result<String, String> {
    let path = modules::account::get_data_dir()?;
    Ok(path.to_string_lossy().to_string())
}

/// Show the main window
#[tauri::command]
pub async fn show_main_window(window: tauri::Window) -> Result<(), String> {
    window.show().map_err(|e| e.to_string())
}

/// Set the window theme (used to sync the Windows title bar button color)
#[tauri::command]
pub async fn set_window_theme(window: tauri::Window, theme: String) -> Result<(), String> {
    use tauri::Theme;

    let tauri_theme = match theme.as_str() {
        "dark" => Some(Theme::Dark),
        "light" => Some(Theme::Light),
        _ => None, // system default
    };

    window.set_theme(tauri_theme).map_err(|e| e.to_string())
}

/// Get the Antigravity executable path
#[tauri::command]
pub async fn get_antigravity_path(bypass_config: Option<bool>) -> Result<String, String> {
    // 1. First check the configuration (unless explicitly asked to bypass it)
    if bypass_config != Some(true) {
        if let Ok(config) = crate::modules::config::load_app_config() {
            if let Some(path) = config.antigravity_executable {
                if std::path::Path::new(&path).exists() {
                    return Ok(path);
                }
            }
        }
    }

    // 2. Perform live detection
    match crate::modules::process::get_antigravity_executable_path(None) {
        Some(path) => Ok(path.to_string_lossy().to_string()),
        None => Err("Antigravity installation path not found".to_string()),
    }
}

/// Get the Antigravity CLI (agy) executable path
#[tauri::command]
pub async fn get_antigravity_cli_path(bypass_config: Option<bool>) -> Result<String, String> {
    // 1. First check the configuration (unless explicitly asked to bypass it)
    if bypass_config != Some(true) {
        if let Ok(config) = crate::modules::config::load_app_config() {
            if let Some(path) = config.antigravity_cli_executable {
                if std::path::Path::new(&path).exists() {
                    return Ok(path);
                }
            }
        }
    }

    // 2. Perform live detection
    match crate::modules::process::get_antigravity_cli_executable_path() {
        Some(path) => Ok(path.to_string_lossy().to_string()),
        None => Err("Antigravity CLI (agy) installation path not found".to_string()),
    }
}

/// Get the Antigravity launch arguments
#[tauri::command]
pub async fn get_antigravity_args() -> Result<Vec<String>, String> {
    match crate::modules::process::get_args_from_running_process(None) {
        Some(args) => Ok(args),
        None => Err("No running Antigravity process found".to_string()),
    }
}

/// Update check response structure
pub use crate::modules::update_checker::UpdateInfo;

/// Check for GitHub releases updates
#[tauri::command]
pub async fn check_for_updates() -> Result<UpdateInfo, String> {
    modules::logger::log_info("Received update check request triggered by the frontend");
    crate::modules::update_checker::check_for_updates().await
}

#[tauri::command]
pub async fn should_check_updates() -> Result<bool, String> {
    let settings = crate::modules::update_checker::load_update_settings()?;
    Ok(crate::modules::update_checker::should_check_for_updates(
        &settings,
    ))
}

#[tauri::command]
pub async fn update_last_check_time() -> Result<(), String> {
    crate::modules::update_checker::update_last_check_time()
}

/// Check whether installed via Homebrew Cask
#[tauri::command]
pub async fn check_homebrew_installation() -> Result<bool, String> {
    Ok(crate::modules::update_checker::is_homebrew_installed())
}

/// Check whether running as an AppImage (Linux-only)
/// Tauri's native updater only supports AppImage on Linux;
/// users with an RPM/DEB install should not trigger the native auto-updater, to avoid ENOEXEC errors.
#[tauri::command]
pub async fn check_appimage_installation() -> Result<bool, String> {
    Ok(crate::modules::update_checker::is_appimage_running())
}

/// Upgrade the app via Homebrew Cask
#[tauri::command]
pub async fn brew_upgrade_cask() -> Result<String, String> {
    modules::logger::log_info("Received Homebrew upgrade request triggered by the frontend");
    crate::modules::update_checker::brew_upgrade_cask().await
}

/// Get update settings
#[tauri::command]
pub async fn get_update_settings() -> Result<crate::modules::update_checker::UpdateSettings, String>
{
    crate::modules::update_checker::load_update_settings()
}

/// Save update settings
#[tauri::command]
pub async fn save_update_settings(
    settings: crate::modules::update_checker::UpdateSettings,
) -> Result<(), String> {
    crate::modules::update_checker::save_update_settings(&settings)
}

/// Toggle an account's reverse proxy disabled state
#[tauri::command]
pub async fn toggle_proxy_status(
    app: tauri::AppHandle,
    proxy_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
    account_id: String,
    enable: bool,
    reason: Option<String>,
) -> Result<(), String> {
    modules::logger::log_info(&format!(
        "Toggling account reverse proxy status: {} -> {}",
        account_id,
        if enable { "enabled" } else { "disabled" }
    ));

    // 1. Read the account file
    let data_dir = modules::account::get_data_dir()?;
    let account_path = data_dir
        .join("accounts")
        .join(format!("{}.json", account_id));

    if !account_path.exists() {
        return Err(format!("Account file does not exist: {}", account_id));
    }

    let content =
        std::fs::read_to_string(&account_path).map_err(|e| format!("Failed to read account file: {}", e))?;

    let mut account_json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse account file: {}", e))?;

    // 2. Update the proxy_disabled field
    if enable {
        // Enable reverse proxy
        account_json["proxy_disabled"] = serde_json::Value::Bool(false);
        account_json["proxy_disabled_reason"] = serde_json::Value::Null;
        account_json["proxy_disabled_at"] = serde_json::Value::Null;
    } else {
        // Disable reverse proxy
        let now = chrono::Utc::now().timestamp();
        account_json["proxy_disabled"] = serde_json::Value::Bool(true);
        account_json["proxy_disabled_at"] = serde_json::Value::Number(now.into());
        account_json["proxy_disabled_reason"] =
            serde_json::Value::String(reason.unwrap_or_else(|| "Manually disabled by user".to_string()));
    }

    // 3. Save to disk
    let json_str = serde_json::to_string_pretty(&account_json)
        .map_err(|e| format!("Failed to serialize account data: {}", e))?;
    std::fs::write(&account_path, json_str).map_err(|e| format!("Failed to write account file: {}", e))?;

    modules::logger::log_info(&format!(
        "Account reverse proxy status updated: {} ({})",
        account_id,
        if enable { "enabled" } else { "disabled" }
    ));

    // 4. If the reverse proxy service is running, sync to the in-memory pool immediately (avoid it still being selected after being disabled)
    {
        let instance_lock = proxy_state.instance.read().await;
        if let Some(instance) = instance_lock.as_ref() {
            // If the account being disabled is the current fixed account, automatically turn off fixed mode (memory + persisted config)
            if !enable {
                let pref_id = instance.token_manager.get_preferred_account().await;
                if pref_id.as_deref() == Some(&account_id) {
                    instance.token_manager.set_preferred_account(None).await;

                    if let Ok(mut cfg) = crate::modules::config::load_app_config() {
                        if cfg.proxy.preferred_account_id.as_deref() == Some(&account_id) {
                            cfg.proxy.preferred_account_id = None;
                            let _ = crate::modules::config::save_app_config(&cfg);
                        }
                    }
                }
            }

            instance
                .token_manager
                .reload_account(&account_id)
                .await
                .map_err(|e| format!("Failed to sync account: {}", e))?;
        }
    }

    // 5. Update the tray menu
    crate::modules::tray::update_tray_menus(&app);

    Ok(())
}

/// Warm up all available accounts
#[tauri::command]
pub async fn warm_up_all_accounts() -> Result<String, String> {
    modules::quota::warm_up_all_accounts().await
}

/// Warm up the specified account
#[tauri::command]
pub async fn warm_up_account(account_id: String) -> Result<String, String> {
    modules::quota::warm_up_account(&account_id).await
}

/// Update the account's custom label
#[tauri::command]
pub async fn update_account_label(account_id: String, label: String) -> Result<(), String> {
    // Validate the label length (counted by character count, supports Chinese)
    if label.chars().count() > 15 {
        return Err("Label length cannot exceed 15 characters".to_string());
    }

    modules::logger::log_info(&format!(
        "Updating account label: {} -> {:?}",
        account_id,
        if label.is_empty() { "none" } else { &label }
    ));

    // 1. Read the account file
    let data_dir = modules::account::get_data_dir()?;
    let account_path = data_dir
        .join("accounts")
        .join(format!("{}.json", account_id));

    if !account_path.exists() {
        return Err(format!("Account file does not exist: {}", account_id));
    }

    let content =
        std::fs::read_to_string(&account_path).map_err(|e| format!("Failed to read account file: {}", e))?;

    let mut account_json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse account file: {}", e))?;

    // 2. Update the custom_label field
    if label.is_empty() {
        account_json["custom_label"] = serde_json::Value::Null;
    } else {
        account_json["custom_label"] = serde_json::Value::String(label.clone());
    }

    // 3. Save to disk
    let json_str = serde_json::to_string_pretty(&account_json)
        .map_err(|e| format!("Failed to serialize account data: {}", e))?;
    std::fs::write(&account_path, json_str).map_err(|e| format!("Failed to write account file: {}", e))?;

    modules::logger::log_info(&format!(
        "Account label updated: {} ({})",
        account_id,
        if label.is_empty() {
            "cleared".to_string()
        } else {
            label
        }
    ));

    Ok(())
}

// ============================================================================
// HTTP API settings commands
// ============================================================================

/// Get HTTP API settings
#[tauri::command]
pub async fn get_http_api_settings() -> Result<crate::modules::http_api::HttpApiSettings, String> {
    crate::modules::http_api::load_settings()
}

/// Save HTTP API settings
#[tauri::command]
pub async fn save_http_api_settings(
    settings: crate::modules::http_api::HttpApiSettings,
) -> Result<(), String> {
    crate::modules::http_api::save_settings(&settings)
}

// ============================================================================
// Token Statistics Commands
// ============================================================================

pub use crate::modules::token_stats::{AccountTokenStats, TokenStatsAggregated, TokenStatsSummary};

#[tauri::command]
pub async fn get_token_stats_hourly(hours: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    crate::modules::token_stats::get_hourly_stats(hours)
}

#[tauri::command]
pub async fn get_token_stats_daily(days: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    crate::modules::token_stats::get_daily_stats(days)
}

#[tauri::command]
pub async fn get_token_stats_weekly(weeks: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    crate::modules::token_stats::get_weekly_stats(weeks)
}

#[tauri::command]
pub async fn get_token_stats_by_account(hours: i64) -> Result<Vec<AccountTokenStats>, String> {
    crate::modules::token_stats::get_account_stats(hours)
}

#[tauri::command]
pub async fn get_token_stats_summary(hours: i64) -> Result<TokenStatsSummary, String> {
    crate::modules::token_stats::get_summary_stats(hours)
}

#[tauri::command]
pub async fn get_token_stats_by_model(
    hours: i64,
) -> Result<Vec<crate::modules::token_stats::ModelTokenStats>, String> {
    crate::modules::token_stats::get_model_stats(hours)
}

#[tauri::command]
pub async fn get_token_stats_model_trend_hourly(
    hours: i64,
) -> Result<Vec<crate::modules::token_stats::ModelTrendPoint>, String> {
    crate::modules::token_stats::get_model_trend_hourly(hours)
}

#[tauri::command]
pub async fn get_token_stats_model_trend_daily(
    days: i64,
) -> Result<Vec<crate::modules::token_stats::ModelTrendPoint>, String> {
    crate::modules::token_stats::get_model_trend_daily(days)
}

#[tauri::command]
pub async fn get_token_stats_account_trend_hourly(
    hours: i64,
) -> Result<Vec<crate::modules::token_stats::AccountTrendPoint>, String> {
    crate::modules::token_stats::get_account_trend_hourly(hours)
}

#[tauri::command]
pub async fn get_token_stats_account_trend_daily(
    days: i64,
) -> Result<Vec<crate::modules::token_stats::AccountTrendPoint>, String> {
    crate::modules::token_stats::get_account_trend_daily(days)
}

#[tauri::command]
pub async fn query_transit_info(url: String, key: String) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .get(&url)
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;

    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("HTTP {}: {}", status, text))
    }
}
