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
static REFRESH_LOCKS: std::sync::OnceLock<
    dashmap::DashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>,
> = std::sync::OnceLock::new();

fn refresh_lock_key(refresh_token: &str, account_id: Option<&str>) -> String {
    account_id
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .unwrap_or_else(|| refresh_token.to_string())
}

// Google OAuth configuration
// 个人版官方客户端 (IDE 默认)
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";

// 企业版 / GCP (Vertex AI) 官方客户端
const GCP_CLIENT_ID: &str =
    "884354919052-36trc1jjb3tguiac32ov6cod268c5blh.apps.googleusercontent.com";
const GCP_CLIENT_SECRET: &str = "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";

pub const BUILTIN_PERSONAL_CLIENT_KEY: &str = "antigravity_personal";
pub const BUILTIN_ENTERPRISE_CLIENT_KEY: &str = "antigravity_enterprise";
pub const BUILTIN_GCP_CLIENT_KEY: &str = "antigravity_gcp";

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const TOKEN_REFRESH_SKEW_SECONDS: i64 = 900;

/// Why the caller needs a token. A rejected credential must not be returned
/// merely because its recorded expiry is still in the future.
#[derive(Clone, Copy)]
pub enum RefreshIntent<'a> {
    Expiring,
    RejectedAccessToken(&'a str),
}

fn can_reuse_account_token(
    token: &crate::models::TokenData,
    intent: RefreshIntent<'_>,
    now: i64,
) -> bool {
    !token.access_token.is_empty()
        && token.expiry_timestamp > now + TOKEN_REFRESH_SKEW_SECONDS
        && match intent {
            RefreshIntent::Expiring => true,
            RefreshIntent::RejectedAccessToken(rejected) => token.access_token != rejected,
        }
}

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
    /// Managed refreshes are durable before the single-flight lock is released.
    #[serde(skip)]
    pub(crate) persisted_token: Option<crate::models::TokenData>,
}

fn committed_token_response(token: crate::models::TokenData) -> TokenResponse {
    TokenResponse {
        access_token: token.access_token.clone(),
        expires_in: token.expiry_timestamp - chrono::Utc::now().timestamp(),
        token_type: token.token_type.clone(),
        refresh_token: Some(token.refresh_token.clone()),
        id_token: token.id_token.clone(),
        oauth_client_key: token.oauth_client_key.clone(),
        persisted_token: Some(token),
    }
}

pub(crate) fn retain_rotated_refresh_token(rotated: Option<String>, previous: String) -> String {
    rotated
        .filter(|token| !token.is_empty())
        .unwrap_or(previous)
}

/// Imports of known credentials must share managed refresh publication and
/// preserve its revision instead of constructing an unversioned old snapshot.
pub(crate) async fn refresh_access_token_for_import(
    refresh_token: &str,
    temporary_account_id: Option<&str>,
) -> Result<TokenResponse, String> {
    let lookup_token = refresh_token.to_string();
    let managed_id = tokio::task::spawn_blocking(move || {
        crate::modules::account::find_managed_account_for_refresh_token(&lookup_token)
    })
    .await
    .map_err(|_| "Import credential lookup worker failed")??;
    refresh_access_token(
        refresh_token,
        managed_id.as_deref().or(temporary_account_id),
    )
    .await
}

pub(crate) fn token_data_for_import(
    response: &TokenResponse,
    previous_refresh_token: &str,
    email: Option<String>,
    project_id: Option<String>,
    is_gcp_tos: bool,
) -> crate::models::TokenData {
    if let Some(mut committed) = response.persisted_token.clone() {
        committed.email = email;
        if project_id.is_some() {
            committed.project_id = project_id;
        }
        committed.is_gcp_tos |= is_gcp_tos;
        return committed;
    }
    crate::models::TokenData::new(
        response.access_token.clone(),
        retain_rotated_refresh_token(
            response.refresh_token.clone(),
            previous_refresh_token.to_string(),
        ),
        response.expires_in,
        email,
        project_id,
        None,
        is_gcp_tos,
        response.id_token.clone(),
    )
    .with_oauth_client_key(response.oauth_client_key.clone())
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
    #[serde(default)]
    pub is_gcp_tos: bool,
}

const OAUTH_CLIENTS_ENV: &str = "ANTIGRAVITY_OAUTH_CLIENTS";
const ACTIVE_OAUTH_CLIENT_ENV: &str = "ANTIGRAVITY_OAUTH_CLIENT_KEY";
const DEFAULT_OAUTH_CLIENT_KEY: &str = "antigravity_enterprise";

static OAUTH_CLIENT_REGISTRY: std::sync::OnceLock<std::sync::RwLock<OAuthClientRegistry>> =
    std::sync::OnceLock::new();

fn normalize_client_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
}

pub fn is_gcp_enterprise_client(client_key: Option<&str>) -> bool {
    let key = match client_key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => k.to_ascii_lowercase(),
        None => return false,
    };
    key == BUILTIN_GCP_CLIENT_KEY || key.contains("gcp") || key.contains("vertex")
}

fn build_registry() -> OAuthClientRegistry {
    let mut clients: Vec<OAuthClientConfig> = vec![
        OAuthClientConfig {
            key: normalize_client_key(DEFAULT_OAUTH_CLIENT_KEY),
            label: "Antigravity (Standard)".to_string(),
            client_id: CLIENT_ID.to_string(),
            client_secret: CLIENT_SECRET.to_string(),
            is_builtin: true,
        },
        OAuthClientConfig {
            key: normalize_client_key(BUILTIN_GCP_CLIENT_KEY),
            label: "Google Cloud / Vertex AI (Enterprise)".to_string(),
            client_id: GCP_CLIENT_ID.to_string(),
            client_secret: GCP_CLIENT_SECRET.to_string(),
            is_builtin: true,
        },
    ];

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
            is_gcp_tos: is_gcp_enterprise_client(Some(&client.key)),
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

/// Check whether the resolved OAuth client for `client_key` (or active client when `None`) is the built-in client.
pub fn is_builtin_oauth_client(client_key: Option<&str>) -> bool {
    select_auth_client(client_key)
        .map(|c| c.is_builtin)
        .unwrap_or(true)
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
    token_endpoint: &str,
    client_override: Option<&rquest::Client>,
) -> Result<TokenResponse, (Option<reqwest::StatusCode>, String)> {
    // [PHASE 2] Use the corresponding proxy based on account_id
    let client = if let Some(client) = client_override {
        client.clone()
    } else if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
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

    crate::proxy::pipeline::admission::check_current_activity().map_err(|error| (None, error))?;
    let response = client
        .post(token_endpoint)
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
    refresh_access_token_with_client_and_intent(
        refresh_token,
        account_id,
        preferred_client_key,
        RefreshIntent::Expiring,
    )
    .await
}

/// Refresh a rejected access token, or reuse a genuinely newer concurrent
/// refresh result after acquiring the same per-account lock as expiry refreshes.
pub async fn refresh_access_token_with_client_and_intent(
    refresh_token: &str,
    account_id: Option<&str>,
    preferred_client_key: Option<&str>,
    intent: RefreshIntent<'_>,
) -> Result<TokenResponse, String> {
    refresh_access_token_with_client_and_intent_at(
        refresh_token,
        account_id,
        preferred_client_key,
        intent,
        TOKEN_URL,
        None,
    )
    .await
}

// Private transport seam keeps tests on a loopback fixture without changing
// production endpoint configuration or inheriting the host's proxy settings.
async fn refresh_access_token_with_client_and_intent_at(
    refresh_token: &str,
    account_id: Option<&str>,
    preferred_client_key: Option<&str>,
    intent: RefreshIntent<'_>,
    token_endpoint: &str,
    client_override: Option<&rquest::Client>,
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
    let refresh_guard = lock.lock_owned().await;

    // [FIX #3319] Re-check the on-disk account state now that we hold the lock: a caller that
    // was waiting on it may have captured its refresh_token before another caller's refresh
    // already completed and persisted. If that happened, either reuse the now-fresh result
    // directly (no need to call Google again) or, if it's not fresh enough by time but the
    // refresh_token value has already changed, use that newer value instead of the stale one
    // this call was originally given - submitting an already-rotated refresh_token would
    // otherwise fail with invalid_grant on an endpoint that rotates them.
    let mut effective_refresh_token = refresh_token.to_string();
    let mut managed_token = None;
    if let Some(id) = account_id.filter(|id| !id.is_empty()) {
        let id_owned = id.to_string();
        if let Some(current) = tokio::task::spawn_blocking(move || {
            crate::modules::account::load_account_if_present(&id_owned)
        })
        .await
        .map_err(|_| "Account credential read worker failed")??
        {
            let now = chrono::Local::now().timestamp();
            if current.id != id {
                return Err("OAuth account identity changed while refreshing".to_string());
            }
            if can_reuse_account_token(&current.token, intent, now) {
                crate::modules::logger::log_info(&format!(
                    "[OAuth] Account {:?} was already refreshed by a concurrent caller while this call waited for the lock; reusing its result",
                    account_id
                ));
                return Ok(committed_token_response(current.token));
            }
            if current.token.refresh_token != refresh_token {
                effective_refresh_token = current.token.refresh_token.clone();
            }
            managed_token = Some(current.token);
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
            match refresh_access_token_once(
                refresh_token,
                account_id,
                client_cfg,
                token_endpoint,
                client_override,
            )
            .await
            {
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
            if let Some(current) = managed_token.as_ref() {
                let updated = token_data_from_refresh(current, token_res);
                let id = account_id.unwrap().to_string();
                let committed = tokio::task::spawn_blocking(move || {
                    // Publication continues if the request is cancelled while
                    // native storage blocks; keep the refresh lock with it.
                    let _refresh_guard = refresh_guard;
                    crate::modules::account::persist_refreshed_credentials(&id, &updated)
                })
                .await
                .map_err(|_| "Credential publication worker failed")??;
                return Ok(committed_token_response(committed));
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

    crate::proxy::pipeline::admission::check_current_activity()?;
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
    crate::proxy::pipeline::admission::check_current_activity()?;
    let latest = if let Some(id) = account_id.filter(|id| !id.is_empty()) {
        let id = id.to_string();
        tokio::task::spawn_blocking(move || crate::modules::account::load_account_if_present(&id))
            .await
            .map_err(|_| "Account credential read worker failed")??
            .map(|account| account.token)
    } else {
        None
    };
    let current_token = latest.as_ref().unwrap_or(current_token);
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

    Ok(token_data_from_refresh(current_token, response))
}

pub(crate) fn token_data_from_refresh(
    current_token: &crate::models::TokenData,
    mut response: TokenResponse,
) -> crate::models::TokenData {
    if let Some(committed) = response.persisted_token.take() {
        return committed;
    }
    let oauth_client_key =
        normalize_refreshed_oauth_client_key(current_token, response.oauth_client_key.clone());
    let mut token = crate::models::TokenData::new(
        response.access_token,
        retain_rotated_refresh_token(response.refresh_token, current_token.refresh_token.clone()),
        response.expires_in,
        current_token.email.clone(),
        current_token.project_id.clone(), // Keep original project_id
        None,                             // session_id will be generated in token_manager
        current_token.is_gcp_tos,
        response
            .id_token
            .filter(|token| !token.is_empty())
            .or(current_token.id_token.clone()),
    )
    .with_oauth_client_key(oauth_client_key);
    token.credential_revision = current_token.credential_revision.clone();
    if is_gcp_enterprise_client(token.oauth_client_key.as_deref()) {
        token.is_gcp_tos = true;
    }
    token
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_refresh_retains_rotated_credentials_and_existing_identity_metadata() {
        let mut current = crate::models::TokenData::new(
            "old-access".into(),
            "old-refresh".into(),
            0,
            Some("synthetic@test.invalid".into()),
            Some("existing-project".into()),
            None,
            false,
            Some("old-id".into()),
        );
        current.oauth_client_key = Some("antigravity".into());
        let response = |refresh_token| TokenResponse {
            access_token: "new-access".into(),
            expires_in: 3600,
            token_type: "Bearer".into(),
            refresh_token,
            id_token: None,
            oauth_client_key: None,
            persisted_token: None,
        };
        let rotated = token_data_from_refresh(&current, response(Some("new-refresh".into())));
        assert_eq!(rotated.refresh_token, "new-refresh");
        assert_eq!(rotated.id_token, current.id_token);
        assert_eq!(rotated.project_id, current.project_id);
        assert_eq!(rotated.oauth_client_key, current.oauth_client_key);
        for missing in [None, Some(String::new())] {
            assert_eq!(
                token_data_from_refresh(&current, response(missing)).refresh_token,
                "old-refresh"
            );
        }
    }

    #[test]
    fn refresh_intent_never_reuses_the_rejected_credential() {
        let now = 1_800_000_000;
        let mut token = crate::models::TokenData::new(
            "rejected-fixture".into(),
            "refresh-fixture".into(),
            3600,
            None,
            None,
            None,
            false,
            None,
        );
        token.expiry_timestamp = now + 3600;
        assert!(can_reuse_account_token(
            &token,
            RefreshIntent::Expiring,
            now
        ));
        assert!(!can_reuse_account_token(
            &token,
            RefreshIntent::RejectedAccessToken("rejected-fixture"),
            now,
        ));
        token.access_token = "newer-fixture".into();
        assert!(can_reuse_account_token(
            &token,
            RefreshIntent::RejectedAccessToken("rejected-fixture"),
            now,
        ));
        token.expiry_timestamp = now + TOKEN_REFRESH_SKEW_SECONDS;
        assert!(!can_reuse_account_token(
            &token,
            RefreshIntent::RejectedAccessToken("rejected-fixture"),
            now,
        ));
        assert!(!can_reuse_account_token(
            &token,
            RefreshIntent::Expiring,
            now
        ));
        token.expiry_timestamp = now + 3600;
        token.access_token.clear();
        assert!(!can_reuse_account_token(
            &token,
            RefreshIntent::Expiring,
            now
        ));
    }

    #[tokio::test]
    async fn rejected_token_refresh_uses_transport_but_reuses_a_newer_locked_result() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let _dir = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        let token = crate::models::TokenData::new(
            "rejected-fixture".into(),
            "refresh-fixture".into(),
            3600,
            None,
            None,
            None,
            false,
            None,
        );
        let mut account = crate::modules::account::upsert_account(
            "refresh-fixture@example.invalid".into(),
            None,
            token,
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let fixture_calls = calls.clone();
        let fixture = axum::Router::new().route(
            "/token",
            axum::routing::post(
                move |axum::Form(form): axum::Form<std::collections::HashMap<String, String>>| {
                    let calls = fixture_calls.clone();
                    async move {
                        assert_eq!(
                            form.get("grant_type").map(String::as_str),
                            Some("refresh_token")
                        );
                        assert_eq!(
                            form.get("refresh_token").map(String::as_str),
                            Some("refresh-fixture")
                        );
                        calls.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({
                            "access_token": "http-refreshed-fixture",
                            "refresh_token": "rotated-refresh-fixture",
                            "expires_in": 3600,
                            "token_type": "Bearer"
                        }))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
        let client = rquest::Client::builder().no_proxy().build().unwrap();
        let refresh = || {
            refresh_access_token_with_client_and_intent_at(
                "refresh-fixture",
                Some(&account.id),
                Some(BUILTIN_PERSONAL_CLIENT_KEY),
                RefreshIntent::RejectedAccessToken("rejected-fixture"),
                &endpoint,
                Some(&client),
            )
        };
        let forced = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(refresh(), refresh())
        })
        .await;
        // No further network is needed: the remaining cases must use disk.
        server.abort();
        let (forced, concurrent) = forced.unwrap();
        let forced = forced.unwrap();
        assert_eq!(concurrent.unwrap().access_token, "http-refreshed-fixture");
        let imported = token_data_for_import(
            &forced,
            "retired-import-refresh",
            Some(account.email.clone()),
            Some("import-project".into()),
            false,
        );
        assert_eq!(imported.refresh_token, "rotated-refresh-fixture");
        assert!(imported.credential_revision.is_some());
        assert_eq!(
            imported.expiry_timestamp,
            forced.persisted_token.as_ref().unwrap().expiry_timestamp
        );
        assert_eq!(imported.project_id.as_deref(), Some("import-project"));
        assert_eq!(forced.access_token, "http-refreshed-fixture");
        assert_eq!(
            crate::modules::account::load_account(&account.id)
                .unwrap()
                .token
                .access_token,
            "http-refreshed-fixture",
            "single-flight refresh must publish before returning"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            crate::modules::account::load_account(&account.id)
                .unwrap()
                .token
                .refresh_token,
            "rotated-refresh-fixture"
        );
        let ensured = ensure_fresh_token(&account.token, Some(&account.id))
            .await
            .unwrap();
        assert_eq!(ensured.access_token, "http-refreshed-fixture");
        assert_eq!(ensured.refresh_token, "rotated-refresh-fixture");

        let ordinary = refresh_access_token_with_client_and_intent_at(
            "refresh-fixture",
            Some(&account.id),
            Some(BUILTIN_PERSONAL_CLIENT_KEY),
            RefreshIntent::Expiring,
            &endpoint,
            Some(&client),
        )
        .await
        .unwrap();
        assert_eq!(ordinary.access_token, "http-refreshed-fixture");

        // The force caller starts before another caller persists its result.
        // It must re-read that account after waiting for the refresh lock.
        let lock = REFRESH_LOCKS
            .get_or_init(dashmap::DashMap::new)
            .get(&account.id)
            .unwrap()
            .clone();
        let guard = lock.lock().await;
        let pending_account_id = account.id.clone();
        let pending = refresh_access_token_with_client_and_intent_at(
            "refresh-fixture",
            Some(&pending_account_id),
            Some(BUILTIN_PERSONAL_CLIENT_KEY),
            RefreshIntent::RejectedAccessToken("rejected-fixture"),
            &endpoint,
            Some(&client),
        );
        tokio::pin!(pending);
        assert!(futures::poll!(&mut pending).is_pending());
        account = crate::modules::account::load_account(&account.id).unwrap();
        account.token.access_token = "concurrently-refreshed-fixture".into();
        crate::modules::account::save_account(&account).unwrap();
        drop(guard);
        let reused = pending.await.unwrap();
        assert_eq!(reused.access_token, "concurrently-refreshed-fixture");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

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

    #[test]
    fn test_is_builtin_oauth_client_default() {
        assert!(is_builtin_oauth_client(None));
        assert!(is_builtin_oauth_client(Some("antigravity_enterprise")));
        assert!(is_builtin_oauth_client(Some("unknown_non_existent_key")));
    }
}
