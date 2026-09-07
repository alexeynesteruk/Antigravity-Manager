use serde::{Deserialize, Serialize};

// [FIX #3319] Per-account lock guarding the actual refresh_token -> access_token HTTP call.
// This module is the one chokepoint every refresh path funnels through: the main proxy
// request path (token_manager::get_token), the /internal/warmup handler
// (token_manager::get_token_by_email), and the periodic background scheduler
// (oauth::ensure_fresh_token, scanned every 5 minutes for every account) all end up calling
// refresh_access_token_with_client below, and none of them previously synchronized with each
// other. Since the proxy's own smooth-refresh buffer and the scheduler's scan interval are
// both 300 seconds, an actively-used account could easily have two of these paths decide to
// refresh at nearly the same moment. If Google rotates refresh tokens (issuing a new one and
// invalidating the old on each use), the losing concurrent request's now-stale refresh_token
// is rejected as invalid_grant - and two of those in a row causes this app to disable the
// account, forcing the user to re-authenticate. Serializing the HTTP call itself, keyed by
// account (falling back to the refresh_token when no account_id is available yet, e.g. during
// initial onboarding), closes that race for every current and future caller at once.
static REFRESH_LOCKS: std::sync::OnceLock<dashmap::DashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>> =
    std::sync::OnceLock::new();

fn refresh_lock_key(refresh_token: &str, account_id: Option<&str>) -> String {
    account_id
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .unwrap_or_else(|| refresh_token.to_string())
}

// Google OAuth configuration
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const TOKEN_REFRESH_SKEW_SECONDS: i64 = 900;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: i64,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(skip)]
    pub oauth_client_key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserInfo {
    pub email: String,
    pub name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub picture: Option<String>,
}

impl UserInfo {
    /// Get best display name
    pub fn get_display_name(&self) -> Option<String> {
        // Prefer name
        if let Some(name) = &self.name {
            if !name.trim().is_empty() {
                return Some(name.clone());
            }
        }

        // If name is empty, combine given_name and family_name
        match (&self.given_name, &self.family_name) {
            (Some(given), Some(family)) => Some(format!("{} {}", given, family)),
            (Some(given), None) => Some(given.clone()),
            (None, Some(family)) => Some(family.clone()),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Clone)]
struct OAuthClientConfig {
    key: String,
    label: String,
    client_id: String,
    client_secret: String,
    is_builtin: bool,
}

#[derive(Debug, Clone)]
struct OAuthClientRegistry {
    clients: Vec<OAuthClientConfig>,
    active_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthClientDescriptor {
    pub key: String,
    pub label: String,
    pub client_id: String,
    pub is_active: bool,
    pub is_builtin: bool,
}

const OAUTH_CLIENTS_ENV: &str = "ANTIGRAVITY_OAUTH_CLIENTS";
const ACTIVE_OAUTH_CLIENT_ENV: &str = "ANTIGRAVITY_OAUTH_CLIENT_KEY";
const DEFAULT_OAUTH_CLIENT_KEY: &str = "antigravity_enterprise";

static OAUTH_CLIENT_REGISTRY: std::sync::OnceLock<std::sync::RwLock<OAuthClientRegistry>> =
    std::sync::OnceLock::new();

fn normalize_client_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
}

fn build_registry() -> OAuthClientRegistry {
    let mut clients: Vec<OAuthClientConfig> = vec![OAuthClientConfig {
        key: normalize_client_key(DEFAULT_OAUTH_CLIENT_KEY),
        label: "Antigravity Enterprise".to_string(),
        client_id: CLIENT_ID.to_string(),
        client_secret: CLIENT_SECRET.to_string(),
        is_builtin: true,
    }];

    if let Ok(raw_extra_clients) = std::env::var(OAUTH_CLIENTS_ENV) {
        for entry in raw_extra_clients.split(';') {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Expected format: key|client_id|client_secret|optional_label
            let parts: Vec<&str> = trimmed.split('|').map(|v| v.trim()).collect();
            if parts.len() < 3 {
                crate::modules::logger::log_warn(&format!(
                    "Ignored invalid OAuth client entry in {}: {}",
                    OAUTH_CLIENTS_ENV, trimmed
                ));
                continue;
            }

            let key = normalize_client_key(parts[0]);
            if key.is_empty() || parts[1].is_empty() || parts[2].is_empty() {
                crate::modules::logger::log_warn(&format!(
                    "Ignored incomplete OAuth client entry in {}: {}",
                    OAUTH_CLIENTS_ENV, trimmed
                ));
                continue;
            }

            let label = if parts.len() >= 4 && !parts[3].is_empty() {
                parts[3].to_string()
            } else {
                key.clone()
            };

            let custom_client = OAuthClientConfig {
                key: key.clone(),
                label,
                client_id: parts[1].to_string(),
                client_secret: parts[2].to_string(),
                is_builtin: false,
            };

            if let Some(existing_index) = clients.iter().position(|c| c.key == key) {
                clients[existing_index] = custom_client;
                crate::modules::logger::log_info(&format!(
                    "OAuth client '{}' overridden by {}",
                    key, OAUTH_CLIENTS_ENV
                ));
            } else {
                clients.push(custom_client);
                crate::modules::logger::log_info(&format!(
                    "OAuth client '{}' loaded from {}",
                    key, OAUTH_CLIENTS_ENV
                ));
            }
        }
    }

    let mut active_key = std::env::var(ACTIVE_OAUTH_CLIENT_ENV)
        .ok()
        .map(|v| normalize_client_key(&v))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| normalize_client_key(DEFAULT_OAUTH_CLIENT_KEY));

    if !clients.iter().any(|c| c.key == active_key) {
        active_key = clients
            .first()
            .map(|c| c.key.clone())
            .unwrap_or_else(|| normalize_client_key(DEFAULT_OAUTH_CLIENT_KEY));
    }

    OAuthClientRegistry {
        clients,
        active_key,
    }
}

fn oauth_registry() -> &'static std::sync::RwLock<OAuthClientRegistry> {
    OAUTH_CLIENT_REGISTRY.get_or_init(|| std::sync::RwLock::new(build_registry()))
}

fn get_client_by_key<'a>(
    clients: &'a [OAuthClientConfig],
    client_key: &str,
) -> Option<&'a OAuthClientConfig> {
    let normalized = normalize_client_key(client_key);
    clients.iter().find(|c| c.key == normalized)
}

fn active_or_first_client(registry: &OAuthClientRegistry) -> Option<OAuthClientConfig> {
    if let Some(active) = get_client_by_key(&registry.clients, &registry.active_key) {
        return Some(active.clone());
    }
    registry.clients.first().cloned()
}

fn select_auth_client(client_key: Option<&str>) -> Result<OAuthClientConfig, String> {
    let registry_guard = oauth_registry().read().map_err(|e| e.to_string())?;
    let registry = &*registry_guard;

    if registry.clients.is_empty() {
        return Err("No OAuth clients configured".to_string());
    }

    if let Some(key) = client_key {
        if let Some(client) = get_client_by_key(&registry.clients, key) {
            return Ok(client.clone());
        }
        return Err(format!("Unknown OAuth client key: {}", key));
    }

    active_or_first_client(registry).ok_or_else(|| "No OAuth clients configured".to_string())
}

fn get_candidate_clients(preferred_client_key: Option<&str>) -> Vec<OAuthClientConfig> {
    let registry_guard = match oauth_registry().read() {
        Ok(guard) => guard,
        Err(_) => return vec![],
    };
    let registry = &*registry_guard;

    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push_candidate = |client: &OAuthClientConfig| {
        if seen.insert(client.key.clone()) {
            candidates.push(client.clone());
        }
    };

    if let Some(preferred_key) = preferred_client_key {
        if let Some(preferred) = get_client_by_key(&registry.clients, preferred_key) {
            push_candidate(preferred);
        } else {
            crate::modules::logger::log_warn(&format!(
                "Preferred OAuth client '{}' not found; fallback to active client list",
                preferred_key
            ));
        }
    }

    if let Some(active) = get_client_by_key(&registry.clients, &registry.active_key) {
        push_candidate(active);
    }

    for client in &registry.clients {
        push_candidate(client);
    }

    candidates
}

fn is_client_mismatch_error(status: reqwest::StatusCode, error_text: &str) -> bool {
    let text = error_text.to_ascii_lowercase();
    status == reqwest::StatusCode::BAD_REQUEST
        || status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
        || text.contains("unauthorized_client")
        || text.contains("invalid_client")
}

fn normalize_refreshed_oauth_client_key(
    current_token: &crate::models::TokenData,
    refreshed_client_key: Option<String>,
) -> Option<String> {
    let resolved = refreshed_client_key.or_else(|| current_token.oauth_client_key.clone());
    let project_missing = current_token
        .project_id
        .as_deref()
        .map(str::trim)
        .map(|value| value.is_empty())
        .unwrap_or(true);

    if current_token.oauth_client_key.is_none()
        && project_missing
        && matches!(resolved.as_deref(), Some("antigravity_enterprise"))
    {
        crate::modules::logger::log_warn(
            "Refreshed token via enterprise client for a legacy account without project_id; keep oauth_client_key unset to avoid accidental enterprise lock",
        );
        return None;
    }

    resolved
}

pub fn list_oauth_clients() -> Result<Vec<OAuthClientDescriptor>, String> {
    let registry_guard = oauth_registry().read().map_err(|e| e.to_string())?;
    let registry = &*registry_guard;

    Ok(registry
        .clients
        .iter()
        .map(|client| OAuthClientDescriptor {
            key: client.key.clone(),
            label: client.label.clone(),
            client_id: client.client_id.clone(),
            is_active: client.key == registry.active_key,
            is_builtin: client.is_builtin,
        })
        .collect())
}

pub fn get_active_oauth_client_key() -> Result<String, String> {
    let registry_guard = oauth_registry().read().map_err(|e| e.to_string())?;
    Ok(registry_guard.active_key.clone())
}

pub fn set_active_oauth_client_key(client_key: &str) -> Result<(), String> {
    let mut registry_guard = oauth_registry().write().map_err(|e| e.to_string())?;
    let normalized = normalize_client_key(client_key);

    if get_client_by_key(&registry_guard.clients, &normalized).is_none() {
        let available = registry_guard
            .clients
            .iter()
            .map(|c| c.key.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Unknown OAuth client key '{}'. Available: {}",
            client_key, available
        ));
    }

    registry_guard.active_key = normalized.clone();
    crate::modules::logger::log_info(&format!("Active OAuth client switched to '{}'", normalized));
    Ok(())
}

/// Generate OAuth authorization URL with optional client selection.
/// Returns (auth_url, resolved_client_key).
pub fn get_auth_url_with_client(
    redirect_uri: &str,
    state: &str,
    client_key: Option<&str>,
) -> Result<(String, String), String> {
    let client = select_auth_client(client_key)?;

    let scopes = vec![
        "openid",
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ]
    .join(" ");

    let params = vec![
        ("client_id", client.client_id.as_str()),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", &scopes),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
        ("state", state),
    ];

    let url = url::Url::parse_with_params(AUTH_URL, &params)
        .map_err(|e| format!("Invalid Auth URL: {}", e))?;
    Ok((url.to_string(), client.key))
}

/// Generate OAuth authorization URL using current active client.
pub fn get_auth_url(redirect_uri: &str, state: &str) -> String {
    get_auth_url_with_client(redirect_uri, state, None)
        .map(|(url, _)| url)
        .expect("Failed to build OAuth URL")
}

async fn exchange_code_once(
    code: &str,
    redirect_uri: &str,
    client_cfg: &OAuthClientConfig,
) -> Result<TokenResponse, (Option<reqwest::StatusCode>, String)> {
    // [PHASE 2] For login actions there is no account_id yet, use the global pool tiered logic
    let client = if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        pool.get_effective_standard_client(None, 60).await
    } else {
        crate::utils::http::get_long_standard_client()
    };

    let params = [
        ("client_id", client_cfg.client_id.as_str()),
        ("client_secret", client_cfg.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ];

    tracing::debug!(
        "[OAuth] Sending exchange_code request with User-Agent: {}",
        crate::constants::NATIVE_OAUTH_USER_AGENT.as_str()
    );

    let response = client
        .post(TOKEN_URL)
        .header(rquest::header::USER_AGENT, crate::constants::NATIVE_OAUTH_USER_AGENT.as_str())
        .form(&params)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                (
                    None,
                    format!(
                        "Token exchange request failed: {}. Please check your network proxy settings to ensure a stable connection to Google services.",
                        e
                    ),
                )
            } else {
                (None, format!("Token exchange request failed: {}", e))
            }
        })?;

    if response.status().is_success() {
        let mut token_res = response
            .json::<TokenResponse>()
            .await
            .map_err(|e| (None, format!("Token parsing failed: {}", e)))?;
        token_res.oauth_client_key = Some(client_cfg.key.clone());

        // Add detailed logs
        crate::modules::logger::log_info(&format!(
            "Token exchange successful via [{}]! access_token: {}..., refresh_token: {}",
            client_cfg.key,
            &token_res.access_token.chars().take(20).collect::<String>(),
            if token_res.refresh_token.is_some() {
                "✓"
            } else {
                "✗ Missing"
            }
        ));

        // Log warning if refresh_token is missing
        if token_res.refresh_token.is_none() {
            crate::modules::logger::log_warn(
                "Warning: Google did not return a refresh_token. Potential reasons:\n\
                 1. User has previously authorized this application\n\
                 2. Need to revoke access in Google Cloud Console and retry\n\
                 3. OAuth parameter configuration issue",
            );
        }

        Ok(token_res)
    } else {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        Err((
            Some(status),
            format!("Token exchange failed: {}", error_text),
        ))
    }
}

/// Exchange authorization code for token using optional preferred client.
/// When preferred/active client mismatches, fallback to other configured clients.
pub async fn exchange_code_with_client(
    code: &str,
    redirect_uri: &str,
    preferred_client_key: Option<&str>,
) -> Result<TokenResponse, String> {
    let candidates = get_candidate_clients(preferred_client_key);
    if candidates.is_empty() {
        return Err("No OAuth clients configured".to_string());
    }

    let mut attempt_errors: Vec<String> = Vec::new();

    for (idx, client_cfg) in candidates.iter().enumerate() {
        match exchange_code_once(code, redirect_uri, client_cfg).await {
            Ok(token_res) => {
                if idx > 0 {
                    crate::modules::logger::log_info(&format!(
                        "OAuth code exchange recovered via fallback client [{}]",
                        client_cfg.key
                    ));
                }
                return Ok(token_res);
            }
            Err((status_opt, err_msg)) => {
                let should_fallback = status_opt
                    .map(|status| is_client_mismatch_error(status, &err_msg))
                    .unwrap_or(false);

                attempt_errors.push(format!("{} => {}", client_cfg.key, err_msg));

                if should_fallback {
                    crate::modules::logger::log_warn(&format!(
                        "OAuth code exchange failed for client [{}], trying next client: {}",
                        client_cfg.key, err_msg
                    ));
                    continue;
                }

                return Err(format!(
                    "Token exchange failed for client [{}]: {}",
                    client_cfg.key, err_msg
                ));
            }
        }
    }

    Err(format!(
        "Token exchange failed for all OAuth clients: {}",
        attempt_errors.join(" | ")
    ))
}

/// Exchange authorization code for token
pub async fn exchange_code(code: &str, redirect_uri: &str) -> Result<TokenResponse, String> {
    exchange_code_with_client(code, redirect_uri, None).await
}

async fn refresh_access_token_once(
    refresh_token: &str,
    account_id: Option<&str>,
    client_cfg: &OAuthClientConfig,
) -> Result<TokenResponse, (Option<reqwest::StatusCode>, String)> {
    // [PHASE 2] Use the corresponding proxy based on account_id
    let client = if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        pool.get_effective_standard_client(account_id, 60).await
    } else {
        crate::utils::http::get_long_standard_client()
    };

    let params = [
        ("client_id", client_cfg.client_id.as_str()),
        ("client_secret", client_cfg.client_secret.as_str()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    // [FIX #1583] Provide more detailed logging to help diagnose proxy issues in Docker environments
    if let Some(id) = account_id {
        crate::modules::logger::log_info(&format!("Refreshing Token for account: {}...", id));
    } else {
        crate::modules::logger::log_info("Refreshing Token for generic request (no account_id)...");
    }

    tracing::debug!(
        "[OAuth] Sending refresh_access_token request with User-Agent: {}",
        crate::constants::NATIVE_OAUTH_USER_AGENT.as_str()
    );

    let response = client
        .post(TOKEN_URL)
        .header(
            rquest::header::USER_AGENT,
            crate::constants::NATIVE_OAUTH_USER_AGENT.as_str(),
        )
        .form(&params)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                (
                    None,
                    format!(
                        "Refresh request failed: {}. Unable to connect to the Google authorization server, please check your proxy settings.",
                        e
                    ),
                )
            } else {
                (None, format!("Refresh request failed: {}", e))
            }
        })?;

    if response.status().is_success() {
        let mut token_data = response
            .json::<TokenResponse>()
            .await
            .map_err(|e| (None, format!("Refresh data parsing failed: {}", e)))?;
        token_data.oauth_client_key = Some(client_cfg.key.clone());

        crate::modules::logger::log_info(&format!(
            "Token refreshed successfully via [{}]! Expires in: {} seconds",
            client_cfg.key, token_data.expires_in
        ));
        Ok(token_data)
    } else {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        Err((Some(status), format!("Refresh failed: {}", error_text)))
    }
}

/// Refresh access_token using refresh_token with optional preferred OAuth client key.
/// If client mismatch occurs, it retries with other configured clients.
pub async fn refresh_access_token_with_client(
    refresh_token: &str,
    account_id: Option<&str>,
    preferred_client_key: Option<&str>,
) -> Result<TokenResponse, String> {
    let candidates = get_candidate_clients(preferred_client_key);
    if candidates.is_empty() {
        return Err("No OAuth clients configured".to_string());
    }

    // [FIX #3319] Serialize refresh attempts for this account/refresh_token across every
    // caller (main proxy path, warmup handler, background scheduler). Held for the whole
    // function body, including the retry-and-fallback loop below, so no other caller can
    // start a concurrent refresh for the same account while this one is in flight.
    let lock_key = refresh_lock_key(refresh_token, account_id);
    let lock = REFRESH_LOCKS
        .get_or_init(dashmap::DashMap::new)
        .entry(lock_key)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _refresh_guard = lock.lock().await;

    // [FIX #3319] Re-check the on-disk account state now that we hold the lock: a caller that
    // was waiting on it may have captured its refresh_token before another caller's refresh
    // already completed and persisted. If that happened, either reuse the now-fresh result
    // directly (no need to call Google again) or, if it's not fresh enough by time but the
    // refresh_token value has already changed, use that newer value instead of the stale one
    // this call was originally given - submitting an already-rotated refresh_token would
    // otherwise fail with invalid_grant on an endpoint that rotates them.
    let mut effective_refresh_token = refresh_token.to_string();
    if let Some(id) = account_id.filter(|id| !id.is_empty()) {
        let id_owned = id.to_string();
        if let Ok(Some(current)) = tokio::task::spawn_blocking(move || {
            crate::modules::account::load_account(&id_owned).ok()
        })
        .await
        {
            let now = chrono::Local::now().timestamp();
            if current.token.expiry_timestamp > now + TOKEN_REFRESH_SKEW_SECONDS {
                crate::modules::logger::log_info(&format!(
                    "[OAuth] Account {:?} was already refreshed by a concurrent caller while this call waited for the lock; reusing its result",
                    account_id
                ));
                return Ok(TokenResponse {
                    access_token: current.token.access_token,
                    expires_in: current.token.expiry_timestamp - now,
                    token_type: "Bearer".to_string(),
                    refresh_token: Some(current.token.refresh_token),
                    id_token: current.token.id_token,
                    oauth_client_key: current.token.oauth_client_key,
                });
            }
            if current.token.refresh_token != refresh_token {
                effective_refresh_token = current.token.refresh_token;
            }
        }
    }
    let refresh_token = effective_refresh_token.as_str();

    let mut attempt_errors: Vec<String> = Vec::new();

    for (idx, client_cfg) in candidates.iter().enumerate() {
        let mut last_attempt_err = None;
        let mut recovered_token = None;

        // Attempt at most 2 times for the same OAuth Client (on first invalid_grant, do a 500ms short backoff retry to confirm)
        for retry_count in 0..2 {
            if retry_count > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
            match refresh_access_token_once(refresh_token, account_id, client_cfg).await {
                Ok(token_res) => {
                    recovered_token = Some(token_res);
                    break;
                }
                Err((status_opt, err_msg)) => {
                    let is_grant_error = err_msg.contains("invalid_grant");
                    if is_grant_error && retry_count == 0 {
                        crate::modules::logger::log_warn(&format!(
                            "[OAuth] Client [{}] received a suspected invalid_grant, will retry with backoff confirmation in 500ms...",
                            client_cfg.key
                        ));
                        last_attempt_err = Some((status_opt, err_msg));
                        continue;
                    }
                    last_attempt_err = Some((status_opt, err_msg));
                    break;
                }
            }
        }

        if let Some(token_res) = recovered_token {
            if idx > 0 {
                crate::modules::logger::log_info(&format!(
                    "Refresh recovered via fallback OAuth client [{}]",
                    client_cfg.key
                ));
            }
            return Ok(token_res);
        }

        if let Some((status_opt, err_msg)) = last_attempt_err {
            let should_fallback = status_opt
                .map(|status| is_client_mismatch_error(status, &err_msg))
                .unwrap_or(false);

            attempt_errors.push(format!("{} => {}", client_cfg.key, err_msg));

            if should_fallback {
                crate::modules::logger::log_warn(&format!(
                    "Refresh failed for client [{}], trying next client: {}",
                    client_cfg.key, err_msg
                ));
                continue;
            }

            return Err(format!(
                "Refresh failed for client [{}]: {}",
                client_cfg.key, err_msg
            ));
        }
    }

    Err(format!(
        "Refresh failed for all OAuth clients: {}",
        attempt_errors.join(" | ")
    ))
}

/// Refresh access_token using refresh_token
pub async fn refresh_access_token(
    refresh_token: &str,
    account_id: Option<&str>,
) -> Result<TokenResponse, String> {
    refresh_access_token_with_client(refresh_token, account_id, None).await
}

/// Get user info
pub async fn get_user_info(
    access_token: &str,
    account_id: Option<&str>,
) -> Result<UserInfo, String> {
    let client = if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        pool.get_effective_client(account_id, 15).await
    } else {
        crate::utils::http::get_client()
    };

    let response = client
        .get(USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("User info request failed: {}", e))?;

    if response.status().is_success() {
        response
            .json::<UserInfo>()
            .await
            .map_err(|e| format!("User info parsing failed: {}", e))
    } else {
        let error_text = response.text().await.unwrap_or_default();
        Err(format!("Failed to get user info: {}", error_text))
    }
}

/// Check and refresh Token if needed
/// Returns the latest access_token
pub async fn ensure_fresh_token(
    current_token: &crate::models::TokenData,
    account_id: Option<&str>,
) -> Result<crate::models::TokenData, String> {
    let now = chrono::Local::now().timestamp();

    // Keep enough validity to avoid immediate post-switch refresh failure.
    if current_token.expiry_timestamp > now + TOKEN_REFRESH_SKEW_SECONDS {
        return Ok(current_token.clone());
    }

    // Need to refresh
    crate::modules::logger::log_info(&format!(
        "Token expiring soon for account {:?}, refreshing...",
        account_id
    ));
    let response = refresh_access_token_with_client(
        &current_token.refresh_token,
        account_id,
        current_token.oauth_client_key.as_deref(),
    )
    .await?;

    let oauth_client_key =
        normalize_refreshed_oauth_client_key(current_token, response.oauth_client_key.clone());

    // Construct new TokenData
    Ok(crate::models::TokenData::new(
        response.access_token,
        current_token.refresh_token.clone(), // refresh_token may not be returned on refresh
        response.expires_in,
        current_token.email.clone(),
        current_token.project_id.clone(), // Keep original project_id
        None,                             // session_id will be generated in token_manager
        current_token.is_gcp_tos,
        response.id_token.or(current_token.id_token.clone()), // Use new id_token or keep old one
    )
    .with_oauth_client_key(oauth_client_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_auth_url_contains_state() {
        let redirect_uri = "http://localhost:8080/callback";
        let state = "test-state-123456";
        let url = get_auth_url(redirect_uri, state);

        assert!(url.contains("state=test-state-123456"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback"));
        assert!(url.contains("response_type=code"));
    }

    #[test]
    fn test_refresh_lock_key_prefers_account_id() {
        // [FIX #3319] Two calls for the same account must land on the same lock key even if
        // they carry different (e.g. already-rotated) refresh_token values, otherwise they
        // would contend on different Arc<Mutex> instances and not actually serialize.
        assert_eq!(
            refresh_lock_key("token-a", Some("acct-1")),
            refresh_lock_key("token-b", Some("acct-1"))
        );
        assert_ne!(
            refresh_lock_key("token-a", Some("acct-1")),
            refresh_lock_key("token-a", Some("acct-2"))
        );
    }

    #[test]
    fn test_refresh_lock_key_falls_back_to_refresh_token() {
        // No account_id yet (e.g. during onboarding, before an account file exists): callers
        // sharing the same refresh_token must still serialize on the same key.
        assert_eq!(
            refresh_lock_key("shared-token", None),
            refresh_lock_key("shared-token", None)
        );
        assert_eq!(
            refresh_lock_key("shared-token", Some("")),
            refresh_lock_key("shared-token", None)
        );
        assert_ne!(
            refresh_lock_key("token-a", None),
            refresh_lock_key("token-b", None)
        );
    }

    #[test]
    fn test_refresh_locks_returns_same_mutex_for_same_key() {
        // [FIX #3319] The core correctness property: two lookups for the same key must
        // resolve to the identical Arc<Mutex<()>>, or two concurrent callers would each
        // acquire a different, independent lock and never actually contend with each other.
        let locks = REFRESH_LOCKS.get_or_init(dashmap::DashMap::new);
        let key = format!("test-key-{}", std::process::id());

        let a = locks
            .entry(key.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let b = locks
            .entry(key.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        assert!(std::sync::Arc::ptr_eq(&a, &b));

        let other_key = format!("test-key-other-{}", std::process::id());
        let c = locks
            .entry(other_key)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        assert!(!std::sync::Arc::ptr_eq(&a, &c));
    }
}
