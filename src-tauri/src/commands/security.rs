use crate::modules::security_db;
use serde::{Deserialize, Serialize};
use tauri::State;

// ==================== Request/response structures ====================

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpAccessLogQuery {
    pub page: usize,
    pub page_size: usize,
    pub search: Option<String>,
    pub blocked_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpAccessLogResponse {
    pub logs: Vec<security_db::IpAccessLog>,
    pub total: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddBlacklistRequest {
    pub ip_pattern: String,
    pub reason: Option<String>,
    pub expires_at: Option<i64>, // Unix timestamp
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddWhitelistRequest {
    pub ip_pattern: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpStatsResponse {
    pub total_requests: usize,
    pub unique_ips: usize,
    pub blocked_requests: usize,
    pub top_ips: Vec<security_db::IpRanking>,
}

// ==================== IP access log commands ====================

/// Get the list of IP access logs
#[tauri::command]
pub async fn get_ip_access_logs(query: IpAccessLogQuery) -> Result<IpAccessLogResponse, String> {
    let offset = (query.page.max(1) - 1) * query.page_size;

    let logs = security_db::get_ip_access_logs(
        query.page_size,
        offset,
        query.search.as_deref(),
        query.blocked_only,
    )?;

    // Simple total count (add a count function if precise pagination is needed)
    let total = logs.len();

    Ok(IpAccessLogResponse { logs, total })
}

/// Get IP statistics
#[tauri::command]
pub async fn get_ip_stats() -> Result<IpStatsResponse, String> {
    let stats = security_db::get_ip_stats()?;
    let top_ips = security_db::get_top_ips(10, 24)?; // Top 10 IPs in last 24 hours

    Ok(IpStatsResponse {
        total_requests: stats.total_requests as usize,
        unique_ips: stats.unique_ips as usize,
        blocked_requests: stats.blocked_count as usize,
        top_ips,
    })
}

/// Clear IP access logs
#[tauri::command]
pub async fn clear_ip_access_logs() -> Result<(), String> {
    security_db::clear_ip_access_logs()
}

// ==================== IP blacklist commands ====================

/// Get the IP blacklist
#[tauri::command]
pub async fn get_ip_blacklist() -> Result<Vec<security_db::IpBlacklistEntry>, String> {
    security_db::get_blacklist()
}

/// Add an IP to the blacklist
#[tauri::command]
pub async fn add_ip_to_blacklist(request: AddBlacklistRequest) -> Result<(), String> {
    // Validate the IP format
    if !is_valid_ip_pattern(&request.ip_pattern) {
        return Err(
            "Invalid IP pattern. Use IP address or CIDR notation (e.g., 192.168.1.0/24)"
                .to_string(),
        );
    }

    security_db::add_to_blacklist(
        &request.ip_pattern,
        request.reason.as_deref(),
        request.expires_at,
        "manual",
    )?;
    Ok(())
}

/// Remove an IP from the blacklist
#[tauri::command]
pub async fn remove_ip_from_blacklist(ip_pattern: String) -> Result<(), String> {
    // First get the blacklist and find the corresponding id
    let entries = security_db::get_blacklist()?;
    let entry = entries.iter().find(|e| e.ip_pattern == ip_pattern);

    if let Some(entry) = entry {
        security_db::remove_from_blacklist(&entry.id)
    } else {
        Err(format!("IP pattern {} not found in blacklist", ip_pattern))
    }
}

/// Clear the blacklist
#[tauri::command]
pub async fn clear_ip_blacklist() -> Result<(), String> {
    // Get all blacklist entries and delete them one by one
    let entries = security_db::get_blacklist()?;
    for entry in entries {
        security_db::remove_from_blacklist(&entry.ip_pattern)?;
    }
    Ok(())
}

/// Check whether an IP is in the blacklist
#[tauri::command]
pub async fn check_ip_in_blacklist(ip: String) -> Result<bool, String> {
    security_db::is_ip_in_blacklist(&ip)
}

// ==================== IP whitelist commands ====================

/// Get the IP whitelist
#[tauri::command]
pub async fn get_ip_whitelist() -> Result<Vec<security_db::IpWhitelistEntry>, String> {
    security_db::get_whitelist()
}

/// Add an IP to the whitelist
#[tauri::command]
pub async fn add_ip_to_whitelist(request: AddWhitelistRequest) -> Result<(), String> {
    // Validate the IP format
    if !is_valid_ip_pattern(&request.ip_pattern) {
        return Err(
            "Invalid IP pattern. Use IP address or CIDR notation (e.g., 192.168.1.0/24)"
                .to_string(),
        );
    }

    security_db::add_to_whitelist(&request.ip_pattern, request.description.as_deref())?;
    Ok(())
}

/// Remove an IP from the whitelist
#[tauri::command]
pub async fn remove_ip_from_whitelist(ip_pattern: String) -> Result<(), String> {
    // First get the whitelist and find the corresponding id
    let entries = security_db::get_whitelist()?;
    let entry = entries.iter().find(|e| e.ip_pattern == ip_pattern);

    if let Some(entry) = entry {
        security_db::remove_from_whitelist(&entry.id)
    } else {
        Err(format!("IP pattern {} not found in whitelist", ip_pattern))
    }
}

/// Clear the whitelist
#[tauri::command]
pub async fn clear_ip_whitelist() -> Result<(), String> {
    // Get all whitelist entries and delete them one by one
    let entries = security_db::get_whitelist()?;
    for entry in entries {
        security_db::remove_from_whitelist(&entry.ip_pattern)?;
    }
    Ok(())
}

/// Check whether an IP is in the whitelist
#[tauri::command]
pub async fn check_ip_in_whitelist(ip: String) -> Result<bool, String> {
    security_db::is_ip_in_whitelist(&ip)
}

// ==================== Security configuration commands ====================

/// Get the security monitoring configuration
#[tauri::command]
pub async fn get_security_config(
    app_state: State<'_, crate::commands::proxy::ProxyServiceState>,
) -> Result<crate::proxy::config::SecurityMonitorConfig, String> {
    // 1. Try to get it from the running instance (memory may hold the latest config)
    let instance_lock = app_state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        return Ok(instance.config.security_monitor.clone());
    }

    // 2. If the service isn't running, load from disk
    let app_config = crate::modules::config::load_app_config()
        .map_err(|e| format!("Failed to load config: {}", e))?;
    Ok(app_config.proxy.security_monitor)
}

/// Update the security monitoring configuration
#[tauri::command]
pub async fn update_security_config(
    config: crate::proxy::config::SecurityMonitorConfig,
    app_state: State<'_, crate::commands::proxy::ProxyServiceState>,
) -> Result<(), String> {
    // 1. Synchronously save to the config file
    let mut app_config = crate::modules::config::load_app_config()
        .map_err(|e| format!("Failed to load config: {}", e))?;
    app_config.proxy.security_monitor = config.clone();
    crate::modules::config::save_app_config(&app_config)
        .map_err(|e| format!("Failed to save config: {}", e))?;

    // 2. Update the in-memory config (if the service is running)
    {
        let mut instance_lock = app_state.instance.write().await;
        if let Some(instance) = instance_lock.as_mut() {
            instance.config.security_monitor = config.clone();
            // [FIX] Call update_security to hot-reload the running middleware config
            // This is a critical step! The middleware reads from AppState.security (Arc<RwLock<ProxySecurityConfig>>)
            // update_security() must be called for the blacklist/whitelist config to take effect in real time
            instance.axum_server.update_security(&instance.config).await;
            tracing::info!("[Security] Runtime security config hot-reloaded");
        }
    }

    tracing::info!("[Security] Security monitor config updated and saved");
    Ok(())
}

// ==================== Statistics/analytics commands ====================

/// Get IP Token consumption statistics
#[tauri::command]
pub async fn get_ip_token_stats(
    limit: Option<usize>,
    hours: Option<i64>,
) -> Result<Vec<crate::modules::proxy_db::IpTokenStats>, String> {
    crate::modules::proxy_db::get_token_usage_by_ip(limit.unwrap_or(100), hours.unwrap_or(720))
}

// ==================== Helper functions ====================

/// Validate the IP pattern format (supports a single IP and CIDR)
fn is_valid_ip_pattern(pattern: &str) -> bool {
    // Check whether it's CIDR format
    if pattern.contains('/') {
        let parts: Vec<&str> = pattern.split('/').collect();
        if parts.len() != 2 {
            return false;
        }

        // Validate the IP part
        if !is_valid_ip(parts[0]) {
            return false;
        }

        // Validate the mask part
        if let Ok(mask) = parts[1].parse::<u8>() {
            return mask <= 32;
        }
        return false;
    }

    // A single IP address
    is_valid_ip(pattern)
}

/// Validate the IP address format
fn is_valid_ip(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return false;
    }

    for part in parts {
        if part.parse::<u8>().is_err() {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_ip_patterns() {
        assert!(is_valid_ip_pattern("192.168.1.1"));
        assert!(is_valid_ip_pattern("10.0.0.0/8"));
        assert!(is_valid_ip_pattern("172.16.0.0/16"));
        assert!(is_valid_ip_pattern("192.168.1.0/24"));
        assert!(is_valid_ip_pattern("8.8.8.8/32"));
    }

    #[test]
    fn test_invalid_ip_patterns() {
        assert!(!is_valid_ip_pattern("256.1.1.1"));
        assert!(!is_valid_ip_pattern("192.168.1"));
        assert!(!is_valid_ip_pattern("192.168.1.1/33"));
        assert!(!is_valid_ip_pattern("192.168.1.1/"));
        assert!(!is_valid_ip_pattern("invalid"));
    }
}
