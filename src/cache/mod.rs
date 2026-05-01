//! Cache-admission policies for ee (EE-372 spike).
//!
//! This module provides a `CachePolicy` trait and implementations for
//! evaluating different cache-admission strategies:
//!
//! - **NoCache**: Always miss, baseline for comparison
//! - **Lru**: Classic Least Recently Used eviction
//! - **S3Fifo**: Three-queue FIFO with ghost tracking (SOSP 2023 paper)
//!
//! The S3-FIFO algorithm maintains three queues:
//! 1. Small FIFO (S): New items enter here
//! 2. Main FIFO (M): Promoted items from S (accessed twice)
//! 3. Ghost FIFO (G): Tracks recently evicted items from S
//!
//! Items are only promoted from S to M if accessed again while in S,
//! reducing one-hit-wonder pollution in the main cache.
//!
//! # Feature Flag
//!
//! This module is available when the `cache-spike` feature is enabled.
//! It is experimental and should not be used in production without
//! further validation.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// Cache statistics for comparison.
#[derive(Clone, Debug, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub promotions: u64,
}

impl CacheStats {
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    pub fn record_hit(&mut self) {
        self.hits += 1;
    }

    pub fn record_miss(&mut self) {
        self.misses += 1;
    }

    pub fn record_eviction(&mut self) {
        self.evictions += 1;
    }

    pub fn record_promotion(&mut self) {
        self.promotions += 1;
    }
}

/// Cache policy trait for different eviction strategies.
pub trait CachePolicy<K, V> {
    /// Get a value from the cache.
    fn get(&mut self, key: &K) -> Option<&V>;

    /// Insert a key-value pair into the cache.
    fn put(&mut self, key: K, value: V);

    /// Remove a key from the cache.
    fn remove(&mut self, key: &K) -> Option<V>;

    /// Get the current number of items in the cache.
    fn len(&self) -> usize;

    /// Check if the cache is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get cache statistics.
    fn stats(&self) -> &CacheStats;

    /// Get the cache policy name.
    fn name(&self) -> &'static str;
}

/// No-cache baseline - always returns None (miss).
pub struct NoCache<K, V> {
    stats: CacheStats,
    _marker: std::marker::PhantomData<(K, V)>,
}

impl<K, V> NoCache<K, V> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            stats: CacheStats::default(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<K, V> Default for NoCache<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> CachePolicy<K, V> for NoCache<K, V> {
    fn get(&mut self, _key: &K) -> Option<&V> {
        self.stats.record_miss();
        None
    }

    fn put(&mut self, _key: K, _value: V) {
        // No-op
    }

    fn remove(&mut self, _key: &K) -> Option<V> {
        None
    }

    fn len(&self) -> usize {
        0
    }

    fn stats(&self) -> &CacheStats {
        &self.stats
    }

    fn name(&self) -> &'static str {
        "no_cache"
    }
}

/// LRU (Least Recently Used) cache implementation.
pub struct LruCache<K, V> {
    capacity: usize,
    map: HashMap<K, V>,
    order: VecDeque<K>,
    stats: CacheStats,
}

impl<K: Eq + Hash + Clone, V> LruCache<K, V> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            map: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            stats: CacheStats::default(),
        }
    }

    fn move_to_front(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            self.order.push_front(key.clone());
        }
    }

    fn evict_lru(&mut self) {
        if let Some(old_key) = self.order.pop_back() {
            self.map.remove(&old_key);
            self.stats.record_eviction();
        }
    }
}

impl<K: Eq + Hash + Clone, V> CachePolicy<K, V> for LruCache<K, V> {
    fn get(&mut self, key: &K) -> Option<&V> {
        if self.map.contains_key(key) {
            self.move_to_front(key);
            self.stats.record_hit();
            self.map.get(key)
        } else {
            self.stats.record_miss();
            None
        }
    }

    fn put(&mut self, key: K, value: V) {
        if self.map.contains_key(&key) {
            self.map.insert(key.clone(), value);
            self.move_to_front(&key);
        } else {
            if self.map.len() >= self.capacity {
                self.evict_lru();
            }
            self.order.push_front(key.clone());
            self.map.insert(key, value);
        }
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.map.remove(key)
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn stats(&self) -> &CacheStats {
        &self.stats
    }

    fn name(&self) -> &'static str {
        "lru"
    }
}

/// Internal entry state for S3-FIFO.
struct S3Entry<V> {
    value: V,
    access_count: u8,
}

/// S3-FIFO cache implementation.
///
/// Based on "FIFO Queues are All You Need for Cache Eviction" (SOSP 2023).
/// Uses three queues: Small (S), Main (M), and Ghost (G).
pub struct S3FifoCache<K, V> {
    capacity: usize,
    small_capacity: usize,
    ghost_capacity: usize,
    small_queue: VecDeque<K>,
    main_queue: VecDeque<K>,
    ghost_set: VecDeque<K>,
    entries: HashMap<K, S3Entry<V>>,
    stats: CacheStats,
}

impl<K: Eq + Hash + Clone, V> S3FifoCache<K, V> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        // Small queue is ~10% of capacity, but at least capacity/3 for small caches
        let small_capacity = (capacity / 10).max(capacity / 3).max(1);
        let ghost_capacity = capacity;
        Self {
            capacity,
            small_capacity,
            ghost_capacity,
            small_queue: VecDeque::with_capacity(small_capacity),
            main_queue: VecDeque::with_capacity(capacity - small_capacity),
            ghost_set: VecDeque::with_capacity(ghost_capacity),
            entries: HashMap::with_capacity(capacity),
            stats: CacheStats::default(),
        }
    }

    fn is_in_ghost(&self, key: &K) -> bool {
        self.ghost_set.iter().any(|k| k == key)
    }

    fn add_to_ghost(&mut self, key: K) {
        if self.ghost_set.len() >= self.ghost_capacity {
            self.ghost_set.pop_back();
        }
        self.ghost_set.push_front(key);
    }

    fn remove_from_ghost(&mut self, key: &K) {
        if let Some(pos) = self.ghost_set.iter().position(|k| k == key) {
            self.ghost_set.remove(pos);
        }
    }

    fn evict_from_small(&mut self) {
        while self.small_queue.len() > self.small_capacity {
            if let Some(key) = self.small_queue.pop_back() {
                if let Some(entry) = self.entries.remove(&key) {
                    if entry.access_count > 0 {
                        // Promote to main queue
                        let promoted_key = key.clone();
                        self.main_queue.push_front(key);
                        self.entries.insert(
                            promoted_key,
                            S3Entry {
                                value: entry.value,
                                access_count: 0,
                            },
                        );
                        self.stats.record_promotion();
                    } else {
                        // Evict and add to ghost
                        self.add_to_ghost(key);
                        self.stats.record_eviction();
                    }
                }
            }
        }
    }

    fn evict_from_main(&mut self) {
        let main_capacity = self.capacity - self.small_capacity;
        while self.main_queue.len() > main_capacity {
            if let Some(key) = self.main_queue.pop_back() {
                let should_reinsert = self
                    .entries
                    .get(&key)
                    .map(|e| e.access_count > 0)
                    .unwrap_or(false);

                if should_reinsert {
                    // Remove and re-insert at front with reset count
                    if let Some(mut entry) = self.entries.remove(&key) {
                        entry.access_count = 0;
                        self.entries.insert(key.clone(), entry);
                        self.main_queue.push_front(key);
                    }
                } else {
                    self.entries.remove(&key);
                    self.stats.record_eviction();
                }
            }
        }
    }
}

impl<K: Eq + Hash + Clone, V> CachePolicy<K, V> for S3FifoCache<K, V> {
    fn get(&mut self, key: &K) -> Option<&V> {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.access_count = entry.access_count.saturating_add(1).min(3);
            self.stats.record_hit();
            self.entries.get(key).map(|entry| &entry.value)
        } else {
            self.stats.record_miss();
            None
        }
    }

    fn put(&mut self, key: K, value: V) {
        if self.entries.contains_key(&key) {
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.value = value;
                entry.access_count = entry.access_count.saturating_add(1).min(3);
            }
            return;
        }

        // Check if item was recently evicted (in ghost)
        let was_in_ghost = self.is_in_ghost(&key);
        if was_in_ghost {
            self.remove_from_ghost(&key);
            // Insert directly into main queue
            self.main_queue.push_front(key.clone());
            self.entries.insert(
                key,
                S3Entry {
                    value,
                    access_count: 0,
                },
            );
            self.evict_from_main();
        } else {
            // Insert into small queue
            self.small_queue.push_front(key.clone());
            self.entries.insert(
                key,
                S3Entry {
                    value,
                    access_count: 0,
                },
            );
            self.evict_from_small();
            self.evict_from_main();
        }
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(pos) = self.small_queue.iter().position(|k| k == key) {
            self.small_queue.remove(pos);
        }
        if let Some(pos) = self.main_queue.iter().position(|k| k == key) {
            self.main_queue.remove(pos);
        }
        self.entries.remove(key).map(|e| e.value)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stats(&self) -> &CacheStats {
        &self.stats
    }

    fn name(&self) -> &'static str {
        "s3_fifo"
    }
}

/// Comparison report for different cache policies.
#[derive(Clone, Debug)]
pub struct CacheComparisonReport {
    pub policy_name: String,
    pub capacity: usize,
    pub operations: u64,
    pub hit_rate: f64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub promotions: u64,
}

impl CacheComparisonReport {
    pub fn from_stats(name: &str, capacity: usize, operations: u64, stats: &CacheStats) -> Self {
        Self {
            policy_name: name.to_string(),
            capacity,
            operations,
            hit_rate: stats.hit_rate(),
            hits: stats.hits,
            misses: stats.misses,
            evictions: stats.evictions,
            promotions: stats.promotions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_cache_always_misses() {
        let mut cache: NoCache<&str, &str> = NoCache::new();
        assert!(cache.get(&"key1").is_none());
        cache.put("key1", "value1");
        assert!(cache.get(&"key1").is_none());
        assert_eq!(CachePolicy::<&str, &str>::stats(&cache).misses, 2);
        assert_eq!(CachePolicy::<&str, &str>::stats(&cache).hits, 0);
        assert_eq!(CachePolicy::<&str, &str>::name(&cache), "no_cache");
    }

    #[test]
    fn lru_basic_operations() {
        let mut cache = LruCache::new(3);
        cache.put("a", 1);
        cache.put("b", 2);
        cache.put("c", 3);

        assert_eq!(cache.get(&"a"), Some(&1));
        assert_eq!(cache.get(&"b"), Some(&2));
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.name(), "lru");
    }

    #[test]
    fn lru_eviction() {
        let mut cache = LruCache::new(2);
        cache.put("a", 1);
        cache.put("b", 2);
        cache.put("c", 3); // Should evict "a"

        assert!(cache.get(&"a").is_none());
        assert_eq!(cache.get(&"b"), Some(&2));
        assert_eq!(cache.get(&"c"), Some(&3));
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn lru_access_updates_order() {
        let mut cache = LruCache::new(2);
        cache.put("a", 1);
        cache.put("b", 2);
        let _ = cache.get(&"a"); // Access "a", making it most recent
        cache.put("c", 3); // Should evict "b", not "a"

        assert_eq!(cache.get(&"a"), Some(&1));
        assert!(cache.get(&"b").is_none());
        assert_eq!(cache.get(&"c"), Some(&3));
    }

    #[test]
    fn s3_fifo_basic_operations() {
        let mut cache = S3FifoCache::new(10);
        cache.put("a", 1);
        cache.put("b", 2);
        cache.put("c", 3);

        assert_eq!(cache.get(&"a"), Some(&1));
        assert_eq!(cache.get(&"b"), Some(&2));
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.name(), "s3_fifo");
    }

    #[test]
    fn s3_fifo_promotion_on_reaccess() {
        let mut cache = S3FifoCache::new(10);
        cache.put("a", 1);

        // First access in small queue
        assert_eq!(cache.get(&"a"), Some(&1));

        // Should have recorded access
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn s3_fifo_ghost_reinsert() {
        let mut cache = S3FifoCache::new(3);

        // Fill cache
        cache.put("a", 1);
        cache.put("b", 2);
        cache.put("c", 3);
        cache.put("d", 4); // Evicts "a" to ghost

        // "a" should be in ghost now
        assert!(cache.get(&"a").is_none());

        // Re-insert "a" - should go to main queue
        cache.put("a", 10);
        assert_eq!(cache.get(&"a"), Some(&10));
    }

    #[test]
    fn cache_stats_hit_rate() {
        let mut stats = CacheStats::default();
        stats.record_hit();
        stats.record_hit();
        stats.record_miss();

        assert!((stats.hit_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn cache_comparison_report_from_stats() {
        let stats = CacheStats {
            hits: 80,
            misses: 20,
            evictions: 10,
            promotions: 5,
        };

        let report = CacheComparisonReport::from_stats("test_cache", 100, 100, &stats);
        assert_eq!(report.policy_name, "test_cache");
        assert_eq!(report.capacity, 100);
        assert!((report.hit_rate - 0.8).abs() < 0.001);
    }

    #[test]
    fn lru_remove_works() {
        let mut cache = LruCache::new(3);
        cache.put("a", 1);
        cache.put("b", 2);

        assert_eq!(cache.remove(&"a"), Some(1));
        assert!(cache.get(&"a").is_none());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn s3_fifo_remove_works() {
        let mut cache = S3FifoCache::new(10);
        cache.put("a", 1);
        cache.put("b", 2);

        assert_eq!(cache.remove(&"a"), Some(1));
        assert!(cache.get(&"a").is_none());
        assert_eq!(cache.len(), 1);
    }
}
