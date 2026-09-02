//! Context Cache Manager — Multi-Layer Split Cache
//!
//! Maintains three independent cache layers, following Antigravity SignatureCache's multi-tier pattern.
//! Each layer has its own TTL, capacity, LRU eviction, and stats - a change in one layer does not affect the hit rate of the others.
//!
//! Architecture:
//!   Layer 1 (SI Cache):        raw_instructions -> sanitized_text
//!     Cross-session reuse: different conversations with the same system prompt share the sanitized result
//!   Layer 2 (Tools Cache):     raw_tools_json -> processed_tools
//!     Cross-session reuse: different conversations with the same tool schema share the processed result
//!   Layer 3 (Prefix Tracker):  combined_key -> PrefixTrackingEntry
//!     Tracks the lifecycle of the (Layer1 + Layer2) combination, used for cachedContent injection and hit stats
//!
//! Cache lifecycle:
//! 1. Request arrives -> compute Layer 1 key -> look up or sanitize systemInstruction
//! 2. Compute Layer 2 key -> look up or build tools
//! 3. Compute Layer 3 key -> check whether a cache_name already exists -> inject cachedContent
//! 4. Response arrives -> if cachedContentTokenCount > 0 -> update Layer 3 stats
//! 5. Expired entries are passively evicted on insert (LRU on limit breach)

use dashmap::DashMap;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

// ===== Layer Limits (following SignatureCache pattern) =====
const SI_CACHE_LIMIT: usize = 200;
const TOOLS_CACHE_LIMIT: usize = 100;
const PREFIX_TRACKER_LIMIT: usize = 500;

// ===== TTL Constants =====
/// Layer 1 & 2 TTL: 30 min - longer than the final content cache, since it does not vary by session
const LAYER_12_TTL: Duration = Duration::from_secs(30 * 60);
/// Layer 3 TTL: 1 hour - aligned with Gemini's default explicit-cache TTL
const LAYER_3_TTL: Duration = Duration::from_secs(3600);

// ===== Layer 1: System Instruction Cache =====

/// Layer 1 entry: raw system instructions -> sanitized text
#[derive(Debug, Clone)]
struct SiCacheEntry {
    /// Sanitized system instruction text
    sanitized_text: String,
    /// Creation/update time
    timestamp: Instant,
    /// Hit count
    hit_count: u64,
}

// ===== Layer 2: Tools Cache =====

/// Layer 2 entry: raw tools JSON -> processed tools
#[derive(Debug, Clone)]
struct ToolsCacheEntry {
    /// Processed tools JSON (serialized as a string, deserialized when used)
    tools_json: String,
    /// Creation/update time
    timestamp: Instant,
    /// Hit count
    hit_count: u64,
}

// ===== Layer 3: Prefix Tracker =====

/// Layer 3 entry: tracks the (Layer1_hash + Layer2_hash) combination
#[derive(Debug, Clone)]
struct PrefixTrackingEntry {
    /// Layer 1's hash (used for association)
    si_hash: String,
    /// Layer 2's hash (used for association)
    tools_hash: String,
    /// Gemini's cache resource name (cachedContents/xxx)
    cache_name: String,
    /// Creation time
    created_at: Instant,
    /// Expiration time
    expires_at: Instant,
    /// Implicit cache hit count (cachedContentTokenCount > 0)
    implicit_hit_count: u64,
    /// Explicit cache hit count (cachedContent successfully injected)
    explicit_hit_count: u64,
    /// Model name
    model: String,
}

// ===== Stats =====

/// Per-layer statistics
#[derive(Debug, Clone, Default)]
pub struct LayerStats {
    /// Layer 1: SI cache stats
    pub si_total: u64,
    pub si_hits: u64,
    pub si_misses: u64,
    /// Layer 2: Tools cache stats
    pub tools_total: u64,
    pub tools_hits: u64,
    pub tools_misses: u64,
    /// Layer 3: Prefix tracker stats
    pub prefix_total: u64,
    pub prefix_hits: u64,
    pub prefix_misses: u64,
    /// Total implicit cache hit count
    pub total_implicit_hits: u64,
    /// Total explicit cache hit count
    pub total_explicit_hits: u64,
    /// Current number of active entries
    pub active_si_entries: usize,
    pub active_tools_entries: usize,
    pub active_prefix_entries: usize,
}

// ===== CacheManager =====

/// Context Cache Manager - multi-tier cache singleton
pub struct CacheManager {
    /// Layer 1: raw SI hash -> sanitized text
    si_cache: DashMap<String, SiCacheEntry>,
    /// Layer 1 stats
    si_stats: std::sync::RwLock<(u64, u64, u64)>, // (total, hits, misses)

    /// Layer 2: raw tools hash -> processed tools JSON
    tools_cache: DashMap<String, ToolsCacheEntry>,
    /// Layer 2 stats
    tools_stats: std::sync::RwLock<(u64, u64, u64)>,

    /// Layer 3: combined hash -> tracking entry
    prefix_tracker: DashMap<String, PrefixTrackingEntry>,
    /// Layer 3 stats
    prefix_stats: std::sync::RwLock<(u64, u64, u64)>,
}

impl CacheManager {
    /// Create a new CacheManager
    pub fn new() -> Self {
        Self {
            si_cache: DashMap::new(),
            si_stats: std::sync::RwLock::new((0, 0, 0)),
            tools_cache: DashMap::new(),
            tools_stats: std::sync::RwLock::new((0, 0, 0)),
            prefix_tracker: DashMap::new(),
            prefix_stats: std::sync::RwLock::new((0, 0, 0)),
        }
    }

    // ===== Shared Utilities =====

    /// Quick SHA256 hash
    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    /// Check whether an Instant has expired
    fn is_expired(timestamp: Instant, ttl: Duration) -> bool {
        timestamp.elapsed() > ttl
    }

    // ===== Layer 1: System Instruction Cache =====

    /// Look up the cached sanitized system instruction
    ///
    /// Returns Some(sanitized_text) if the cache is valid, otherwise None
    pub fn lookup_si(&self, raw_hash: &str) -> Option<String> {
        if let Ok(mut stats) = self.si_stats.write() {
            stats.0 += 1; // total
        }

        let mut hit = false;
        let mut expired = false;
        let mut text = None;

        if let Some(entry) = self.si_cache.get(raw_hash) {
            if !Self::is_expired(entry.timestamp, LAYER_12_TTL) {
                hit = true;
                text = Some(entry.sanitized_text.clone());
            } else {
                expired = true;
            }
        }

        if hit {
            if let Ok(mut stats) = self.si_stats.write() {
                stats.1 += 1; // hits
            }
            let mut hit_count = 0;
            if let Some(mut e) = self.si_cache.get_mut(raw_hash) {
                e.hit_count += 1;
                hit_count = e.hit_count;
            }
            tracing::debug!(
                "[CacheManager:L1-SI] HIT hash={} hit_count={}",
                &raw_hash[..raw_hash.len().min(16)],
                hit_count
            );
            return text;
        }

        if expired {
            self.si_cache.remove_if(raw_hash, |_, entry| {
                Self::is_expired(entry.timestamp, LAYER_12_TTL)
            });
            if let Ok(mut stats) = self.si_stats.write() {
                stats.2 += 1; // misses
            }
            tracing::debug!(
                "[CacheManager:L1-SI] EXPIRED hash={}",
                &raw_hash[..raw_hash.len().min(16)]
            );
            return None;
        }

        if let Ok(mut stats) = self.si_stats.write() {
            stats.2 += 1; // misses
        }
        tracing::debug!(
            "[CacheManager:L1-SI] MISS hash={}",
            &raw_hash[..raw_hash.len().min(16)]
        );
        None
    }

    /// Store into Layer 1: raw -> sanitized
    pub fn cache_si(&self, raw_hash: String, sanitized_text: String) {
        let entry = SiCacheEntry {
            sanitized_text,
            timestamp: Instant::now(),
            hit_count: 0,
        };

        self.si_cache.insert(raw_hash.clone(), entry);
        tracing::debug!(
            "[CacheManager:L1-SI] INSERT hash={}",
            &raw_hash[..raw_hash.len().min(16)]
        );

        // LRU eviction: clear expired entries when over the limit
        if self.si_cache.len() > SI_CACHE_LIMIT {
            let before = self.si_cache.len();
            self.si_cache
                .retain(|_, v| !Self::is_expired(v.timestamp, LAYER_12_TTL));
            let after = self.si_cache.len();
            if before != after {
                tracing::debug!(
                    "[CacheManager:L1-SI] LRU cleanup: {} → {} (limit: {})",
                    before,
                    after,
                    SI_CACHE_LIMIT
                );
            }
        }
    }

    /// Compute the Layer 1 key: SHA256(raw instructions text)
    pub fn compute_si_key(raw_instructions: &str) -> String {
        Self::sha256_hex(raw_instructions.as_bytes())
    }

    // ===== Layer 2: Tools Cache =====

    /// Look up the cached processed tools
    ///
    /// Returns Some(tools_json_string) if the cache is valid, otherwise None
    pub fn lookup_tools(&self, raw_hash: &str) -> Option<String> {
        if let Ok(mut stats) = self.tools_stats.write() {
            stats.0 += 1;
        }

        let mut hit = false;
        let mut expired = false;
        let mut json = None;

        if let Some(entry) = self.tools_cache.get(raw_hash) {
            if !Self::is_expired(entry.timestamp, LAYER_12_TTL) {
                hit = true;
                json = Some(entry.tools_json.clone());
            } else {
                expired = true;
            }
        }

        if hit {
            if let Ok(mut stats) = self.tools_stats.write() {
                stats.1 += 1;
            }
            if let Some(mut e) = self.tools_cache.get_mut(raw_hash) {
                e.hit_count += 1;
            }
            tracing::debug!(
                "[CacheManager:L2-Tools] HIT hash={}",
                &raw_hash[..raw_hash.len().min(16)]
            );
            return json;
        }

        if expired {
            self.tools_cache.remove_if(raw_hash, |_, entry| {
                Self::is_expired(entry.timestamp, LAYER_12_TTL)
            });
            if let Ok(mut stats) = self.tools_stats.write() {
                stats.2 += 1;
            }
            tracing::debug!(
                "[CacheManager:L2-Tools] EXPIRED hash={}",
                &raw_hash[..raw_hash.len().min(16)]
            );
            return None;
        }

        if let Ok(mut stats) = self.tools_stats.write() {
            stats.2 += 1;
        }
        tracing::debug!(
            "[CacheManager:L2-Tools] MISS hash={}",
            &raw_hash[..raw_hash.len().min(16)]
        );
        None
    }

    /// Store into Layer 2: raw -> processed tools JSON
    pub fn cache_tools(&self, raw_hash: String, tools_json: String) {
        let entry = ToolsCacheEntry {
            tools_json,
            timestamp: Instant::now(),
            hit_count: 0,
        };

        self.tools_cache.insert(raw_hash.clone(), entry);
        tracing::debug!(
            "[CacheManager:L2-Tools] INSERT hash={}",
            &raw_hash[..raw_hash.len().min(16)]
        );

        if self.tools_cache.len() > TOOLS_CACHE_LIMIT {
            let before = self.tools_cache.len();
            self.tools_cache
                .retain(|_, v| !Self::is_expired(v.timestamp, LAYER_12_TTL));
            let after = self.tools_cache.len();
            if before != after {
                tracing::debug!(
                    "[CacheManager:L2-Tools] LRU cleanup: {} → {} (limit: {})",
                    before,
                    after,
                    TOOLS_CACHE_LIMIT
                );
            }
        }
    }

    /// Compute the Layer 2 key: SHA256(raw tools JSON string)
    pub fn compute_tools_key(raw_tools_json: &str) -> String {
        Self::sha256_hex(raw_tools_json.as_bytes())
    }

    // ===== Layer 3: Prefix Tracker =====

    /// Compute a stable combined prefix hash
    ///
    /// Input: JSON strings of systemInstruction and tools
    /// Uses the combination of Layer 1 + Layer 2's independent hashes, rather than the raw data
    pub fn compute_prefix_hash(si_json: &str, tools_json: &str) -> String {
        let si_hash = Self::sha256_hex(si_json.as_bytes());
        let tools_hash = if tools_json.is_empty() {
            String::from("no-tools")
        } else {
            Self::sha256_hex(tools_json.as_bytes())
        };
        // Combine: SHA256(si_hash + "::" + tools_hash)
        Self::sha256_hex(format!("{}::{}", si_hash, tools_hash).as_bytes())
    }

    /// Look up the cache_name in a prefix tracking entry
    ///
    /// Returns Some(cache_name) if a valid cache exists, otherwise None
    pub fn lookup_prefix(&self, hash: &str) -> Option<String> {
        if let Ok(mut stats) = self.prefix_stats.write() {
            stats.0 += 1;
        }

        let mut hit = false;
        let mut expired = false;
        let mut cache_name = None;

        if let Some(entry) = self.prefix_tracker.get(hash) {
            if entry.expires_at > Instant::now() {
                hit = true;
                cache_name = Some(entry.cache_name.clone());
            } else {
                expired = true;
            }
        }

        if hit {
            if let Ok(mut stats) = self.prefix_stats.write() {
                stats.1 += 1;
            }
            if let Some(ref name) = cache_name {
                tracing::debug!(
                    "[CacheManager:L3-Prefix] HIT hash={} cache_name={}",
                    &hash[..hash.len().min(16)],
                    name
                );
            }
            return cache_name;
        }

        if expired {
            self.prefix_tracker
                .remove_if(hash, |_, entry| entry.expires_at <= Instant::now());
            if let Ok(mut stats) = self.prefix_stats.write() {
                stats.2 += 1;
            }
            tracing::debug!(
                "[CacheManager:L3-Prefix] EXPIRED hash={}",
                &hash[..hash.len().min(16)]
            );
            return None;
        }

        if let Ok(mut stats) = self.prefix_stats.write() {
            stats.2 += 1;
        }
        tracing::debug!(
            "[CacheManager:L3-Prefix] MISS hash={}",
            &hash[..hash.len().min(16)]
        );
        None
    }

    /// Insert or update a Layer 3 entry
    ///
    /// cache_name: the cache resource name returned by Gemini. Currently uses the hash itself as a fallback.
    /// si_hash / tools_hash: the associated Layer 1/2 keys, used for tracking
    pub fn insert_prefix(
        &self,
        hash: String,
        cache_name: String,
        si_hash: String,
        tools_hash: String,
        model: String,
        ttl_secs: Option<u64>,
    ) {
        let ttl = ttl_secs.unwrap_or(3600);
        let now = Instant::now();
        let entry = PrefixTrackingEntry {
            si_hash,
            tools_hash,
            cache_name,
            created_at: now,
            expires_at: now + Duration::from_secs(ttl),
            implicit_hit_count: 0,
            explicit_hit_count: 0,
            model,
        };
        tracing::info!(
            "[CacheManager:L3-Prefix] INSERT hash={} ttl={}s",
            &hash[..hash.len().min(16)],
            ttl
        );
        self.prefix_tracker.insert(hash, entry);

        if self.prefix_tracker.len() > PREFIX_TRACKER_LIMIT {
            let before = self.prefix_tracker.len();
            let now = Instant::now();
            self.prefix_tracker.retain(|_, v| v.expires_at > now);
            let after = self.prefix_tracker.len();
            if before != after {
                tracing::debug!(
                    "[CacheManager:L3-Prefix] LRU cleanup: {} → {} (limit: {})",
                    before,
                    after,
                    PREFIX_TRACKER_LIMIT
                );
            }
        }
    }

    /// Record one implicit cache hit (from a response's cachedContentTokenCount > 0)
    pub fn record_implicit_hit(&self, hash: &str) {
        if let Some(mut entry) = self.prefix_tracker.get_mut(hash) {
            entry.implicit_hit_count += 1;
            tracing::debug!(
                "[CacheManager:L3-Prefix] implicit hit #{} for hash={}",
                entry.implicit_hit_count,
                &hash[..hash.len().min(16)]
            );
        }
    }

    /// Record one explicit cache hit
    pub fn record_explicit_hit(&self, hash: &str) {
        if let Some(mut entry) = self.prefix_tracker.get_mut(hash) {
            entry.explicit_hit_count += 1;
        }
    }

    // ===== Backward-compatible API =====

    /// Look up a prefix: compatible with the legacy API, equivalent to lookup_prefix
    #[inline]
    pub fn lookup(&self, hash: &str) -> Option<String> {
        self.lookup_prefix(hash)
    }

    /// Insert a prefix: compatible with the legacy API
    #[inline]
    pub fn insert(&self, hash: String, cache_name: String, ttl_secs: Option<u64>) {
        self.insert_prefix(
            hash,
            cache_name,
            String::new(), // si_hash unknown in legacy path
            String::new(), // tools_hash unknown in legacy path
            String::new(), // model unknown in legacy path
            ttl_secs,
        );
    }

    // ===== Stats & Management =====

    /// Get per-layer stats
    pub fn get_layer_stats(&self) -> LayerStats {
        let si = self.si_stats.read().unwrap();
        let tools = self.tools_stats.read().unwrap();
        let prefix = self.prefix_stats.read().unwrap();

        let total_implicit = self
            .prefix_tracker
            .iter()
            .map(|e| e.implicit_hit_count)
            .sum();
        let total_explicit = self
            .prefix_tracker
            .iter()
            .map(|e| e.explicit_hit_count)
            .sum();

        LayerStats {
            si_total: si.0,
            si_hits: si.1,
            si_misses: si.2,
            tools_total: tools.0,
            tools_hits: tools.1,
            tools_misses: tools.2,
            prefix_total: prefix.0,
            prefix_hits: prefix.1,
            prefix_misses: prefix.2,
            total_implicit_hits: total_implicit,
            total_explicit_hits: total_explicit,
            active_si_entries: self.si_cache.len(),
            active_tools_entries: self.tools_cache.len(),
            active_prefix_entries: self.prefix_tracker.len(),
        }
    }

    /// Evict all expired entries
    pub fn evict_expired(&self) -> usize {
        let si_before = self.si_cache.len();
        self.si_cache
            .retain(|_, v| !Self::is_expired(v.timestamp, LAYER_12_TTL));
        let tools_before = self.tools_cache.len();
        self.tools_cache
            .retain(|_, v| !Self::is_expired(v.timestamp, LAYER_12_TTL));
        let prefix_before = self.prefix_tracker.len();
        let now = Instant::now();
        self.prefix_tracker.retain(|_, v| v.expires_at > now);

        let total = (si_before - self.si_cache.len())
            + (tools_before - self.tools_cache.len())
            + (prefix_before - self.prefix_tracker.len());

        if total > 0 {
            tracing::debug!(
                "[CacheManager] Evicted {} expired entries across all layers",
                total
            );
        }
        total
    }

    /// Clear all layer caches and stats
    pub fn clear(&self) {
        self.si_cache.clear();
        self.tools_cache.clear();
        self.prefix_tracker.clear();
        if let Ok(mut s) = self.si_stats.write() {
            *s = (0, 0, 0);
        }
        if let Ok(mut s) = self.tools_stats.write() {
            *s = (0, 0, 0);
        }
        if let Ok(mut s) = self.prefix_stats.write() {
            *s = (0, 0, 0);
        }
        tracing::info!("[CacheManager] All layers cleared");
    }
}

// ===== Global Singleton =====

use std::sync::LazyLock;
static GLOBAL_CACHE_MANAGER: LazyLock<CacheManager> = LazyLock::new(CacheManager::new);

/// Get the global CacheManager singleton
pub fn global_cache_manager() -> &'static CacheManager {
    &GLOBAL_CACHE_MANAGER
}

// ===== Tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    // ===== Layer 1 Tests =====

    #[test]
    fn test_si_cache_insert_and_lookup() {
        let cm = CacheManager::new();
        let raw = "Current time: 2024-01-01\nYou are a helpful assistant.";
        let key = CacheManager::compute_si_key(raw);

        // Initially miss
        assert!(cm.lookup_si(&key).is_none());

        // Insert and hit
        cm.cache_si(key.clone(), "sanitized text".to_string());
        assert_eq!(cm.lookup_si(&key), Some("sanitized text".to_string()));

        // Second hit should work
        assert_eq!(cm.lookup_si(&key), Some("sanitized text".to_string()));
    }

    #[test]
    fn test_si_cache_hit_count() {
        let cm = CacheManager::new();
        let key = CacheManager::compute_si_key("test prompt");
        cm.cache_si(key.clone(), "cached".to_string());

        cm.lookup_si(&key);
        cm.lookup_si(&key);

        let stats = cm.get_layer_stats();
        assert_eq!(stats.si_total, 2); // 2 lookups
        assert_eq!(stats.si_hits, 2);
        assert_eq!(stats.active_si_entries, 1);
    }

    #[test]
    fn test_si_cache_different_inputs() {
        let cm = CacheManager::new();
        let key_a = CacheManager::compute_si_key("prompt A");
        let key_b = CacheManager::compute_si_key("prompt B");

        cm.cache_si(key_a.clone(), "sanitized A".to_string());
        cm.cache_si(key_b.clone(), "sanitized B".to_string());

        assert_eq!(cm.lookup_si(&key_a), Some("sanitized A".to_string()));
        assert_eq!(cm.lookup_si(&key_b), Some("sanitized B".to_string()));
        assert_ne!(key_a, key_b); // different raw prompts → different keys

        let stats = cm.get_layer_stats();
        assert_eq!(stats.active_si_entries, 2);
    }

    // ===== Layer 2 Tests =====

    #[test]
    fn test_tools_cache_insert_and_lookup() {
        let cm = CacheManager::new();
        let raw = r#"[{"functionDeclarations":[{"name":"tool_a"}]}]"#;
        let key = CacheManager::compute_tools_key(raw);

        assert!(cm.lookup_tools(&key).is_none());

        cm.cache_tools(key.clone(), raw.to_string());
        assert_eq!(cm.lookup_tools(&key), Some(raw.to_string()));
    }

    #[test]
    fn test_tools_cache_hit_count() {
        let cm = CacheManager::new();
        let key = CacheManager::compute_tools_key("[{}]");
        cm.cache_tools(key.clone(), "[{}]".to_string());

        cm.lookup_tools(&key);
        cm.lookup_tools(&key);
        cm.lookup_tools(&key);

        let stats = cm.get_layer_stats();
        assert_eq!(stats.tools_total, 3);
        assert_eq!(stats.tools_hits, 3);
    }

    #[test]
    fn test_tools_cache_isolated_from_si() {
        let cm = CacheManager::new();

        let si_key = CacheManager::compute_si_key("prompt");
        let tools_key = CacheManager::compute_tools_key("tools");

        cm.cache_si(si_key.clone(), "cached prompt".to_string());
        cm.cache_tools(tools_key.clone(), "cached tools".to_string());

        // SI lookup shouldn't affect Tools stats
        cm.lookup_si(&si_key);
        cm.lookup_si(&si_key);
        cm.lookup_tools(&tools_key);

        let stats = cm.get_layer_stats();
        assert_eq!(stats.si_total, 2);
        assert_eq!(stats.si_hits, 2);
        assert_eq!(stats.tools_total, 1);
        assert_eq!(stats.tools_hits, 1);
    }

    // ===== Layer 3 Tests =====

    #[test]
    fn test_compute_prefix_hash_deterministic() {
        let si1 = r#"{"role":"user","parts":[{"text":"system prompt"}]}"#;
        let tools1 = r#"[{"functionDeclarations":[{"name":"test_tool"}]}]"#;

        let hash1 = CacheManager::compute_prefix_hash(si1, tools1);
        let hash2 = CacheManager::compute_prefix_hash(si1, tools1);
        assert_eq!(hash1, hash2, "Same inputs must produce same hash");
    }

    #[test]
    fn test_compute_prefix_hash_different_si() {
        let si_a = r#"{"role":"user","parts":[{"text":"system A"}]}"#;
        let si_b = r#"{"role":"user","parts":[{"text":"system B"}]}"#;
        let tools = r#"[{"functionDeclarations":[]}]"#;

        let hash_a = CacheManager::compute_prefix_hash(si_a, tools);
        let hash_b = CacheManager::compute_prefix_hash(si_b, tools);
        assert_ne!(hash_a, hash_b, "Different SI must produce different hash");
    }

    #[test]
    fn test_compute_prefix_hash_different_tools() {
        let si = r#"{"role":"user","parts":[{"text":"system"}]}"#;
        let tools_a = r#"[{"functionDeclarations":[{"name":"a"}]}]"#;
        let tools_b = r#"[{"functionDeclarations":[{"name":"b"}]}]"#;

        let hash_a = CacheManager::compute_prefix_hash(si, tools_a);
        let hash_b = CacheManager::compute_prefix_hash(si, tools_b);
        assert_ne!(
            hash_a, hash_b,
            "Different tools must produce different hash"
        );
    }

    #[test]
    fn test_compute_prefix_hash_empty_tools() {
        let si = "some si";
        let tools_empty = "";

        let hash = CacheManager::compute_prefix_hash(si, tools_empty);
        // Should not panic with empty tools
        assert!(!hash.is_empty());
    }

    #[test]
    fn test_prefix_lookup_miss() {
        let cm = CacheManager::new();
        let result = cm.lookup_prefix("nonexistent_hash");
        assert!(result.is_none());
    }

    #[test]
    fn test_prefix_insert_and_lookup() {
        let cm = CacheManager::new();
        let hash = "test_hash_prefix_123".to_string();
        let cache_name = "cachedContents/test123".to_string();

        cm.insert_prefix(
            hash.clone(),
            cache_name.clone(),
            "si_abc".to_string(),
            "tools_def".to_string(),
            "gemini-flash".to_string(),
            Some(3600),
        );
        let result = cm.lookup_prefix(&hash);
        assert_eq!(result, Some(cache_name));
    }

    #[test]
    fn test_prefix_expiry() {
        let cm = CacheManager::new();
        let hash = "expiring_prefix_hash".to_string();

        cm.insert_prefix(
            hash.clone(),
            "cache_name".to_string(),
            "si".to_string(),
            "tools".to_string(),
            "model".to_string(),
            Some(0), // TTL=0 expires immediately
        );

        thread::sleep(Duration::from_millis(10));
        let result = cm.lookup_prefix(&hash);
        assert!(result.is_none(), "Expired entry should not be returned");
    }

    #[test]
    fn test_record_implicit_hit() {
        let cm = CacheManager::new();
        let hash = "test_implicit_hash".to_string();

        cm.insert_prefix(
            hash.clone(),
            "cache".to_string(),
            "si".to_string(),
            "tools".to_string(),
            "model".to_string(),
            Some(3600),
        );

        cm.record_implicit_hit(&hash);
        cm.record_implicit_hit(&hash);

        let stats = cm.get_layer_stats();
        assert_eq!(stats.total_implicit_hits, 2);
    }

    #[test]
    fn test_record_explicit_hit() {
        let cm = CacheManager::new();
        let hash = "test_explicit_hash".to_string();

        cm.insert_prefix(
            hash.clone(),
            "cache".to_string(),
            "si".to_string(),
            "tools".to_string(),
            "model".to_string(),
            Some(3600),
        );

        cm.record_explicit_hit(&hash);

        let stats = cm.get_layer_stats();
        assert_eq!(stats.total_explicit_hits, 1);
    }

    // ===== Backward Compatibility Tests =====

    #[test]
    fn test_legacy_lookup() {
        let cm = CacheManager::new();
        let hash = "legacy_hash".to_string();
        cm.insert(hash.clone(), "legacy_cache".to_string(), Some(60));
        let result = cm.lookup(&hash);
        assert_eq!(result, Some("legacy_cache".to_string()));
    }

    #[test]
    fn test_legacy_insert_and_hit_tracking() {
        let cm = CacheManager::new();
        let hash = "legacy_with_hits".to_string();
        cm.insert(hash.clone(), "cache_name".to_string(), Some(3600));

        cm.lookup(&hash);
        cm.record_implicit_hit(&hash);

        let stats = cm.get_layer_stats();
        assert_eq!(stats.prefix_hits, 1);
        assert_eq!(stats.total_implicit_hits, 1);
    }

    // ===== Combined Scenario Tests =====

    #[test]
    fn test_full_three_layer_flow() {
        let cm = CacheManager::new();

        // Layer 1: SI
        let si_raw = "Current time: 2024-06-01\nYou are a coding assistant.";
        let si_key = CacheManager::compute_si_key(si_raw);
        cm.cache_si(si_key.clone(), "You are a coding assistant.".to_string());
        assert!(cm.lookup_si(&si_key).is_some());

        // Layer 2: Tools
        let tools_raw = r#"{"functionDeclarations":[{"name":"read_file"},{"name":"write_file"}]}"#;
        let tools_key = CacheManager::compute_tools_key(tools_raw);
        cm.cache_tools(tools_key.clone(), tools_raw.to_string());
        assert!(cm.lookup_tools(&tools_key).is_some());

        // Layer 3: Prefix
        let prefix_hash = CacheManager::compute_prefix_hash(si_raw, tools_raw);
        cm.insert_prefix(
            prefix_hash.clone(),
            format!("cachedContents/{}", &prefix_hash[..8]),
            si_key.clone(),
            tools_key.clone(),
            "gemini-3-flash".to_string(),
            Some(3600),
        );
        assert!(cm.lookup_prefix(&prefix_hash).is_some());

        // Stats should show all layers active
        let stats = cm.get_layer_stats();
        assert_eq!(stats.active_si_entries, 1);
        assert_eq!(stats.active_tools_entries, 1);
        assert_eq!(stats.active_prefix_entries, 1);
    }

    #[test]
    fn test_layer_independence() {
        let cm = CacheManager::new();

        // Cache only SI, not tools
        let si_key = CacheManager::compute_si_key("prompt");
        cm.cache_si(si_key.clone(), "sanitized prompt".to_string());

        // SI: hit, Tools: miss
        assert!(cm.lookup_si(&si_key).is_some());
        let tools_key = CacheManager::compute_tools_key("some tools");
        assert!(cm.lookup_tools(&tools_key).is_none());

        let stats = cm.get_layer_stats();
        assert_eq!(stats.si_hits, 1);
        assert_eq!(stats.tools_misses, 1);
    }

    #[test]
    fn test_evict_expired_all_layers() {
        let cm = CacheManager::new();

        // Insert into all layers with 0 TTL via insert_prefix
        cm.cache_si("si_key".to_string(), "text".to_string());
        cm.cache_tools("tools_key".to_string(), "{}".to_string());
        cm.insert_prefix(
            "prefix_key".to_string(),
            "cache".to_string(),
            "si".to_string(),
            "tools".to_string(),
            "model".to_string(),
            Some(0),
        );

        thread::sleep(Duration::from_millis(10));

        // should be expired
        assert!(cm.lookup_prefix("prefix_key").is_none());

        let evicted = cm.evict_expired();
        // prefix entry removed, SI and tools still valid (30min TTL)
        assert_eq!(evicted, 1, "Only prefix should be evicted");

        let stats = cm.get_layer_stats();
        assert_eq!(stats.active_si_entries, 1);
        assert_eq!(stats.active_tools_entries, 1);
        assert_eq!(stats.active_prefix_entries, 0);
    }

    #[test]
    fn test_clear_all() {
        let cm = CacheManager::new();

        cm.cache_si("si".to_string(), "text".to_string());
        cm.cache_tools("tools".to_string(), "{}".to_string());
        cm.insert_prefix(
            "prefix".to_string(),
            "cache".to_string(),
            "s".to_string(),
            "t".to_string(),
            "m".to_string(),
            Some(60),
        );

        cm.clear();

        let stats = cm.get_layer_stats();
        assert_eq!(stats.active_si_entries, 0);
        assert_eq!(stats.active_tools_entries, 0);
        assert_eq!(stats.active_prefix_entries, 0);
        assert_eq!(stats.si_total, 0);
        assert_eq!(stats.prefix_total, 0);
    }

    #[test]
    fn test_global_singleton() {
        let cm = global_cache_manager();
        cm.clear();
        let stats = cm.get_layer_stats();
        assert_eq!(stats.si_total, 0);
        assert_eq!(stats.prefix_total, 0);
        assert_eq!(stats.active_si_entries, 0);
    }
}
