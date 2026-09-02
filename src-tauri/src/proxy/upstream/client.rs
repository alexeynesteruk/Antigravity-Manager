// Upstream client implementation
// Wraps a high-performance communication interface

use dashmap::DashMap;
use rquest::{header, Client, Response, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;

/// Record of an endpoint fallback attempt
#[derive(Debug, Clone)]
pub struct FallbackAttemptLog {
    /// URL of the endpoint attempted
    pub endpoint_url: String,
    /// HTTP status code (None on a network error)
    pub status: Option<u16>,
    /// Error description
    pub error: String,
}

/// Upstream call result: the response plus a record of fallback attempts
pub struct UpstreamCallResult {
    /// The final HTTP response
    pub response: Response,
    /// Failed endpoint attempts during fallback (empty on success)
    pub fallback_attempts: Vec<FallbackAttemptLog>,
}

/// Mask an email: first 3 chars + *** + first 2 chars of the domain + ***
/// Example: "userexample@gmail.com" -> "use***@gm***"
pub fn mask_email(email: &str) -> String {
    if let Some(at_pos) = email.find('@') {
        let local = &email[..at_pos];
        let domain = &email[at_pos + 1..];
        let local_prefix: String = local.chars().take(3).collect();
        let domain_prefix: String = domain.chars().take(2).collect();
        format!("{}***@{}***", local_prefix, domain_prefix)
    } else {
        // Not a valid email format; just take the first 5 chars
        let prefix: String = email.chars().take(5).collect();
        format!("{}***", prefix)
    }
}

/// [NEW] Mask error logs: strip sensitive credentials such as access_token and proxy_url from error text
#[allow(dead_code)]
pub fn sanitize_error_for_log(error_text: &str) -> String {
    // Strip the values of common sensitive keys
    let re = regex::Regex::new(r#"(?i)(access_token|refresh_token|id_token|authorization|api_key|secret|password|proxy_url|http_proxy|https_proxy)\s*[:=]\s*[^"'\\\s,}\]]+"#).unwrap();
    let redacted = re.replace_all(error_text, "$1=<redacted>");

    // Strip the Bearer token
    let re_bearer = regex::Regex::new(r#"(?i)(bearer\s+)[^"'\\\s,}\]]+"#).unwrap();
    let redacted = re_bearer.replace_all(&redacted, "$1<redacted>");

    // Cap the length to prevent log bombs
    if redacted.len() > 1000 {
        format!("{}... (truncated)", &redacted[..1000])
    } else {
        redacted.into_owned()
    }
}

// Cloud Code v1internal endpoints (fallback order: Sandbox → Daily → Prod)
// Prefer the Sandbox/Daily environments to avoid 429 errors from Prod (Ref: Issue #1176)
const V1_INTERNAL_BASE_URL_PROD: &str = "https://cloudcode-pa.googleapis.com/v1internal";
const V1_INTERNAL_BASE_URL_DAILY: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal";
const V1_INTERNAL_BASE_URL_SANDBOX: &str =
    "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal";

const V1_INTERNAL_BASE_URL_FALLBACKS: [&str; 3] = [
    V1_INTERNAL_BASE_URL_SANDBOX, // Priority 1: Sandbox (known to work and stable)
    V1_INTERNAL_BASE_URL_DAILY,   // Priority 2: Daily (backup)
    V1_INTERNAL_BASE_URL_PROD,    // Priority 3: Prod (last-resort fallback only)
];

pub struct UpstreamClient {
    default_client: RwLock<Client>,
    proxy_pool: Option<Arc<crate::proxy::proxy_pool::ProxyPoolManager>>,
    client_cache: DashMap<String, Client>, // proxy_id -> Client
    user_agent_override: RwLock<Option<String>>,
}

impl UpstreamClient {
    pub fn new(
        proxy_config: Option<crate::proxy::config::UpstreamProxyConfig>,
        proxy_pool: Option<Arc<crate::proxy::proxy_pool::ProxyPoolManager>>,
    ) -> Self {
        let default_client = match Self::build_client_internal(proxy_config.clone()) {
            Ok(client) => client,
            Err(err_with_proxy) => {
                tracing::error!(
                    error = %err_with_proxy,
                    "Failed to create default HTTP client with configured upstream proxy; retrying without proxy"
                );
                match Self::build_client_internal(None) {
                    Ok(client) => client,
                    Err(err_without_proxy) => {
                        tracing::error!(
                            error = %err_without_proxy,
                            "Failed to create default HTTP client without proxy; falling back to bare client"
                        );
                        Client::new()
                    }
                }
            }
        };

        Self {
            default_client: RwLock::new(default_client),
            proxy_pool,
            client_cache: DashMap::new(),
            user_agent_override: RwLock::new(None),
        }
    }

    /// [HOT-RELOAD] Rebuild the default HTTP client using the supplied upstream
    /// proxy config. Called from `update_proxy` so changes to the upstream proxy
    /// take effect without restarting the app.
    pub async fn rebuild_default_client(
        &self,
        proxy_config: Option<crate::proxy::config::UpstreamProxyConfig>,
    ) {
        let new_client = match Self::build_client_internal(proxy_config.clone()) {
            Ok(c) => c,
            Err(err_with_proxy) => {
                tracing::error!(
                    error = %err_with_proxy,
                    "Hot-reload: failed to rebuild default HTTP client with configured upstream proxy; retrying without proxy"
                );
                match Self::build_client_internal(None) {
                    Ok(c) => c,
                    Err(err_without_proxy) => {
                        tracing::error!(
                            error = %err_without_proxy,
                            "Hot-reload: failed to rebuild default HTTP client without proxy; keeping previous client"
                        );
                        return;
                    }
                }
            }
        };
        let mut guard = self.default_client.write().await;
        *guard = new_client;
        tracing::info!("UpstreamClient default_client rebuilt (upstream proxy hot-reloaded)");
    }

    /// [HOT-RELOAD] Drop all per-proxy cached clients. Call after the pool
    /// configuration changes (proxy URL/credentials edited, proxy removed,
    /// bindings changed) so the next request rebuilds with fresh settings.
    pub fn clear_client_cache(&self) {
        let size = self.client_cache.len();
        self.client_cache.clear();
        if size > 0 {
            tracing::info!("UpstreamClient cleared {} cached per-proxy clients", size);
        }
    }

    /// Internal helper to build a client with optional upstream proxy config
    fn build_client_internal(
        proxy_config: Option<crate::proxy::config::UpstreamProxyConfig>,
    ) -> Result<Client, rquest::Error> {
        let mut builder = Client::builder()
            .emulation(rquest_util::Emulation::Chrome123)
            // Connection settings (improve connection reuse, reduce setup cost)
            .connect_timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(20) // At most 20 idle connections per host (matches the official fingerprint)
            .pool_idle_timeout(Duration::from_secs(90)) // Keep idle connections for 90 seconds
            .tcp_keepalive(Duration::from_secs(60)) // TCP keepalive probe every 60 seconds
            // Force HTTP/2, allowing ALPN-based downgrade/negotiation behind a SOCKS/HTTPS proxy
            .timeout(Duration::from_secs(600));

        builder = Self::apply_default_user_agent(builder);

        if let Some(config) = proxy_config {
            if config.enabled && !config.url.is_empty() {
                let url = crate::proxy::config::normalize_proxy_url(&config.url);
                if let Ok(proxy) = rquest::Proxy::all(&url) {
                    builder = builder.proxy(proxy);
                    tracing::info!("UpstreamClient enabled proxy: {}", url);
                }
            }
        }

        builder.build()
    }

    /// Build a client with a specific PoolProxyConfig (from ProxyPool)
    fn build_client_with_proxy(
        &self,
        proxy_config: crate::proxy::proxy_pool::PoolProxyConfig,
    ) -> Result<Client, rquest::Error> {
        // Reuse base settings similar to default client but with specific proxy
        let builder = Client::builder()
            .emulation(rquest_util::Emulation::Chrome123)
            .connect_timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .timeout(Duration::from_secs(600))
            .proxy(proxy_config.proxy); // Apply the specific proxy

        Self::apply_default_user_agent(builder).build()
    }

    fn apply_default_user_agent(builder: rquest::ClientBuilder) -> rquest::ClientBuilder {
        let ua = crate::constants::USER_AGENT.as_str();
        if header::HeaderValue::from_str(ua).is_ok() {
            builder.user_agent(ua)
        } else {
            tracing::warn!(
                user_agent = %ua,
                "Invalid default User-Agent value, using fallback"
            );
            builder.user_agent("antigravity")
        }
    }

    /// Set dynamic User-Agent override
    pub async fn set_user_agent_override(&self, ua: Option<String>) {
        let mut lock = self.user_agent_override.write().await;
        *lock = ua;
        tracing::debug!("UpstreamClient User-Agent override updated: {:?}", lock);
    }

    /// Get current User-Agent
    pub async fn get_user_agent(&self) -> String {
        let ua_override = self.user_agent_override.read().await;
        ua_override
            .as_ref()
            .cloned()
            .unwrap_or_else(|| crate::constants::USER_AGENT.clone())
    }

    /// Get client for a specific account (or default if no proxy bound)
    pub async fn get_client(&self, account_id: Option<&str>) -> Client {
        if let Some(pool) = &self.proxy_pool {
            if let Some(acc_id) = account_id {
                // Try to get per-account proxy
                match pool.get_proxy_for_account(acc_id).await {
                    Ok(Some(proxy_cfg)) => {
                        // Check cache
                        if let Some(client) = self.client_cache.get(&proxy_cfg.entry_id) {
                            return client.clone();
                        }
                        // Build new client and cache it
                        match self.build_client_with_proxy(proxy_cfg.clone()) {
                            Ok(client) => {
                                self.client_cache
                                    .insert(proxy_cfg.entry_id.clone(), client.clone());
                                tracing::info!(
                                    "Using ProxyPool proxy ID: {} for account: {}",
                                    proxy_cfg.entry_id,
                                    acc_id
                                );
                                return client;
                            }
                            Err(e) => {
                                tracing::error!("Failed to build client for proxy {}: {}, falling back to default", proxy_cfg.entry_id, e);
                            }
                        }
                    }
                    Ok(None) => {
                        // No proxy found or required for this account, use default
                    }
                    Err(e) => {
                        tracing::error!(
                            "Error getting proxy for account {}: {}, falling back to default",
                            acc_id,
                            e
                        );
                    }
                }
            }
        }
        // Fallback to default client
        self.default_client.read().await.clone()
    }

    /// Build v1internal URL
    fn build_url(base_url: &str, method: &str, query_string: Option<&str>) -> String {
        if let Some(qs) = query_string {
            format!("{}:{}?{}", base_url, method, qs)
        } else {
            format!("{}:{}", base_url, method)
        }
    }

    /// Determine if we should try next endpoint (fallback logic)
    fn should_try_next_endpoint(status: StatusCode) -> bool {
        status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::NOT_FOUND
            || status.is_server_error()
    }

    /// Call v1internal API (Basic Method)
    ///
    /// Initiates a basic network request, supporting multi-endpoint auto-fallback.
    /// [UPDATED] Takes optional account_id for per-account proxy selection.
    pub async fn call_v1_internal(
        &self,
        method: &str,
        access_token: &str,
        body: Value,
        query_string: Option<&str>,
        account_id: Option<&str>, // [NEW] Account ID for proxy selection
    ) -> Result<UpstreamCallResult, String> {
        self.call_v1_internal_with_headers(
            method,
            access_token,
            body,
            query_string,
            std::collections::HashMap::new(),
            account_id,
        )
        .await
    }

    /// [FIX #765] Call the v1internal API, passing through additional headers
    /// [ENHANCED] Returns UpstreamCallResult, including fallback attempt records for debug logging
    pub async fn call_v1_internal_with_headers(
        &self,
        method: &str,
        access_token: &str,
        body: Value,
        query_string: Option<&str>,
        extra_headers: std::collections::HashMap<String, String>,
        account_id: Option<&str>, // [NEW] Account ID
    ) -> Result<UpstreamCallResult, String> {
        // [NEW] Get client based on account (cached in proxy pool manager)
        let client = self.get_client(account_id).await;

        // Build headers (reused across all endpoints)
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", access_token))
                .map_err(|e| e.to_string())?,
        );

        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(&self.get_user_agent().await).unwrap_or_else(|e| {
                tracing::warn!("Invalid User-Agent header value, using fallback: {}", e);
                header::HeaderValue::from_static("antigravity")
            }),
        );

        // [ENHANCED] Inject the headers that characterize the official Antigravity client
        // 1. Client Identity
        headers.insert(
            "x-client-name",
            header::HeaderValue::from_static("antigravity"),
        );
        if let Ok(ver) = header::HeaderValue::from_str(&crate::constants::CURRENT_VERSION) {
            headers.insert("x-client-version", ver);
        }

        // 2. Device & Session Identity
        // Machine ID (Persistent)
        if let Ok(mid) = machine_uid::get() {
            if let Ok(mid_val) = header::HeaderValue::from_str(&mid) {
                headers.insert("x-machine-id", mid_val);
            }
        }
        // Session ID (Per App Launch)
        if let Ok(sess_val) = header::HeaderValue::from_str(&crate::constants::SESSION_ID) {
            headers.insert("x-vscode-sessionid", sess_val);
        }

        // [REMOVED v4.1.24] x-goog-api-client (gl-node/fire/grpc) header has been removed.
        // This header belongs to the IDE's JS layer, not the official client's egress.
        // Sending it creates a contradictory "Electron + Node.js" fingerprint.

        // Keep body.project for content requests, but omit the quota-project header.
        let is_content_request = matches!(method, "generateContent" | "streamGenerateContent");
        if !is_content_request {
            if let Some(proj) = body.get("project").and_then(|v| v.as_str()) {
                if !proj.is_empty() && proj != "test-project" && proj != "project-id" {
                    if let Ok(hv) = header::HeaderValue::from_str(proj) {
                        headers.insert("x-goog-user-project", hv);
                    }
                }
            }
        }

        // Inject additional headers (such as anthropic-beta)
        for (k, v) in extra_headers {
            if let Ok(hk) = header::HeaderName::from_bytes(k.as_bytes()) {
                if let Ok(hv) = header::HeaderValue::from_str(&v) {
                    headers.insert(hk, hv);
                }
            }
        }
        if is_content_request {
            headers.remove("x-goog-user-project");
        }

        // [DEBUG] Log headers for verification
        tracing::debug!(?headers, "Final Upstream Request Headers");

        let mut has_triggered_downgrade = false;

        // [TEMPORARY FIX #3074] Automatic fallback-retry logic for 403 SERVICE_DISABLED
        // Wrapped in an outer loop so the header can be removed and the request retried on that specific error
        loop {
            let mut last_err: Option<String> = None;
            let mut fallback_attempts: Vec<FallbackAttemptLog> = Vec::new();
            let mut should_retry_without_header = false;

            // Iterate every endpoint, switching automatically on failure
            for (idx, base_url) in V1_INTERNAL_BASE_URL_FALLBACKS.iter().enumerate() {
                let url = Self::build_url(base_url, method, query_string);
                let has_next = idx + 1 < V1_INTERNAL_BASE_URL_FALLBACKS.len();

                let body_bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;

                let mut req_builder = client.post(&url).headers(headers.clone());

                // [FIX] Use chunked-transfer emulation only for the streaming endpoint (streamGenerateContent)
                // Send a normal fixed-length body for the others (generateContent, loadCodeAssist)
                // Otherwise image generation is rejected or rate limited (429) by Google for a missing Content-Length
                if method == "streamGenerateContent" {
                    let stream_bytes = body_bytes.clone();
                    req_builder = req_builder.body(rquest::Body::wrap_stream(
                        futures::stream::once(async move { Ok::<_, std::io::Error>(stream_bytes) }),
                    ));
                } else {
                    req_builder = req_builder.body(body_bytes.clone());
                }

                let response = req_builder.send().await;

                match response {
                    Ok(resp) => {
                        let status = resp.status();
                        if status.is_success() {
                            if idx > 0 {
                                tracing::info!(
                                    "✓ Upstream fallback succeeded | Endpoint: {} | Status: {} | Next endpoints available: {}",
                                    base_url,
                                    status,
                                    V1_INTERNAL_BASE_URL_FALLBACKS.len() - idx - 1
                                );
                            } else {
                                tracing::debug!(
                                    "✓ Upstream request succeeded | Endpoint: {} | Status: {}",
                                    base_url,
                                    status
                                );
                            }
                            return Ok(UpstreamCallResult {
                                response: resp,
                                fallback_attempts,
                            });
                        }

                        // [NEW] Detect 403 errors (Issue #3074)
                        // Whenever the project header is present and a 403 comes back, retry once with fallback
                        if status == StatusCode::FORBIDDEN
                            && !has_triggered_downgrade
                            && headers.contains_key("x-goog-user-project")
                        {
                            tracing::warn!(
                                "Detected 403 Forbidden with project header, retrying WITHOUT x-goog-user-project header (Account: {:?})",
                                account_id
                            );
                            should_retry_without_header = true;
                            break;
                        }

                        // Switch if another endpoint remains and the current error is retryable
                        if has_next && Self::should_try_next_endpoint(status) {
                            let err_msg = format!("Upstream {} returned {}", base_url, status);
                            tracing::warn!(
                                "Upstream endpoint returned {} at {} (method={}), trying next endpoint",
                                status,
                                base_url,
                                method
                            );
                            // [NEW] Record the fallback attempt
                            fallback_attempts.push(FallbackAttemptLog {
                                endpoint_url: url.clone(),
                                status: Some(status.as_u16()),
                                error: err_msg.clone(),
                            });
                            last_err = Some(err_msg);
                            continue;
                        }

                        // Non-retryable error, or the last endpoint: return directly
                        return Ok(UpstreamCallResult {
                            response: resp,
                            fallback_attempts,
                        });
                    }
                    Err(e) => {
                        let msg = format!("HTTP request failed at {}: {}", base_url, e);
                        tracing::debug!("{}", msg);
                        // [NEW] Record the fallback attempt for a network error
                        fallback_attempts.push(FallbackAttemptLog {
                            endpoint_url: url.clone(),
                            status: None,
                            error: msg.clone(),
                        });
                        last_err = Some(msg);

                        // Break out of the loop on the last endpoint
                        if !has_next {
                            break;
                        }
                        continue;
                    }
                }
            }

            // Handle the fallback logic
            if should_retry_without_header {
                headers.remove("x-goog-user-project");
                has_triggered_downgrade = true;
                // Restart the outer loop, trying again from the first endpoint
                continue;
            }

            // If no fallback was triggered and every endpoint was tried, return the last error
            return Err(last_err.unwrap_or_else(|| "All endpoints failed".to_string()));
        }
    }

    /// Call the v1internal API (with 429 retry, closure-based)
    ///
    /// Core request logic with fault tolerance and retry
    ///
    /// # Arguments
    /// * `method` - API method (e.g., "generateContent")
    /// * `query_string` - Optional query string (e.g., "?alt=sse")
    /// * `get_credentials` - closure that obtains credentials (supports account rotation)
    /// * `build_body` - closure that takes a project_id and builds the request body
    /// * `max_attempts` - maximum number of retries
    ///
    /// # Returns
    /// HTTP Response
    // Removed the deprecated retry method (call_v1_internal_with_retry)

    // Removed the deprecated helper (parse_retry_delay)

    // Removed the deprecated helper (parse_duration_ms)

    /// Get the list of available models
    ///
    /// Fetch the remote model list, with automatic fallback across endpoints
    #[allow(dead_code)] // API ready for future model discovery feature
    pub async fn fetch_available_models(
        &self,
        access_token: &str,
        account_id: Option<&str>,
    ) -> Result<Value, String> {
        // Reuse call_v1_internal, then parse the JSON
        let result = self
            .call_v1_internal(
                "fetchAvailableModels",
                access_token,
                serde_json::json!({}),
                None,
                account_id,
            )
            .await?;
        let json: Value = result
            .response
            .json()
            .await
            .map_err(|e| format!("Parse json failed: {}", e))?;
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_url() {
        let base_url = "https://cloudcode-pa.googleapis.com/v1internal";

        let url1 = UpstreamClient::build_url(base_url, "generateContent", None);
        assert_eq!(
            url1,
            "https://cloudcode-pa.googleapis.com/v1internal:generateContent"
        );

        let url2 = UpstreamClient::build_url(base_url, "streamGenerateContent", Some("alt=sse"));
        assert_eq!(
            url2,
            "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
    }
}
