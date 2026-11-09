use serde::{Deserialize, Serialize};

/// Scheduling mode enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedulingMode {
    /// Cache-first: lock onto the same account whenever possible, prefer waiting on rate limits, greatly improves Prompt Caching hit rate
    CacheFirst,
    /// Balance: lock onto the same account, switch to a backup account immediately on rate limit, balances success rate and performance
    Balance,
    /// Performance-first: pure round-robin mode, most even account load, but does not use caching
    PerformanceFirst,
}

impl Default for SchedulingMode {
    fn default() -> Self {
        Self::Balance
    }
}

/// Sticky binding dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffinityScope {
    /// Bind by session id. Conversations stay isolated and a single session keeps its cache.
    Session,
    /// Bind by client API key. Stops multi-agent transient session ids from missing the prompt cache.
    ApiKey,
}

impl Default for AffinityScope {
    fn default() -> Self {
        Self::Session
    }
}

/// Sticky session config
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StickySessionConfig {
    /// Current scheduling mode
    pub mode: SchedulingMode,
    /// Max wait time in cache-first mode (seconds)
    pub max_wait_seconds: u64,
    /// 并发分流使用的账号上限 (0 表示不限制，>0 时最多分流至该数量的账号)
    #[serde(default = "default_max_spillover_accounts")]
    pub max_spillover_accounts: usize,
    /// 粘性路由绑定作用域（默认 Session 会话级）
    #[serde(default)]
    pub affinity_scope: AffinityScope,
}

impl StickySessionConfig {
    /// 获取实际生效的粘性作用域（优先从环境变量读取覆盖，对齐 Headless & CLI 规范）
    pub fn resolve_affinity_scope(&self) -> AffinityScope {
        if let Ok(val) = std::env::var("ANTIGRAVITY_SCHEDULING_AFFINITY_SCOPE")
            .or_else(|_| std::env::var("AGY_SCHEDULING_AFFINITY_SCOPE"))
        {
            let val = val.trim().to_ascii_lowercase();
            if val == "api_key" || val == "apikey" || val == "key" {
                return AffinityScope::ApiKey;
            } else if val == "session" || val == "session_id" {
                return AffinityScope::Session;
            }
        }
        self.affinity_scope
    }
}

/// 全局获取当前生效的粘性绑定维度（支持环境变量覆盖，保证与 Headless/CLI 一致）
pub fn get_affinity_scope() -> AffinityScope {
    if let Ok(val) = std::env::var("ANTIGRAVITY_SCHEDULING_AFFINITY_SCOPE")
        .or_else(|_| std::env::var("AGY_SCHEDULING_AFFINITY_SCOPE"))
    {
        let val = val.trim().to_ascii_lowercase();
        if val == "api_key" || val == "apikey" || val == "key" {
            return AffinityScope::ApiKey;
        } else if val == "session" || val == "session_id" {
            return AffinityScope::Session;
        }
    }

    crate::modules::config::load_app_config()
        .map(|cfg| cfg.proxy.scheduling.affinity_scope)
        .unwrap_or_default()
}

fn default_max_spillover_accounts() -> usize {
    0
}

impl Default for StickySessionConfig {
    fn default() -> Self {
        Self {
            mode: SchedulingMode::Balance,
            max_wait_seconds: 60,
            max_spillover_accounts: 0,
            affinity_scope: AffinityScope::Session,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sticky_session_config_default() {
        let cfg = StickySessionConfig::default();
        assert_eq!(cfg.mode, SchedulingMode::Balance);
        assert_eq!(cfg.affinity_scope, AffinityScope::Session);
        assert_eq!(cfg.resolve_affinity_scope(), AffinityScope::Session);
    }

    #[test]
    fn test_sticky_session_config_deserialize_defaults() {
        let json_str = r#"{"mode": "CacheFirst", "max_wait_seconds": 90}"#;
        let cfg: StickySessionConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(cfg.mode, SchedulingMode::CacheFirst);
        assert_eq!(cfg.max_wait_seconds, 90);
        assert_eq!(cfg.affinity_scope, AffinityScope::Session);
    }

    #[test]
    fn test_sticky_session_config_deserialize_api_key() {
        let json_str = r#"{"affinity_scope": "api_key"}"#;
        let cfg: StickySessionConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(cfg.affinity_scope, AffinityScope::ApiKey);
    }
}
