use serde::{Deserialize, Serialize};

/// Scheduling mode enum
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
        }
    }
}
