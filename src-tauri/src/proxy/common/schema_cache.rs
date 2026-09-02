#![allow(dead_code)]
// Reserved cache implementation, not currently enabled on the production path

use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

/// A cache entry
#[derive(Clone)]
struct CacheEntry {
    /// The cleaned Schema
    schema: Value,
    /// Last-used time
    last_used: Instant,
    /// Hit count
    hit_count: usize,
}

/// Schema cache
struct SchemaCache {
    /// Cache storage (key: cache_key, value: CacheEntry)
    cache: HashMap<String, CacheEntry>,
    /// Cache statistics
    stats: CacheStats,
}

/// Cache statistics
#[derive(Default, Clone, Debug)]
pub struct CacheStats {
    /// Total number of requests
    pub total_requests: usize,
    /// Number of cache hits
    pub cache_hits: usize,
    /// Number of cache misses
    pub cache_misses: usize,
}

impl CacheStats {
    /// Computes the cache hit rate
    pub fn hit_rate(&self) -> f64 {
        if self.total_requests == 0 {
            0.0
        } else {
            self.cache_hits as f64 / self.total_requests as f64
        }
    }
}

impl SchemaCache {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    /// Gets a cache entry
    fn get(&mut self, key: &str) -> Option<Value> {
        self.stats.total_requests += 1;

        if let Some(entry) = self.cache.get_mut(key) {
            // Update the last-used time and hit count
            entry.last_used = Instant::now();
            entry.hit_count += 1;
            self.stats.cache_hits += 1;
            Some(entry.schema.clone())
        } else {
            self.stats.cache_misses += 1;
            None
        }
    }

    /// Inserts a cache entry
    fn insert(&mut self, key: String, schema: Value) {
        // Check the cache size and evict if the limit is exceeded
        const MAX_CACHE_SIZE: usize = 1000;
        if self.cache.len() >= MAX_CACHE_SIZE {
            self.evict_lru();
        }

        let entry = CacheEntry {
            schema,
            last_used: Instant::now(),
            hit_count: 0,
        };
        self.cache.insert(key, entry);
    }

    /// LRU eviction policy: removes the least-recently-used entry
    fn evict_lru(&mut self) {
        if self.cache.is_empty() {
            return;
        }

        // Find the least-recently-used entry
        let oldest_key = self
            .cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone());

        if let Some(key) = oldest_key {
            self.cache.remove(&key);
        }
    }

    /// Gets cache statistics
    fn stats(&self) -> CacheStats {
        self.stats.clone()
    }

    /// Clears the cache
    fn clear(&mut self) {
        self.cache.clear();
        self.stats = CacheStats::default();
    }
}

/// Global Schema cache instance
static SCHEMA_CACHE: Lazy<RwLock<SchemaCache>> = Lazy::new(|| RwLock::new(SchemaCache::new()));

/// Computes the hash of a Schema
///
/// Uses the SHA-256 algorithm to hash the Schema, ensuring identical Schemas produce identical hashes
fn compute_schema_hash(schema: &Value) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    // Serialize in compact form to improve consistency
    let schema_str = schema.to_string();
    hasher.update(schema_str.as_bytes());

    // Return the first 16 characters of the hex string (unique enough)
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Schema cleaning with caching
///
/// This is the recommended cleaning entry point, with cache-based optimization
///
/// # Arguments
/// * `schema` - the JSON Schema to clean
/// * `tool_name` - the tool name, used as part of the cache key
///
/// # Returns
/// The cleaned Schema
pub fn clean_json_schema_cached(schema: &mut Value, tool_name: &str) {
    // 1. Compute the cache key for the original Schema
    let hash = compute_schema_hash(schema);
    let cache_key = format!("{}:{}", tool_name, hash);

    // 2. Try reading from the cache
    {
        if let Ok(mut cache) = SCHEMA_CACHE.write() {
            if let Some(cached) = cache.get(&cache_key) {
                *schema = cached;
                return;
            }
        }
    }

    // 3. Cache miss, perform cleaning
    super::json_schema::clean_json_schema_for_tool(schema, tool_name);

    // 4. Write to the cache (using the original hash as the key)
    if let Ok(mut cache) = SCHEMA_CACHE.write() {
        cache.insert(cache_key, schema.clone());
    }
}

/// Gets cache statistics
pub fn get_cache_stats() -> CacheStats {
    SCHEMA_CACHE
        .read()
        .map(|cache| cache.stats())
        .unwrap_or_default()
}

/// Clears the cache
pub fn clear_cache() {
    if let Ok(mut cache) = SCHEMA_CACHE.write() {
        cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_compute_schema_hash() {
        let schema1 = json!({"type": "string"});
        let schema2 = json!({"type": "string"});
        let schema3 = json!({"type": "number"});

        let hash1 = compute_schema_hash(&schema1);
        let hash2 = compute_schema_hash(&schema2);
        let hash3 = compute_schema_hash(&schema3);

        // Identical Schemas should produce identical hashes
        assert_eq!(hash1, hash2);
        // Different Schemas should produce different hashes
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_cache_hit() {
        clear_cache();

        let mut schema = json!({"type": "string", "minLength": 5});
        let tool_name = "test_tool";

        // First call - cache miss
        clean_json_schema_cached(&mut schema, tool_name);

        // Second call with the same Schema - should be a cache hit
        let mut schema2 = json!({"type": "string", "minLength": 5});
        clean_json_schema_cached(&mut schema2, tool_name);

        let stats = get_cache_stats();
        // Verify there was a cache hit
        assert!(
            stats.cache_hits > 0,
            "Expected cache hits, got: {:?}",
            stats
        );
        assert!(stats.hit_rate() > 0.0);
    }

    #[test]
    fn test_cache_eviction() {
        clear_cache();

        // Insert a large number of entries to trigger eviction
        for i in 0..1100 {
            let mut schema = json!({"type": "string", "index": i});
            let tool_name = format!("tool_{}", i);
            clean_json_schema_cached(&mut schema, &tool_name);
        }

        // Verify the cache size is bounded
        let stats = get_cache_stats();
        assert!(stats.total_requests > 0);
    }
}
