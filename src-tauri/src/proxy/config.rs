use serde::{Deserialize, Serialize};
// use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

// ============================================================================
// Helper utility functions
// ============================================================================

/// Normalize a proxy URL, defaulting to http:// if the scheme is missing
pub fn normalize_proxy_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    if !url.contains("://") {
        format!("http://{}", url)
    } else {
        url.to_string()
    }
}

// ============================================================================
// Global Thinking Budget config storage
// Used to access the config from within request transform functions (without changing their signatures)
// ============================================================================
static GLOBAL_THINKING_BUDGET_CONFIG: OnceLock<RwLock<ThinkingBudgetConfig>> = OnceLock::new();

/// Get the current Thinking Budget config
pub fn get_thinking_budget_config() -> ThinkingBudgetConfig {
    GLOBAL_THINKING_BUDGET_CONFIG
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|cfg| cfg.clone())
        .unwrap_or_default()
}

/// Update the global Thinking Budget config
pub fn update_thinking_budget_config(config: ThinkingBudgetConfig) {
    if let Some(lock) = GLOBAL_THINKING_BUDGET_CONFIG.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = config.clone();
            tracing::info!(
                "[Thinking-Budget] Global config updated: mode={:?}, custom_value={}",
                config.mode,
                config.custom_value
            );
        }
    } else {
        // First-time initialization
        let _ = GLOBAL_THINKING_BUDGET_CONFIG.set(RwLock::new(config.clone()));
        tracing::info!(
            "[Thinking-Budget] Global config initialized: mode={:?}, custom_value={}",
            config.mode,
            config.custom_value
        );
    }
}

// ============================================================================
// Global system prompt config storage
// Users can configure a global prompt in settings, automatically injected into every request's systemInstruction
// ============================================================================
static GLOBAL_SYSTEM_PROMPT_CONFIG: OnceLock<RwLock<GlobalSystemPromptConfig>> = OnceLock::new();

/// Get the current global system prompt config
pub fn get_global_system_prompt() -> GlobalSystemPromptConfig {
    GLOBAL_SYSTEM_PROMPT_CONFIG
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|cfg| cfg.clone())
        .unwrap_or_default()
}

/// Update the global system prompt config
pub fn update_global_system_prompt_config(config: GlobalSystemPromptConfig) {
    if let Some(lock) = GLOBAL_SYSTEM_PROMPT_CONFIG.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = config.clone();
            tracing::info!(
                "[Global-System-Prompt] Config updated: enabled={}, content_len={}",
                config.enabled,
                config.content.len()
            );
        }
    } else {
        // First-time initialization
        let _ = GLOBAL_SYSTEM_PROMPT_CONFIG.set(RwLock::new(config.clone()));
        tracing::info!(
            "[Global-System-Prompt] Config initialized: enabled={}, content_len={}",
            config.enabled,
            config.content.len()
        );
    }
}

// ============================================================================
// Global image thinking mode config storage
// ============================================================================
static GLOBAL_IMAGE_THINKING_MODE: OnceLock<RwLock<String>> = OnceLock::new();

pub fn get_image_thinking_mode() -> String {
    GLOBAL_IMAGE_THINKING_MODE
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|s| s.clone())
        .unwrap_or_else(|| "enabled".to_string())
}

pub fn update_image_thinking_mode(mode: Option<String>) {
    let val = mode.unwrap_or_else(|| "enabled".to_string());
    if let Some(lock) = GLOBAL_IMAGE_THINKING_MODE.get() {
        if let Ok(mut cfg) = lock.write() {
            if *cfg != val {
                *cfg = val.clone();
                tracing::info!("[Image-Thinking] Global config updated: {}", val);
            }
        }
    } else {
        let _ = GLOBAL_IMAGE_THINKING_MODE.set(RwLock::new(val.clone()));
    }
}

// ============================================================================
// Global compression level config storage
// ============================================================================
static GLOBAL_COMPRESSION_LEVEL: OnceLock<RwLock<String>> = OnceLock::new();
static GLOBAL_USAGE_SCALING: OnceLock<RwLock<bool>> = OnceLock::new();
static GLOBAL_THRESHOLD_L1: OnceLock<RwLock<f32>> = OnceLock::new();
static GLOBAL_THRESHOLD_L2: OnceLock<RwLock<f32>> = OnceLock::new();
static GLOBAL_THRESHOLD_L3: OnceLock<RwLock<f32>> = OnceLock::new();

pub fn get_global_threshold_l1() -> f32 {
    GLOBAL_THRESHOLD_L1
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|v| *v)
        .unwrap_or(0.6)
}

pub fn get_global_threshold_l2() -> f32 {
    GLOBAL_THRESHOLD_L2
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|v| *v)
        .unwrap_or(0.75)
}

pub fn get_global_threshold_l3() -> f32 {
    GLOBAL_THRESHOLD_L3
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|v| *v)
        .unwrap_or(0.9)
}

pub fn get_global_compression_level() -> String {
    let level = GLOBAL_COMPRESSION_LEVEL
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|cfg| cfg.clone())
        .unwrap_or_else(|| "disabled".to_string());

    if level == "disabled" {
        let scaling = GLOBAL_USAGE_SCALING
            .get()
            .and_then(|lock| lock.read().ok())
            .map(|s| *s)
            .unwrap_or(false);
        if scaling {
            "high".to_string()
        } else {
            "disabled".to_string()
        }
    } else {
        level
    }
}

pub fn update_global_compression_level(level: String, scaling: bool) {
    if let Some(lock) = GLOBAL_COMPRESSION_LEVEL.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = level;
        }
    } else {
        let _ = GLOBAL_COMPRESSION_LEVEL.set(RwLock::new(level));
    }

    if let Some(lock) = GLOBAL_USAGE_SCALING.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = scaling;
        }
    } else {
        let _ = GLOBAL_USAGE_SCALING.set(RwLock::new(scaling));
    }
}

pub fn update_global_thresholds(l1: f32, l2: f32, l3: f32) {
    if let Some(lock) = GLOBAL_THRESHOLD_L1.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = l1;
        }
    } else {
        let _ = GLOBAL_THRESHOLD_L1.set(RwLock::new(l1));
    }

    if let Some(lock) = GLOBAL_THRESHOLD_L2.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = l2;
        }
    } else {
        let _ = GLOBAL_THRESHOLD_L2.set(RwLock::new(l2));
    }

    if let Some(lock) = GLOBAL_THRESHOLD_L3.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = l3;
        }
    } else {
        let _ = GLOBAL_THRESHOLD_L3.set(RwLock::new(l3));
    }
}

/// Global system prompt config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSystemPromptConfig {
    /// Whether the global system prompt is enabled
    #[serde(default)]
    pub enabled: bool,
    /// System prompt content
    #[serde(default)]
    pub content: String,
}

impl Default for GlobalSystemPromptConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            content: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyAuthMode {
    Off,
    Strict,
    AllExceptHealth,
    Auto,
}

impl Default for ProxyAuthMode {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ZaiDispatchMode {
    /// Never use z.ai.
    Off,
    /// Use z.ai for all Anthropic protocol requests.
    Exclusive,
    /// Treat z.ai as one additional slot in the shared pool.
    Pooled,
    /// Use z.ai only when the Google pool is unavailable.
    Fallback,
}

impl Default for ZaiDispatchMode {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaiModelDefaults {
    /// Default model for "opus" family (when the incoming model is a Claude id).
    #[serde(default = "default_zai_opus_model")]
    pub opus: String,
    /// Default model for "sonnet" family (when the incoming model is a Claude id).
    #[serde(default = "default_zai_sonnet_model")]
    pub sonnet: String,
    /// Default model for "haiku" family (when the incoming model is a Claude id).
    #[serde(default = "default_zai_haiku_model")]
    pub haiku: String,
}

impl Default for ZaiModelDefaults {
    fn default() -> Self {
        Self {
            opus: default_zai_opus_model(),
            sonnet: default_zai_sonnet_model(),
            haiku: default_zai_haiku_model(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaiMcpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub web_search_enabled: bool,
    #[serde(default)]
    pub web_reader_enabled: bool,
    #[serde(default)]
    pub vision_enabled: bool,
}

impl Default for ZaiMcpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            web_search_enabled: false,
            web_reader_enabled: false,
            vision_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_zai_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub dispatch_mode: ZaiDispatchMode,
    /// Optional per-model mapping overrides for Anthropic/Claude model ids.
    /// Key: incoming `model` string, Value: upstream z.ai model id (e.g. `glm-4.7`).
    #[serde(default)]
    pub model_mapping: HashMap<String, String>,
    #[serde(default)]
    pub models: ZaiModelDefaults,
    #[serde(default)]
    pub mcp: ZaiMcpConfig,
}

impl Default for ZaiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_zai_base_url(),
            api_key: String::new(),
            dispatch_mode: ZaiDispatchMode::Off,
            model_mapping: HashMap::new(),
            models: ZaiModelDefaults::default(),
            mcp: ZaiMcpConfig::default(),
        }
    }
}

/// Experimental feature config (Feature Flags)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentalConfig {
    /// Enable the two-tier signature cache (Signature Cache)
    #[serde(default = "default_true")]
    pub enable_signature_cache: bool,

    /// Enable automatic tool loop recovery (Tool Loop Recovery)
    #[serde(default = "default_true")]
    pub enable_tool_loop_recovery: bool,

    /// Enable cross-model compatibility checks (Cross-Model Checks)
    #[serde(default = "default_true")]
    pub enable_cross_model_checks: bool,

    /// Enable context usage scaling (Context Usage Scaling)
    /// Aggressive mode: scales usage and activates auto compaction to break past the 200k limit
    /// Off by default to preserve transparency, letting the client trigger native compaction instructions
    #[serde(default = "default_false")]
    pub enable_usage_scaling: bool,

    /// Compression level (Compression Level)
    /// disabled, low, medium, high
    #[serde(default = "default_compression_level")]
    pub compression_level: String,

    /// Context compression threshold L1 (Tool Trimming)
    #[serde(default = "default_threshold_l1")]
    pub context_compression_threshold_l1: f32,

    /// Context compression threshold L2 (Thinking Compression)
    #[serde(default = "default_threshold_l2")]
    pub context_compression_threshold_l2: f32,

    /// Context compression threshold L3 (Fork + Summary)
    #[serde(default = "default_threshold_l3")]
    pub context_compression_threshold_l3: f32,
}

impl Default for ExperimentalConfig {
    fn default() -> Self {
        Self {
            enable_signature_cache: true,
            enable_tool_loop_recovery: true,
            enable_cross_model_checks: true,
            enable_usage_scaling: false,
            compression_level: "disabled".to_string(),
            context_compression_threshold_l1: 0.4,
            context_compression_threshold_l2: 0.55,
            context_compression_threshold_l3: 0.7,
        }
    }
}

fn default_threshold_l1() -> f32 {
    0.4
}
fn default_threshold_l2() -> f32 {
    0.55
}
fn default_threshold_l3() -> f32 {
    0.7
}
fn default_compression_level() -> String {
    "disabled".to_string()
}

/// Thinking Budget mode
/// Controls how the thinking_budget parameter passed in by the caller is handled
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingBudgetMode {
    /// Auto limit: applies a 24576 cap to specific models (Flash/Thinking)
    Auto,
    /// Passthrough: use the caller-supplied value as-is, with no modification
    Passthrough,
    /// Custom: override all requests with a user-set fixed value
    Custom,
    /// Adaptive: use the effort parameter to control thinking intensity (Claude 4.6+)
    Adaptive,
}

impl Default for ThinkingBudgetMode {
    fn default() -> Self {
        Self::Auto
    }
}

/// Thinking Budget config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBudgetConfig {
    /// Mode selection
    #[serde(default)]
    pub mode: ThinkingBudgetMode,
    /// Custom fixed value (only takes effect when mode=Custom)
    #[serde(default = "default_thinking_budget_custom_value")]
    pub custom_value: u32,
    /// Thinking intensity (only takes effect when mode=Adaptive): low, medium, high
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Default for ThinkingBudgetConfig {
    fn default() -> Self {
        Self {
            mode: ThinkingBudgetMode::Auto,
            custom_value: default_thinking_budget_custom_value(),
            effort: None,
        }
    }
}

fn default_thinking_budget_custom_value() -> u32 {
    24576
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugLoggingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub output_dir: Option<String>,
}

impl Default for DebugLoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            output_dir: None,
        }
    }
}

/// IP blocklist config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpBlacklistConfig {
    /// Whether the blocklist is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Custom ban message
    #[serde(default = "default_block_message")]
    pub block_message: String,
}

impl Default for IpBlacklistConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            block_message: default_block_message(),
        }
    }
}

fn default_block_message() -> String {
    "Access denied".to_string()
}

/// IP allowlist config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpWhitelistConfig {
    /// Whether allowlist mode is enabled (once enabled, only allowlisted IPs can access)
    #[serde(default)]
    pub enabled: bool,

    /// Allowlist-priority mode (allowlisted IPs skip the blocklist check)
    #[serde(default = "default_true")]
    pub whitelist_priority: bool,
}

impl Default for IpWhitelistConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            whitelist_priority: true,
        }
    }
}

/// Security monitor config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityMonitorConfig {
    /// IP blocklist config
    #[serde(default)]
    pub blacklist: IpBlacklistConfig,

    /// IP allowlist config
    #[serde(default)]
    pub whitelist: IpWhitelistConfig,
}

impl Default for SecurityMonitorConfig {
    fn default() -> Self {
        Self {
            blacklist: IpBlacklistConfig::default(),
            whitelist: IpWhitelistConfig::default(),
        }
    }
}

/// Image task scheduling config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSchedulerConfig {
    #[serde(default = "default_image_per_account_concurrency")]
    pub per_account_concurrency: usize,
}

impl Default for ImageSchedulerConfig {
    fn default() -> Self {
        Self {
            per_account_concurrency: default_image_per_account_concurrency(),
        }
    }
}

fn default_image_per_account_concurrency() -> usize {
    4
}

/// Reverse proxy service config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Whether the reverse proxy service is enabled
    pub enabled: bool,

    /// Whether LAN access is allowed
    /// - false: local access only, 127.0.0.1 (default, privacy-first)
    /// - true: allow LAN access, 0.0.0.0
    #[serde(default)]
    pub allow_lan_access: bool,

    /// Authorization policy for the proxy.
    /// - off: no auth required
    /// - strict: auth required for all routes
    /// - all_except_health: auth required for all routes except `/healthz`
    /// - auto: recommended defaults (currently: allow_lan_access => all_except_health, else off)
    #[serde(default)]
    pub auth_mode: ProxyAuthMode,

    /// Listening port
    pub port: u16,

    /// API key
    pub api_key: String,

    /// Web UI admin panel password (optional; falls back to api_key if unset)
    pub admin_password: Option<String>,

    /// Whether to auto-start
    pub auto_start: bool,

    /// Custom exact model mapping table (key: original model name, value: target model name)
    #[serde(default)]
    pub custom_mapping: std::collections::HashMap<String, String>,

    /// API request timeout (seconds)
    #[serde(default = "default_request_timeout")]
    pub request_timeout: u64,

    /// Whether request logging is enabled (monitoring)
    #[serde(default)]
    pub enable_logging: bool,

    /// Debug logging config (saves the full trace)
    #[serde(default)]
    pub debug_logging: DebugLoggingConfig,

    /// Upstream proxy config
    #[serde(default)]
    pub upstream_proxy: UpstreamProxyConfig,

    /// Whether to expose only real quota models in /v1/models (hiding the built-in virtual aliases)
    #[serde(default)]
    pub only_raw_quota_models: bool,

    /// z.ai provider configuration (Anthropic-compatible).
    #[serde(default)]
    pub zai: ZaiConfig,

    /// Custom User-Agent header (optional override)
    #[serde(default)]
    pub user_agent_override: Option<String>,

    /// Account scheduling config (sticky sessions / rate limit retry)
    #[serde(default)]
    pub scheduling: crate::proxy::sticky_config::StickySessionConfig,

    /// Experimental feature config
    #[serde(default)]
    pub experimental: ExperimentalConfig,

    /// Security monitor config (IP allowlist/blocklist)
    #[serde(default)]
    pub security_monitor: SecurityMonitorConfig,

    /// Account ID for Fixed Account Mode
    /// - None: use round-robin mode
    /// - Some(account_id): always use the specified account
    #[serde(default)]
    pub preferred_account_id: Option<String>,

    /// Saved User-Agent string (persisted even when override is disabled)
    #[serde(default)]
    pub saved_user_agent: Option<String>,

    /// Thinking Budget config
    /// Controls how the token budget for AI deep thinking is handled
    #[serde(default)]
    pub thinking_budget: ThinkingBudgetConfig,

    /// Global system prompt config
    /// Automatically injected into every API request's systemInstruction
    #[serde(default)]
    pub global_system_prompt: GlobalSystemPromptConfig,

    /// Image thinking mode config
    /// - enabled: keep the thinking chain (default)
    /// - disabled: remove the thinking chain (image quality first)
    #[serde(default)]
    pub image_thinking_mode: Option<String>,

    /// Per-account concurrency for image upstream tasks (takes effect after restart)
    #[serde(default)]
    pub image_scheduler: ImageSchedulerConfig,

    /// Proxy pool config
    #[serde(default)]
    pub proxy_pool: ProxyPoolConfig,
}

/// Upstream proxy config
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpstreamProxyConfig {
    /// Whether it's enabled
    pub enabled: bool,
    /// Proxy address (http://, https://, socks5://)
    pub url: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_lan_access: false, // Local access only by default, privacy-first
            auth_mode: ProxyAuthMode::default(),
            port: 8045,
            api_key: format!("sk-{}", uuid::Uuid::new_v4().simple()),
            admin_password: None,
            auto_start: false,
            custom_mapping: std::collections::HashMap::new(),
            request_timeout: default_request_timeout(),
            enable_logging: true, // Enabled by default, supports token usage stats
            debug_logging: DebugLoggingConfig::default(),
            upstream_proxy: UpstreamProxyConfig::default(),
            only_raw_quota_models: false,
            zai: ZaiConfig::default(),
            scheduling: crate::proxy::sticky_config::StickySessionConfig::default(),
            experimental: ExperimentalConfig::default(),
            security_monitor: SecurityMonitorConfig::default(),
            preferred_account_id: None, // Round-robin mode by default
            user_agent_override: None,
            saved_user_agent: None,
            thinking_budget: ThinkingBudgetConfig::default(),
            global_system_prompt: GlobalSystemPromptConfig::default(),
            proxy_pool: ProxyPoolConfig::default(),
            image_thinking_mode: None,
            image_scheduler: ImageSchedulerConfig::default(),
        }
    }
}

fn default_request_timeout() -> u64 {
    120 // 120 seconds by default; the original 60 seconds was too short
}

fn default_zai_base_url() -> String {
    "https://api.z.ai/api/anthropic".to_string()
}

fn default_zai_opus_model() -> String {
    "glm-4.7".to_string()
}

fn default_zai_sonnet_model() -> String {
    "glm-4.7".to_string()
}

fn default_zai_haiku_model() -> String {
    "glm-4.5-air".to_string()
}

impl ProxyConfig {
    /// Get the actual bind address
    /// - allow_lan_access = false: returns "127.0.0.1" (default, privacy-first)
    /// - allow_lan_access = true: returns "0.0.0.0" (allows LAN access)
    pub fn get_bind_address(&self) -> &str {
        if self.allow_lan_access {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        }
    }
}

/// Proxy auth info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyAuth {
    pub username: String,
    #[serde(
        serialize_with = "crate::utils::crypto::serialize_password",
        deserialize_with = "crate::utils::crypto::deserialize_password"
    )]
    pub password: String,
}

/// A single proxy entry config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyEntry {
    pub id: String,                       // Unique identifier
    pub name: String,                     // Display name
    pub url: String,                      // Proxy address (http://, https://, socks5://)
    pub auth: Option<ProxyAuth>,          // Auth info (optional)
    pub enabled: bool,                    // Whether it's enabled
    pub priority: i32,                    // Priority (lower number = higher priority)
    pub tags: Vec<String>,                // Tags (e.g. "US", "Residential IP")
    pub max_accounts: Option<usize>,      // Max bound accounts (0 = unlimited)
    pub health_check_url: Option<String>, // Health check URL
    pub last_check_time: Option<i64>,     // Last check time
    pub is_healthy: bool,                 // Health status
    pub latency: Option<u64>,             // Latency (ms) [NEW]
}

/// Proxy pool config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPoolConfig {
    pub enabled: bool, // Whether the proxy pool is enabled
    // pub mode: ProxyPoolMode,        // [REMOVED] proxy pool mode, unified into Hybrid logic
    pub proxies: Vec<ProxyEntry>,         // Proxy list
    pub health_check_interval: u64,       // Health check interval (seconds)
    pub auto_failover: bool,              // Auto failover
    pub strategy: ProxySelectionStrategy, // Proxy selection strategy
    /// Account-to-proxy binding relationships (account_id -> proxy_id), persisted storage
    #[serde(default)]
    pub account_bindings: HashMap<String, String>,
}

impl Default for ProxyPoolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            // mode: ProxyPoolMode::Global,
            proxies: Vec::new(),
            health_check_interval: 300,
            auto_failover: true,
            strategy: ProxySelectionStrategy::Priority,
            account_bindings: HashMap::new(),
        }
    }
}

/// Proxy selection strategy
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProxySelectionStrategy {
    /// Round-robin: use each proxy in turn
    RoundRobin,
    /// Random: pick a random proxy
    Random,
    /// Priority: sort by the priority field
    Priority,
    /// Least connections: pick the currently least-used proxy
    LeastConnections,
    /// Weighted round-robin: based on health status and priority
    WeightedRoundRobin,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_proxy_url() {
        // Test a URL that already has a scheme
        assert_eq!(
            normalize_proxy_url("http://127.0.0.1:7890"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_proxy_url("https://proxy.com"),
            "https://proxy.com"
        );
        assert_eq!(
            normalize_proxy_url("socks5://127.0.0.1:1080"),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(
            normalize_proxy_url("socks5h://127.0.0.1:1080"),
            "socks5h://127.0.0.1:1080"
        );

        // Test a URL missing a scheme (defaults to filling in http://)
        assert_eq!(
            normalize_proxy_url("127.0.0.1:7890"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_proxy_url("localhost:1082"),
            "http://localhost:1082"
        );

        // Test edge cases
        assert_eq!(normalize_proxy_url(""), "");
        assert_eq!(normalize_proxy_url("   "), "");
    }
}
