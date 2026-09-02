export interface UpstreamProxyConfig {
    enabled: boolean;
    url: string;
}

export interface ProxyConfig {
    enabled: boolean;
    allow_lan_access?: boolean;
    auth_mode?: 'off' | 'strict' | 'all_except_health' | 'auto';
    port: number;
    api_key: string;
    admin_password?: string;
    auto_start: boolean;
    custom_mapping?: Record<string, string>;
    request_timeout: number;
    enable_logging: boolean;
    debug_logging?: DebugLoggingConfig;
    upstream_proxy: UpstreamProxyConfig;
    zai?: ZaiConfig;
    scheduling?: StickySessionConfig;
    experimental?: ExperimentalConfig;
    user_agent_override?: string;
    saved_user_agent?: string;
    thinking_budget?: ThinkingBudgetConfig;
    global_system_prompt?: GlobalSystemPromptConfig;
    image_thinking_mode?: 'enabled' | 'disabled'; // [NEW] Image thinking mode toggle
    only_raw_quota_models?: boolean; // [NEW] Whether to only expose real quota models
    proxy_pool?: ProxyPoolConfig;
}

// ============================================================================
// Thinking Budget configuration (controls the token budget for AI deep reasoning)
// ============================================================================

/** Thinking Budget processing mode */
export type ThinkingBudgetMode = 'auto' | 'passthrough' | 'custom' | 'adaptive'; // [NEW] Supports adaptive mode

/** Thinking Effort level (adaptive mode only) */
export type ThinkingEffort = 'low' | 'medium' | 'high';

/** Thinking Budget configuration */
export interface ThinkingBudgetConfig {
    /** Mode selection */
    mode: ThinkingBudgetMode;
    /** Custom fixed value (only effective when mode=custom), range 1024-65536 */
    custom_value: number;
    /** Thinking intensity (only effective when mode=adaptive) */
    effort?: ThinkingEffort;
}

// ============================================================================
// Global system prompt configuration
// ============================================================================

/** Global system prompt configuration */
export interface GlobalSystemPromptConfig {
    /** Whether enabled */
    enabled: boolean;
    /** Prompt content */
    content: string;
}

export interface DebugLoggingConfig {
    enabled: boolean;
    output_dir?: string;
}

export type SchedulingMode = 'CacheFirst' | 'Balance' | 'PerformanceFirst';

export interface StickySessionConfig {
    mode: SchedulingMode;
    max_wait_seconds: number;
}

export type ZaiDispatchMode = 'off' | 'exclusive' | 'pooled' | 'fallback';

export interface ZaiMcpConfig {
    enabled: boolean;
    web_search_enabled: boolean;
    web_reader_enabled: boolean;
    vision_enabled: boolean;
}

export interface ZaiModelDefaults {
    opus: string;
    sonnet: string;
    haiku: string;
}

export interface ZaiConfig {
    enabled: boolean;
    base_url: string;
    api_key: string;
    dispatch_mode: ZaiDispatchMode;
    model_mapping?: Record<string, string>;
    models: ZaiModelDefaults;
    mcp: ZaiMcpConfig;
}

export interface ScheduledWarmupConfig {
    enabled: boolean;
    monitored_models: string[];
}

export interface QuotaProtectionConfig {
    enabled: boolean;
    threshold_percentage: number; // 1-99
    monitored_models: string[];
}

export interface PinnedQuotaModelsConfig {
    models: string[];
}

export interface ExperimentalConfig {
    enable_usage_scaling: boolean;
    compression_level?: string;
    context_compression_threshold_l1?: number;
    context_compression_threshold_l2?: number;
    context_compression_threshold_l3?: number;
}

export interface CircuitBreakerConfig {
    enabled: boolean;
    backoff_steps: number[];
}

export interface AppConfig {
    language: string;
    theme: string;
    auto_refresh: boolean;
    refresh_interval: number;
    auto_sync: boolean;
    sync_interval: number;
    default_export_path?: string;
    antigravity_executable?: string; // [NEW] Manually specified Antigravity program path
    antigravity_ide_executable?: string; // [NEW] Manually specified Antigravity IDE program path
    antigravity_cli_executable?: string; // [NEW] Manually specified Antigravity CLI (agy) path
    antigravity_args?: string[]; // [NEW] Antigravity launch arguments
    auto_launch?: boolean; // Launch at login
    auto_check_update?: boolean; // Automatically check for updates
    update_check_interval?: number; // Update check interval (hours)
    accounts_page_size?: number; // Number of items per page in the account list; default 0 means auto-calculate
    hidden_menu_items?: string[]; // List of hidden menu item paths
    scheduled_warmup: ScheduledWarmupConfig;
    quota_protection: QuotaProtectionConfig; // [NEW] Quota protection configuration
    pinned_quota_models: PinnedQuotaModelsConfig; // [NEW] Pinned quota watch list
    circuit_breaker: CircuitBreakerConfig; // [NEW] Circuit breaker configuration
    proxy: ProxyConfig;
    cloudflared: CloudflaredConfig; // [NEW] Cloudflared configuration
}

// ============================================================================
// Cloudflared (CF tunnel) type definitions
// ============================================================================

export type TunnelMode = 'quick' | 'auth';

export interface CloudflaredConfig {
    enabled: boolean;
    mode: TunnelMode;
    port: number;
    token?: string;
    use_http2: boolean;
}

export interface CloudflaredStatus {
    installed: boolean;
    version?: string;
    running: boolean;
    url?: string;
    error?: string;
}

// ============================================================================
// Proxy pool type definitions
// ============================================================================

export interface ProxyAuth {
    username: string;
    password?: string;
}

export interface ProxyEntry {
    id: string;
    name: string;
    url: string;
    auth?: ProxyAuth;
    enabled: boolean;
    priority: number;
    tags: string[];
    max_accounts?: number;
    health_check_url?: string;
    last_check_time?: number;
    is_healthy: boolean;
    latency?: number; // [NEW] Latency (ms)
}

// export type ProxyPoolMode = 'global' | 'per_account' | 'hybrid'; // [REMOVED]

export type ProxySelectionStrategy = 'round_robin' | 'random' | 'priority' | 'least_connections' | 'weighted_round_robin';

export interface ProxyPoolConfig {
    enabled: boolean;
    // mode: ProxyPoolMode; // [REMOVED]
    proxies: ProxyEntry[];
    health_check_interval: number;
    auto_failover: boolean;
    strategy: ProxySelectionStrategy;
    account_bindings?: Record<string, string>;
}
