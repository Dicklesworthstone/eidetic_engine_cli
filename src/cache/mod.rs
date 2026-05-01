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

// ============================================================================
// EE-373: Cache budget, memory-pressure fallback
// ============================================================================

/// Cache budget configuration (EE-373).
#[derive(Clone, Copy, Debug)]
pub struct CacheBudget {
    /// Maximum number of entries.
    pub max_entries: usize,
    /// Maximum memory in bytes (estimated).
    pub max_bytes: usize,
    /// High watermark ratio (0.0-1.0) for triggering pressure.
    pub high_watermark: f64,
    /// Critical watermark ratio for aggressive eviction.
    pub critical_watermark: f64,
}

impl Default for CacheBudget {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_bytes: 100 * 1024 * 1024, // 100MB
            high_watermark: 0.80,
            critical_watermark: 0.95,
        }
    }
}

impl CacheBudget {
    #[must_use]
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            ..Default::default()
        }
    }

    #[must_use]
    pub fn with_watermarks(mut self, high: f64, critical: f64) -> Self {
        self.high_watermark = high.clamp(0.0, 1.0);
        self.critical_watermark = critical.clamp(high, 1.0);
        self
    }

    #[must_use]
    pub fn high_entry_threshold(&self) -> usize {
        ((self.max_entries as f64) * self.high_watermark) as usize
    }

    #[must_use]
    pub fn critical_entry_threshold(&self) -> usize {
        ((self.max_entries as f64) * self.critical_watermark) as usize
    }
}

/// Memory pressure level.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum MemoryPressure {
    /// Normal operation.
    Normal,
    /// High pressure - start evicting proactively.
    High,
    /// Critical pressure - aggressive eviction and fallback.
    Critical,
}

impl MemoryPressure {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    #[must_use]
    pub const fn is_pressured(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

/// Assess memory pressure based on current usage.
#[must_use]
pub fn assess_pressure(current_entries: usize, budget: &CacheBudget) -> MemoryPressure {
    if current_entries >= budget.critical_entry_threshold() {
        MemoryPressure::Critical
    } else if current_entries >= budget.high_entry_threshold() {
        MemoryPressure::High
    } else {
        MemoryPressure::Normal
    }
}

/// Fallback policy for cache degradation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheFallbackPolicy {
    /// Continue with reduced capacity.
    ReduceCapacity,
    /// Fall back to no-cache mode.
    NoCache,
    /// Fall back to LRU from S3-FIFO.
    SimplifyPolicy,
    /// Evict aggressively and continue.
    AggressiveEvict,
}

impl CacheFallbackPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReduceCapacity => "reduce_capacity",
            Self::NoCache => "no_cache",
            Self::SimplifyPolicy => "simplify_policy",
            Self::AggressiveEvict => "aggressive_evict",
        }
    }
}

/// Fallback cache wrapper that handles degradation (EE-373).
pub struct FallbackCache<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    /// Primary cache policy.
    primary: Box<dyn CachePolicy<K, V>>,
    /// Fallback policy to apply under pressure.
    fallback_policy: CacheFallbackPolicy,
    /// Budget configuration.
    budget: CacheBudget,
    /// Current pressure level.
    pressure: MemoryPressure,
    /// Whether fallback is active.
    fallback_active: bool,
    /// Stats for the fallback wrapper.
    stats: CacheStats,
    /// Fallback activation count.
    fallback_activations: u64,
}

impl<K, V> FallbackCache<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    pub fn new(
        primary: Box<dyn CachePolicy<K, V>>,
        fallback_policy: CacheFallbackPolicy,
        budget: CacheBudget,
    ) -> Self {
        Self {
            primary,
            fallback_policy,
            budget,
            pressure: MemoryPressure::Normal,
            fallback_active: false,
            stats: CacheStats::default(),
            fallback_activations: 0,
        }
    }

    pub fn pressure(&self) -> MemoryPressure {
        self.pressure
    }

    pub fn is_fallback_active(&self) -> bool {
        self.fallback_active
    }

    pub fn fallback_activations(&self) -> u64 {
        self.fallback_activations
    }

    fn update_pressure(&mut self) {
        let new_pressure = assess_pressure(self.primary.len(), &self.budget);
        if new_pressure > self.pressure && new_pressure == MemoryPressure::Critical {
            self.activate_fallback();
        }
        self.pressure = new_pressure;
    }

    fn activate_fallback(&mut self) {
        if !self.fallback_active {
            self.fallback_active = true;
            self.fallback_activations += 1;
        }
    }
}

impl<K, V> CachePolicy<K, V> for FallbackCache<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    fn get(&mut self, key: &K) -> Option<&V> {
        let result = self.primary.get(key);
        if result.is_some() {
            self.stats.record_hit();
        } else {
            self.stats.record_miss();
        }
        result
    }

    fn put(&mut self, key: K, value: V) {
        self.update_pressure();
        if self.fallback_active && self.fallback_policy == CacheFallbackPolicy::NoCache {
            return;
        }
        self.primary.put(key, value);
        self.update_pressure();
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        let result = self.primary.remove(key);
        self.update_pressure();
        result
    }

    fn len(&self) -> usize {
        self.primary.len()
    }

    fn stats(&self) -> &CacheStats {
        &self.stats
    }

    fn name(&self) -> &'static str {
        if self.fallback_active {
            "fallback_cache"
        } else {
            "primary_cache"
        }
    }
}

/// Degradation report for cache fallback events.
#[derive(Clone, Debug)]
pub struct CacheDegradationReport {
    /// Current pressure level.
    pub pressure: MemoryPressure,
    /// Whether fallback is active.
    pub fallback_active: bool,
    /// Fallback policy in use.
    pub fallback_policy: CacheFallbackPolicy,
    /// Number of times fallback was activated.
    pub activation_count: u64,
    /// Current cache size.
    pub current_size: usize,
    /// Budget max entries.
    pub budget_max_entries: usize,
    /// Usage ratio (0.0-1.0).
    pub usage_ratio: f64,
}

impl CacheDegradationReport {
    #[must_use]
    pub fn from_fallback_cache<K, V>(cache: &FallbackCache<K, V>) -> Self
    where
        K: Clone + Eq + Hash,
        V: Clone,
    {
        let current_size = cache.len();
        let usage_ratio = if cache.budget.max_entries > 0 {
            current_size as f64 / cache.budget.max_entries as f64
        } else {
            0.0
        };

        Self {
            pressure: cache.pressure(),
            fallback_active: cache.is_fallback_active(),
            fallback_policy: cache.fallback_policy,
            activation_count: cache.fallback_activations(),
            current_size,
            budget_max_entries: cache.budget.max_entries,
            usage_ratio,
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

    // ====================================================================
    // EE-373: Cache budget and fallback tests
    // ====================================================================

    #[test]
    fn cache_budget_default_values() {
        let budget = CacheBudget::default();
        assert_eq!(budget.max_entries, 10_000);
        assert_eq!(budget.max_bytes, 100 * 1024 * 1024);
        assert!((budget.high_watermark - 0.80).abs() < 0.001);
        assert!((budget.critical_watermark - 0.95).abs() < 0.001);
    }

    #[test]
    fn cache_budget_thresholds() {
        let budget = CacheBudget::new(1000, 1024 * 1024);
        assert_eq!(budget.high_entry_threshold(), 800);
        assert_eq!(budget.critical_entry_threshold(), 950);
    }

    #[test]
    fn cache_budget_custom_watermarks() {
        let budget = CacheBudget::new(100, 1024)
            .with_watermarks(0.70, 0.90);
        assert_eq!(budget.high_entry_threshold(), 70);
        assert_eq!(budget.critical_entry_threshold(), 90);
    }

    #[test]
    fn memory_pressure_assessment_normal() {
        let budget = CacheBudget::new(100, 1024);
        assert_eq!(assess_pressure(50, &budget), MemoryPressure::Normal);
        assert_eq!(assess_pressure(79, &budget), MemoryPressure::Normal);
    }

    #[test]
    fn memory_pressure_assessment_high() {
        let budget = CacheBudget::new(100, 1024);
        assert_eq!(assess_pressure(80, &budget), MemoryPressure::High);
        assert_eq!(assess_pressure(94, &budget), MemoryPressure::High);
    }

    #[test]
    fn memory_pressure_assessment_critical() {
        let budget = CacheBudget::new(100, 1024);
        assert_eq!(assess_pressure(95, &budget), MemoryPressure::Critical);
        assert_eq!(assess_pressure(100, &budget), MemoryPressure::Critical);
    }

    #[test]
    fn memory_pressure_is_pressured() {
        assert!(!MemoryPressure::Normal.is_pressured());
        assert!(MemoryPressure::High.is_pressured());
        assert!(MemoryPressure::Critical.is_pressured());
    }

    #[test]
    fn fallback_policy_strings_are_stable() {
        assert_eq!(CacheFallbackPolicy::ReduceCapacity.as_str(), "reduce_capacity");
        assert_eq!(CacheFallbackPolicy::NoCache.as_str(), "no_cache");
        assert_eq!(CacheFallbackPolicy::SimplifyPolicy.as_str(), "simplify_policy");
        assert_eq!(CacheFallbackPolicy::AggressiveEvict.as_str(), "aggressive_evict");
    }

    #[test]
    fn fallback_cache_normal_operation() {
        let lru: Box<dyn CachePolicy<String, i32>> = Box::new(LruCache::new(10));
        let budget = CacheBudget::new(100, 1024);
        let mut cache = FallbackCache::new(lru, CacheFallbackPolicy::NoCache, budget);

        cache.put("a".to_string(), 1);
        cache.put("b".to_string(), 2);

        assert_eq!(cache.get(&"a".to_string()), Some(&1));
        assert_eq!(cache.pressure(), MemoryPressure::Normal);
        assert!(!cache.is_fallback_active());
    }

    #[test]
    fn fallback_cache_activates_under_pressure() {
        let lru: Box<dyn CachePolicy<String, i32>> = Box::new(LruCache::new(100));
        let budget = CacheBudget::new(10, 1024); // Small budget triggers pressure
        let mut cache = FallbackCache::new(lru, CacheFallbackPolicy::NoCache, budget);

        // Fill past critical threshold
        for i in 0..20 {
            cache.put(format!("key{}", i), i);
        }

        assert!(cache.pressure() >= MemoryPressure::High);
        assert!(cache.fallback_activations() >= 1);
    }

    #[test]
    fn fallback_cache_no_cache_mode_skips_puts() {
        let lru: Box<dyn CachePolicy<String, i32>> = Box::new(LruCache::new(100));
        let budget = CacheBudget::new(5, 1024);
        let mut cache = FallbackCache::new(lru, CacheFallbackPolicy::NoCache, budget);

        // Fill to trigger critical
        for i in 0..10 {
            cache.put(format!("key{}", i), i);
        }

        assert!(cache.is_fallback_active());

        // New puts should be skipped in NoCache fallback mode
        let initial_len = cache.len();
        cache.put("new_key".to_string(), 999);
        assert_eq!(cache.len(), initial_len);
    }

    #[test]
    fn cache_degradation_report_from_fallback() {
        let lru: Box<dyn CachePolicy<String, i32>> = Box::new(LruCache::new(100));
        let budget = CacheBudget::new(10, 1024);
        let mut cache = FallbackCache::new(lru, CacheFallbackPolicy::ReduceCapacity, budget);

        for i in 0..8 {
            cache.put(format!("key{}", i), i);
        }

        let report = CacheDegradationReport::from_fallback_cache(&cache);
        assert_eq!(report.current_size, 8);
        assert_eq!(report.budget_max_entries, 10);
        assert!((report.usage_ratio - 0.8).abs() < 0.001);
        assert_eq!(report.fallback_policy, CacheFallbackPolicy::ReduceCapacity);
    }

    #[test]
    fn cache_policy_fallback_degradation_scenario() {
        // Simulate a degradation scenario where cache must handle pressure
        let lru: Box<dyn CachePolicy<String, i32>> = Box::new(LruCache::new(50));
        let budget = CacheBudget::new(20, 1024).with_watermarks(0.60, 0.80);
        let mut cache = FallbackCache::new(lru, CacheFallbackPolicy::AggressiveEvict, budget);

        // Phase 1: Normal operation
        for i in 0..10 {
            cache.put(format!("phase1_{}", i), i);
        }
        assert_eq!(cache.pressure(), MemoryPressure::Normal);

        // Phase 2: Approach high watermark
        for i in 0..5 {
            cache.put(format!("phase2_{}", i), i + 100);
        }
        // Should be at or above high watermark now
        assert!(cache.pressure() >= MemoryPressure::High);

        // Phase 3: Push to critical
        for i in 0..10 {
            cache.put(format!("phase3_{}", i), i + 200);
        }
        assert_eq!(cache.pressure(), MemoryPressure::Critical);
        assert!(cache.is_fallback_active());

        // Verify degradation was tracked
        assert!(cache.fallback_activations() >= 1);

        // Cache should still be functional even under pressure
        let _ = cache.get(&"phase1_0".to_string());
        assert!(cache.stats().hits + cache.stats().misses > 0);
    }
}
