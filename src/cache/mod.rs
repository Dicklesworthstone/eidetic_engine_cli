//! Cache-admission policies for shadow evaluation and memory-budget gates.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// Cache statistics for comparison.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
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

/// Common interface for cache-admission policies.
pub trait CachePolicy<K, V> {
    fn get(&mut self, key: &K) -> Option<&V>;
    fn put(&mut self, key: K, value: V);
    fn remove(&mut self, key: &K) -> Option<V>;
    fn len(&self) -> usize;
    fn stats(&self) -> &CacheStats;
    fn name(&self) -> &'static str;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Baseline policy that intentionally never stores values.
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

    fn put(&mut self, _key: K, _value: V) {}

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

/// Deterministic least-recently-used cache used as a simple fallback policy.
pub struct LruCache<K, V> {
    capacity: usize,
    entries: HashMap<K, V>,
    order: VecDeque<K>,
    stats: CacheStats,
}

impl<K, V> LruCache<K, V>
where
    K: Clone + Eq + Hash,
{
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            stats: CacheStats::default(),
        }
    }

    fn touch(&mut self, key: &K) {
        if let Some(index) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(index);
        }
        self.order.push_front(key.clone());
    }

    fn evict_if_needed(&mut self) {
        while self.entries.len() > self.capacity {
            if let Some(key) = self.order.pop_back() {
                if self.entries.remove(&key).is_some() {
                    self.stats.record_eviction();
                }
            } else {
                break;
            }
        }
    }
}

impl<K, V> CachePolicy<K, V> for LruCache<K, V>
where
    K: Clone + Eq + Hash,
{
    fn get(&mut self, key: &K) -> Option<&V> {
        if self.entries.contains_key(key) {
            self.touch(key);
            self.stats.record_hit();
            self.entries.get(key)
        } else {
            self.stats.record_miss();
            None
        }
    }

    fn put(&mut self, key: K, value: V) {
        self.entries.insert(key.clone(), value);
        self.touch(&key);
        self.evict_if_needed();
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(index) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(index);
        }
        self.entries.remove(key)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stats(&self) -> &CacheStats {
        &self.stats
    }

    fn name(&self) -> &'static str {
        "lru"
    }
}

#[derive(Clone, Debug)]
struct S3Entry<V> {
    value: V,
    access_count: u8,
}

/// Compact S3-FIFO-style admission policy with small, main, and ghost queues.
pub struct S3FifoCache<K, V> {
    capacity: usize,
    small_capacity: usize,
    ghost_capacity: usize,
    small: VecDeque<K>,
    main: VecDeque<K>,
    ghost: VecDeque<K>,
    entries: HashMap<K, S3Entry<V>>,
    stats: CacheStats,
}

impl<K, V> S3FifoCache<K, V>
where
    K: Clone + Eq + Hash,
{
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let small_capacity = (capacity / 10).max(capacity / 3).max(1);
        Self {
            capacity,
            small_capacity,
            ghost_capacity: capacity,
            small: VecDeque::with_capacity(small_capacity),
            main: VecDeque::with_capacity(capacity - small_capacity),
            ghost: VecDeque::with_capacity(capacity),
            entries: HashMap::with_capacity(capacity),
            stats: CacheStats::default(),
        }
    }

    fn contains_ghost(&self, key: &K) -> bool {
        self.ghost.iter().any(|candidate| candidate == key)
    }

    fn remove_ghost(&mut self, key: &K) {
        if let Some(index) = self.ghost.iter().position(|candidate| candidate == key) {
            self.ghost.remove(index);
        }
    }

    fn push_ghost(&mut self, key: K) {
        if self.ghost.len() >= self.ghost_capacity {
            self.ghost.pop_back();
        }
        self.ghost.push_front(key);
    }

    fn enforce_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            if let Some(key) = self.small.pop_back() {
                if let Some(entry) = self.entries.remove(&key) {
                    if entry.access_count > 0 {
                        let promoted = key.clone();
                        self.main.push_front(key);
                        self.entries.insert(
                            promoted,
                            S3Entry {
                                value: entry.value,
                                access_count: 0,
                            },
                        );
                        self.stats.record_promotion();
                    } else {
                        self.push_ghost(key);
                        self.stats.record_eviction();
                    }
                }
            } else if let Some(key) = self.main.pop_back() {
                if self.entries.remove(&key).is_some() {
                    self.stats.record_eviction();
                }
            } else {
                break;
            }
        }
    }
}

impl<K, V> CachePolicy<K, V> for S3FifoCache<K, V>
where
    K: Clone + Eq + Hash,
{
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
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.value = value;
            entry.access_count = entry.access_count.saturating_add(1).min(3);
            return;
        }

        if self.contains_ghost(&key) {
            self.remove_ghost(&key);
            self.main.push_front(key.clone());
        } else {
            self.small.push_front(key.clone());
            while self.small.len() > self.small_capacity && self.entries.len() >= self.capacity {
                if let Some(evicted) = self.small.pop_back() {
                    if let Some(entry) = self.entries.remove(&evicted) {
                        if entry.access_count > 0 {
                            let promoted = evicted.clone();
                            self.main.push_front(evicted);
                            self.entries.insert(promoted, entry);
                            self.stats.record_promotion();
                        } else {
                            self.push_ghost(evicted);
                            self.stats.record_eviction();
                        }
                    }
                }
            }
        }

        self.entries.insert(
            key,
            S3Entry {
                value,
                access_count: 0,
            },
        );
        self.enforce_capacity();
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(index) = self.small.iter().position(|candidate| candidate == key) {
            self.small.remove(index);
        }
        if let Some(index) = self.main.iter().position(|candidate| candidate == key) {
            self.main.remove(index);
        }
        self.entries.remove(key).map(|entry| entry.value)
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

/// Comparison report for cache policy experiments.
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
    #[must_use]
    pub fn from_stats(name: &str, capacity: usize, operations: u64, stats: &CacheStats) -> Self {
        Self {
            policy_name: name.to_owned(),
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

/// Cache budget configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheBudget {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub high_watermark: f64,
    pub critical_watermark: f64,
}

impl Default for CacheBudget {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_bytes: 100 * 1024 * 1024,
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
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_watermarks(mut self, high: f64, critical: f64) -> Self {
        self.high_watermark = high.clamp(0.0, 1.0);
        self.critical_watermark = critical.clamp(self.high_watermark, 1.0);
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

/// Memory pressure level for cache fallback.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MemoryPressure {
    Normal,
    High,
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

/// Fallback policy under cache pressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheFallbackPolicy {
    ReduceCapacity,
    NoCache,
    SimplifyPolicy,
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

/// Cache wrapper that reports and reacts to memory pressure.
pub struct FallbackCache<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    primary: Box<dyn CachePolicy<K, V>>,
    fallback_policy: CacheFallbackPolicy,
    budget: CacheBudget,
    pressure: MemoryPressure,
    fallback_active: bool,
    stats: CacheStats,
    fallback_activations: u64,
}

impl<K, V> FallbackCache<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    #[must_use]
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

    #[must_use]
    pub fn pressure(&self) -> MemoryPressure {
        self.pressure
    }

    #[must_use]
    pub fn is_fallback_active(&self) -> bool {
        self.fallback_active
    }

    #[must_use]
    pub fn fallback_activations(&self) -> u64 {
        self.fallback_activations
    }

    fn update_pressure(&mut self) {
        self.pressure = assess_pressure(self.primary.len(), &self.budget);
        if self.pressure == MemoryPressure::Critical && !self.fallback_active {
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
#[derive(Clone, Debug, PartialEq)]
pub struct CacheDegradationReport {
    pub pressure: MemoryPressure,
    pub fallback_active: bool,
    pub fallback_policy: CacheFallbackPolicy,
    pub activation_count: u64,
    pub current_size: usize,
    pub budget_max_entries: usize,
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
