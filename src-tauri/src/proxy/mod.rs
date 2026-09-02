// proxy module - API reverse proxy service

// Existing modules (retained)
pub mod config;
pub mod project_resolver;
pub mod security;
pub mod server;
pub mod token_manager;

// New architecture modules
pub mod audio; // Audio processing module
pub mod cache_manager; // Context Cache management (prefix hash -> cache_id mapping)
pub mod cli_sync; // CLI config sync (v3.3.35)
pub mod common; // Common utilities
pub mod debug_logger;
pub mod droid_sync; // Droid (Factory CLI) config sync
pub mod handlers; // API endpoint handlers
pub mod http_session_store; // HTTP multi-turn conversation session history store
pub mod mappers; // Protocol converters
pub mod middleware; // Axum middleware
pub mod model_specs; // Model spec management (v4.1.29)
pub mod monitor; // Monitoring
pub mod opencode_sync; // OpenCode config sync
pub mod providers; // Extra upstream providers (z.ai, etc.)
pub mod proxy_pool; // Proxy pool manager
pub mod rate_limit; // Rate limit tracking
pub mod session_manager; // Session fingerprint management
pub mod signature_cache; // Signature Cache (v3.3.16)
pub mod sticky_config; // Sticky scheduling config
pub mod upstream; // Upstream client
pub mod zai_vision_mcp; // Built-in Vision MCP server state
pub mod zai_vision_tools; // Built-in Vision MCP tools (z.ai vision API) // Debug logging

pub use config::update_global_system_prompt_config;
pub use config::update_image_thinking_mode;
pub use config::update_thinking_budget_config;
pub use config::ProxyAuthMode;
pub use config::ProxyConfig;
pub use config::ProxyPoolConfig;
pub use config::ZaiConfig;
pub use config::ZaiDispatchMode;
pub use security::ProxySecurityConfig;
pub use server::AxumServer;
pub use signature_cache::SignatureCache;
pub use token_manager::TokenManager;

#[cfg(test)]
pub mod tests;

pub mod adapters;
