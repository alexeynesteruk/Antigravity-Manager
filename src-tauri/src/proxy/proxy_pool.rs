use crate::proxy::config::{ProxyEntry, ProxyPoolConfig, ProxySelectionStrategy};
use dashmap::DashMap;
use futures::{stream, StreamExt};
use rquest::Client;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use rquest_util::Emulation;
use std::sync::OnceLock;

/// Global proxy pool manager singleton
pub static GLOBAL_PROXY_POOL: OnceLock<Arc<ProxyPoolManager>> = OnceLock::new();

/// Get the global proxy pool manager
pub fn get_global_proxy_pool() -> Option<Arc<ProxyPoolManager>> {
    GLOBAL_PROXY_POOL.get().cloned()
}

/// Initialize the global proxy pool manager
pub fn init_global_proxy_pool(config: Arc<RwLock<ProxyPoolConfig>>) -> Arc<ProxyPoolManager> {
    let manager = Arc::new(ProxyPoolManager::new(config));
    let _ = GLOBAL_PROXY_POOL.set(manager.clone());
    manager
}

/// Proxy config (used to build a reqwest Client)
/// Note: renamed to PoolProxyConfig to avoid conflicting with config::ProxyConfig
#[derive(Debug, Clone)]
pub struct PoolProxyConfig {
    pub proxy: rquest::Proxy,
    pub entry_id: String,
}

/// Proxy pool manager
pub struct ProxyPoolManager {
    config: Arc<RwLock<ProxyPoolConfig>>,

    /// Proxy usage count (proxy_id -> count)
    usage_counter: Arc<DashMap<String, usize>>,

    /// Account-to-proxy binding (account_id -> proxy_id)
    account_bindings: Arc<DashMap<String, String>>,

    /// Round-robin index (used for the RoundRobin strategy)
    round_robin_index: Arc<AtomicUsize>,
}

impl ProxyPoolManager {
    pub fn new(config: Arc<RwLock<ProxyPoolConfig>>) -> Self {
        // Load the saved bindings from config
        let account_bindings = Arc::new(DashMap::new());

        // Read the config in a blocking way (since new is not async)
        // Note: use try_read here to avoid deadlock
        if let Ok(cfg) = config.try_read() {
            for (account_id, proxy_id) in &cfg.account_bindings {
                account_bindings.insert(account_id.clone(), proxy_id.clone());
            }
            if !cfg.account_bindings.is_empty() {
                tracing::info!(
                    "[ProxyPool] Loaded {} account bindings from config",
                    cfg.account_bindings.len()
                );
            }
        }

        Self {
            config,
            usage_counter: Arc::new(DashMap::new()),
            account_bindings,
            round_robin_index: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// [NEW] Get the "effectively resolved" HttpClient for a given account
    /// Logic:
    /// 1. An explicit account-proxy binding takes priority (Account-Proxy Binding)
    /// 2. If there is no binding and "auto global" is enabled, take the first node in the pool
    /// 3. If neither applies, check the global upstream proxy (Upstream Proxy) [handled by the caller as fallback]
    pub async fn get_effective_client(
        &self,
        account_id: Option<&str>,
        timeout_secs: u64,
    ) -> Client {
        let mut builder = Client::builder()
            .emulation(Emulation::Chrome123)
            .timeout(Duration::from_secs(timeout_secs));

        // Try to get the proxy config
        let proxy_opt = if let Some(acc_id) = account_id {
            self.get_proxy_for_account(acc_id).await.ok().flatten()
        } else {
            // Generic request without an account_id; if the proxy pool is enabled, select a node from it as the exit by default
            let config = self.config.read().await;
            if config.enabled {
                let res = self.select_proxy_from_pool(&config).await.ok().flatten();
                if let Some(ref p) = res {
                    tracing::info!(
                        "[Proxy] Route: Generic Request -> Proxy {} (Pool)",
                        p.entry_id
                    );
                } else {
                    // [FIX #1583] Explicitly log the case where no proxy is available in the pool
                    tracing::warn!("[Proxy] Route: Generic Request -> No available proxy in pool, falling back to upstream or direct");
                }
                res
            } else {
                tracing::debug!("[Proxy] Route: Generic Request -> Proxy pool disabled");
                None
            }
        };

        if let Some(proxy_cfg) = proxy_opt {
            builder = builder.proxy(proxy_cfg.proxy);
            // Already logged more detail in get_proxy_for_account or pool selection
        } else {
            // Fall back to the app config's single upstream proxy
            if let Ok(app_cfg) = crate::modules::config::load_app_config() {
                let up = app_cfg.proxy.upstream_proxy;
                if up.enabled && !up.url.is_empty() {
                    if let Ok(p) = rquest::Proxy::all(&up.url) {
                        tracing::info!(
                            "[Proxy] Route: {:?} -> Upstream: {} (AppConfig)",
                            account_id.unwrap_or("Generic"),
                            up.url
                        );
                        builder = builder.proxy(p);
                    }
                } else {
                    tracing::info!(
                        "[Proxy] Route: {:?} -> Direct",
                        account_id.unwrap_or("Generic")
                    );
                }
            }
        }

        builder.build().unwrap_or_else(|_| Client::new())
    }

    /// [NEW] Get the "effectively resolved" featureless Standard HttpClient for a given account (dedicated to pure scenarios such as OAuth revocation)
    pub async fn get_effective_standard_client(
        &self,
        account_id: Option<&str>,
        timeout_secs: u64,
    ) -> Client {
        let mut builder = Client::builder()
            // No Emulation setting; use the plain base TLS fingerprint
            .timeout(Duration::from_secs(timeout_secs));

        // Try to get the proxy config
        let proxy_opt = if let Some(acc_id) = account_id {
            self.get_proxy_for_account(acc_id).await.ok().flatten()
        } else {
            // Generic request without an account_id; if the proxy pool is enabled, select a node from it as the exit by default
            let config = self.config.read().await;
            if config.enabled {
                let res = self.select_proxy_from_pool(&config).await.ok().flatten();
                if let Some(ref p) = res {
                    tracing::info!(
                        "[Proxy] Route: Generic Request (Standard Client) -> Proxy {} (Pool)",
                        p.entry_id
                    );
                } else {
                    tracing::warn!("[Proxy] Route: Generic Request (Standard Client) -> No available proxy in pool, falling back to upstream or direct");
                }
                res
            } else {
                tracing::debug!(
                    "[Proxy] Route: Generic Request (Standard Client) -> Proxy pool disabled"
                );
                None
            }
        };

        if let Some(proxy_cfg) = proxy_opt {
            builder = builder.proxy(proxy_cfg.proxy);
        } else {
            // Fall back to the app config's single upstream proxy
            if let Ok(app_cfg) = crate::modules::config::load_app_config() {
                let up = app_cfg.proxy.upstream_proxy;
                if up.enabled && !up.url.is_empty() {
                    if let Ok(p) = rquest::Proxy::all(&up.url) {
                        tracing::info!(
                            "[Proxy] Route: {:?} (Standard Client) -> Upstream: {} (AppConfig)",
                            account_id.unwrap_or("Generic"),
                            up.url
                        );
                        builder = builder.proxy(p);
                    }
                } else {
                    tracing::info!(
                        "[Proxy] Route: {:?} (Standard Client) -> Direct",
                        account_id.unwrap_or("Generic")
                    );
                }
            }
        }

        builder.build().unwrap_or_else(|_| Client::new())
    }

    /// Get a proxy for an account
    pub async fn get_proxy_for_account(
        &self,
        account_id: &str,
    ) -> Result<Option<PoolProxyConfig>, String> {
        let config = self.config.read().await;

        if !config.enabled || config.proxies.is_empty() {
            return Ok(None);
        }

        // 1. Prefer the account binding (dedicated IP)
        if let Some(proxy) = self.get_bound_proxy(account_id, &config).await? {
            tracing::info!(
                "[Proxy] Route: Account {} -> Proxy {} (Bound)",
                account_id,
                proxy.entry_id
            );
            return Ok(Some(proxy));
        }

        // 2. Otherwise select from the pool using the strategy (shared pool)
        let res = self.select_proxy_from_pool(&config).await?;
        if let Some(ref p) = res {
            tracing::info!(
                "[Proxy] Route: Account {} -> Proxy {} (Pool)",
                account_id,
                p.entry_id
            );
        }
        Ok(res)
    }

    /// Get the proxy bound to an account
    async fn get_bound_proxy(
        &self,
        account_id: &str,
        config: &ProxyPoolConfig,
    ) -> Result<Option<PoolProxyConfig>, String> {
        if let Some(proxy_id) = self.account_bindings.get(account_id) {
            if let Some(entry) = config.proxies.iter().find(|p| p.id == *proxy_id.value()) {
                if entry.enabled {
                    // If auto failover is enabled and the proxy is unhealthy, return None (falls back to other strategies or fails)
                    if config.auto_failover && !entry.is_healthy {
                        return Ok(None);
                    }
                    return Ok(Some(self.build_proxy_config(entry)?));
                }
            }
        }
        Ok(None)
    }

    /// Select a proxy from the proxy pool
    async fn select_proxy_from_pool(
        &self,
        config: &ProxyPoolConfig,
    ) -> Result<Option<PoolProxyConfig>, String> {
        // [FIX] Dedicated isolation logic: exclude all already-bound proxies to protect the safety of dedicated-IP accounts
        let bound_ids: std::collections::HashSet<String> = self
            .account_bindings
            .iter()
            .map(|kv| kv.value().clone())
            .collect();

        let healthy_proxies: Vec<_> = config
            .proxies
            .iter()
            .filter(|p| {
                if !p.enabled {
                    return false;
                }
                if config.auto_failover && !p.is_healthy {
                    return false;
                }
                // If this proxy has already been "exclusively bound" to an account, exclude it from the shared round-robin
                if bound_ids.contains(&p.id) {
                    return false;
                }
                true
            })
            .collect();

        if healthy_proxies.is_empty() {
            // If all proxies are bound, or the pool itself is empty, try to return an enabled proxy that doesn't depend on bindings
            // (this can be further tuned per business needs; for now strict isolation is kept)
            return Ok(None);
        }

        let selected = match config.strategy {
            ProxySelectionStrategy::RoundRobin => self.select_round_robin(&healthy_proxies),
            ProxySelectionStrategy::Random => self.select_random(&healthy_proxies),
            ProxySelectionStrategy::Priority => self.select_by_priority(&healthy_proxies),
            ProxySelectionStrategy::LeastConnections => {
                self.select_least_connections(&healthy_proxies)
            }
            ProxySelectionStrategy::WeightedRoundRobin => self.select_weighted(&healthy_proxies),
        };

        if let Some(entry) = selected {
            // Update the count
            *self.usage_counter.entry(entry.id.clone()).or_insert(0) += 1;
            Ok(Some(self.build_proxy_config(entry)?))
        } else {
            Ok(None)
        }
    }

    fn select_round_robin<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        if proxies.is_empty() {
            return None;
        }
        let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed);
        Some(proxies[index % proxies.len()])
    }

    fn select_random<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        if proxies.is_empty() {
            return None;
        }
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        proxies.choose(&mut rng).copied()
    }

    fn select_by_priority<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        // Lower priority value takes precedence
        proxies.iter().min_by_key(|p| p.priority).copied()
    }

    fn select_least_connections<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        proxies
            .iter()
            .min_by_key(|p| self.usage_counter.get(&p.id).map(|v| *v).unwrap_or(0))
            .copied()
    }

    fn select_weighted<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        // Simple implementation: priority-like weighting, using Priority as a stand-in for now
        self.select_by_priority(proxies)
    }

    /// Build the rquest::Proxy config
    fn build_proxy_config(&self, entry: &ProxyEntry) -> Result<PoolProxyConfig, String> {
        let raw_url = crate::proxy::config::normalize_proxy_url(&entry.url);

        // Try to parse the URL to extract any username and password embedded in it
        let (clean_url, parsed_auth) = match url::Url::parse(&raw_url) {
            Ok(mut u) => {
                let user = if !u.username().is_empty() {
                    Some(u.username().to_string())
                } else {
                    None
                };
                let pass = u.password().map(|p| p.to_string());

                // Strip the credentials from the URL to avoid parsing issues in certain underlying libraries
                let _ = u.set_username("");
                let _ = u.set_password(None);

                let auth = if let (Some(user), Some(pass)) = (user, pass) {
                    Some((user, pass))
                } else {
                    None
                };
                (u.to_string(), auth)
            }
            Err(_) => (raw_url.clone(), None),
        };

        let mut proxy = rquest::Proxy::all(&clean_url)
            .or_else(|_| rquest::Proxy::all(&raw_url))
            .map_err(|e| format!("Invalid proxy URL: {}", e))?;

        // Prefer the structured auth, falling back to the auth parsed out of the embedded URL
        if let Some(auth) = &entry.auth {
            if !auth.username.is_empty() {
                proxy = proxy.basic_auth(&auth.username, &auth.password);
            }
        } else if let Some((user, pass)) = parsed_auth {
            proxy = proxy.basic_auth(&user, &pass);
        }

        Ok(PoolProxyConfig {
            proxy,
            entry_id: entry.id.clone(),
        })
    }

    /// Bind an account to a proxy
    pub async fn bind_account_to_proxy(
        &self,
        account_id: String,
        proxy_id: String,
    ) -> Result<(), String> {
        // Check whether the proxy exists
        {
            let config = self.config.read().await;
            if !config.proxies.iter().any(|p| p.id == proxy_id) {
                return Err(format!("Proxy {} not found", proxy_id));
            }

            // Check the proxy's max account count limit
            if let Some(entry) = config.proxies.iter().find(|p| p.id == proxy_id) {
                if let Some(max) = entry.max_accounts {
                    if max > 0 {
                        let current_count = self
                            .account_bindings
                            .iter()
                            .filter(|kv| *kv.value() == proxy_id)
                            .count();
                        if current_count >= max {
                            return Err(format!(
                                "Proxy {} has reached max accounts limit",
                                proxy_id
                            ));
                        }
                    }
                }
            }
        }

        // Update the in-memory binding
        self.account_bindings
            .insert(account_id.clone(), proxy_id.clone());

        // Persist to the config file
        self.persist_bindings().await;

        tracing::info!(
            "[ProxyPool] Bound account {} to proxy {}",
            account_id,
            proxy_id
        );
        Ok(())
    }

    /// Unbind an account's proxy
    pub async fn unbind_account_proxy(&self, account_id: String) {
        self.account_bindings.remove(&account_id);

        // Persist to the config file
        self.persist_bindings().await;

        tracing::info!("[ProxyPool] Unbound account {}", account_id);
    }

    /// Get the proxy ID currently bound to an account
    pub fn get_account_binding(&self, account_id: &str) -> Option<String> {
        self.account_bindings
            .get(account_id)
            .map(|v| v.value().clone())
    }

    /// Get a snapshot of all bindings
    pub fn get_all_bindings_snapshot(&self) -> std::collections::HashMap<String, String> {
        self.account_bindings
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().clone()))
            .collect()
    }

    /// [HOT-RELOAD] Re-sync the in-memory DashMap from `config.account_bindings`.
    /// Called after `update_proxy_pool` so that a wholesale ProxyPoolConfig
    /// replacement (e.g. via `save_config`) does not leave the in-memory
    /// bindings stale or empty.
    pub async fn sync_bindings_from_config(&self) {
        let config = self.config.read().await;
        let snapshot = config.account_bindings.clone();
        drop(config);

        // Reset the DashMap: clear old entries, then insert fresh ones.
        self.account_bindings.clear();
        for (account_id, proxy_id) in &snapshot {
            self.account_bindings
                .insert(account_id.clone(), proxy_id.clone());
        }
        tracing::info!(
            "[ProxyPool] Re-synced {} account bindings from config (hot-reload)",
            snapshot.len()
        );
    }

    /// Persist the bindings to the config file
    async fn persist_bindings(&self) {
        // Get the current bindings snapshot
        let bindings = self.get_all_bindings_snapshot();

        // Update the bindings in the config
        {
            let mut config = self.config.write().await;
            config.account_bindings = bindings;
        }

        // Save to disk
        if let Ok(mut app_config) = crate::modules::config::load_app_config() {
            let config = self.config.read().await;
            app_config.proxy.proxy_pool = config.clone();
            if let Err(e) = crate::modules::config::save_app_config(&app_config) {
                tracing::error!("[ProxyPool] Failed to persist bindings: {}", e);
            }
        }
    }

    /// Batch-check proxy health status
    pub async fn health_check(&self) -> Result<(), String> {
        let proxies_to_check: Vec<ProxyEntry> = {
            let config = self.config.read().await;
            config
                .proxies
                .iter()
                .filter(|p| p.enabled)
                .cloned()
                .collect()
        };

        let concurrency_limit = 20usize;
        let results = stream::iter(proxies_to_check)
            .map(|proxy| async move {
                let (is_healthy, latency) = self.check_proxy_health(&proxy).await;

                let latency_msg = if let Some(ms) = latency {
                    format!("{}ms", ms)
                } else {
                    "-".to_string()
                };

                tracing::info!(
                    "Proxy {} ({}) health check: {} (Latency: {})",
                    proxy.name,
                    proxy.url,
                    if is_healthy { "✓ OK" } else { "✗ FAILED" },
                    latency_msg
                );

                (proxy.id, is_healthy, latency)
            })
            .buffer_unordered(concurrency_limit)
            .collect::<Vec<_>>()
            .await;

        // Update statuses uniformly
        let mut config = self.config.write().await;
        for (id, is_healthy, latency) in results {
            if let Some(proxy) = config.proxies.iter_mut().find(|p| p.id == id) {
                proxy.is_healthy = is_healthy;
                proxy.latency = latency;
                proxy.last_check_time = Some(chrono::Utc::now().timestamp());
            }
        }

        Ok(())
    }

    /// Check a single proxy's health status
    async fn check_proxy_health(&self, entry: &ProxyEntry) -> (bool, Option<u64>) {
        const DEFAULT_HEALTH_CHECK_URL: &str = "https://cp.cloudflare.com/generate_204";

        let check_url = if let Some(url) = &entry.health_check_url {
            if url.trim().is_empty() {
                DEFAULT_HEALTH_CHECK_URL
            } else {
                url.as_str()
            }
        } else {
            DEFAULT_HEALTH_CHECK_URL
        };

        // Try to build the Client; treat it as unhealthy immediately if it fails
        let proxy_res = self.build_proxy_config(entry);
        if let Err(e) = proxy_res {
            tracing::error!("Proxy {} build config failed: {}", entry.url, e);
            return (false, None);
        }
        let proxy_cfg = proxy_res.unwrap();

        let client_result = Client::builder()
            .proxy(proxy_cfg.proxy)
            .emulation(Emulation::Chrome123)
            .timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36")
            .build();

        let client = match client_result {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Proxy {} build client failed: {}", entry.url, e);
                return (false, None);
            }
        };

        let start = std::time::Instant::now();
        match client.get(check_url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as u64;
                if resp.status().is_success() {
                    (true, Some(latency))
                } else {
                    tracing::warn!(
                        "Proxy {} health check status error: {}",
                        entry.url,
                        resp.status()
                    );
                    (false, None)
                }
            }
            Err(e) => {
                tracing::warn!("Proxy {} health check request failed: {}", entry.url, e);
                (false, None)
            }
        }
    }

    /// Start the health check loop
    pub fn start_health_check_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            tracing::info!("Starting proxy pool health check loop...");
            loop {
                // Perform check only if enabled
                let enabled = self.config.read().await.enabled;
                if enabled {
                    if let Err(e) = self.health_check().await {
                        tracing::error!("Proxy pool health check failed: {}", e);
                    }
                }

                // Get interval and sleep AFTER check
                let interval_secs = {
                    let cfg = self.config.read().await;
                    if !cfg.enabled {
                        60 // check every minute if disabled
                    } else {
                        cfg.health_check_interval.max(30) // Back to default min 30s
                    }
                };

                tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::config::ProxyAuth;

    #[test]
    fn test_build_proxy_config_with_explicit_auth() {
        let pool = ProxyPoolManager::new(Arc::new(RwLock::new(ProxyPoolConfig::default())));
        let entry = ProxyEntry {
            id: "p1".to_string(),
            name: "test".to_string(),
            url: "http://127.0.0.1:8080".to_string(),
            auth: Some(ProxyAuth {
                username: "user".to_string(),
                password: "pass".to_string(),
            }),
            enabled: true,
            priority: 1,
            tags: vec![],
            max_accounts: None,
            health_check_url: None,
            last_check_time: None,
            is_healthy: true,
            latency: None,
        };

        let res = pool.build_proxy_config(&entry);
        assert!(res.is_ok());
        assert_eq!(res.unwrap().entry_id, "p1");
    }

    #[test]
    fn test_build_proxy_config_with_url_auth() {
        let pool = ProxyPoolManager::new(Arc::new(RwLock::new(ProxyPoolConfig::default())));
        let entry = ProxyEntry {
            id: "p2".to_string(),
            name: "test_url_auth".to_string(),
            url: "http://user:pass@127.0.0.1:10080".to_string(),
            auth: None,
            enabled: true,
            priority: 1,
            tags: vec![],
            max_accounts: None,
            health_check_url: None,
            last_check_time: None,
            is_healthy: true,
            latency: None,
        };

        let res = pool.build_proxy_config(&entry);
        assert!(res.is_ok());
        assert_eq!(res.unwrap().entry_id, "p2");
    }
}

