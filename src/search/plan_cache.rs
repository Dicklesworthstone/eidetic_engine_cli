//! In-process EQL query plan cache.
//!
//! Memoizes the resolved EQL plan (parse + bind + index selection + join
//! strategy) so repeated identical queries skip the dominant per-call cost.
//! Lookup key is `(eql_hash, index_manifest_version, search_config_hash)`;
//! bumping the manifest version or the search-config hash naturally invalidates
//! entries because they form part of the key. No active eviction is required
//! beyond bounded LRU.
//!
//! Distinguishability versus neighboring caches:
//!
//! * L2 pack cache (`bd-ndzfg`) caches **results** keyed on
//!   `(query, workspace, manifest)`. On an L2 miss the search path still pays
//!   parse + bind + index-choice cost; the plan cache eliminates that cost.
//! * Single-flight (`bd-gni47`) coalesces concurrent in-flight duplicate calls.
//!   The plan cache helps **after** the in-flight wave ends and the same plan
//!   is reused across new callers.
//!
//! Bead: `bd-2mey5`. `run_search_inner` consults this process cache before
//! opening Frankensearch so repeated identical searches reuse parse/bind/index
//! selection work while still executing fresh retrieval against live indexes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use serde::Serialize;

use crate::models::query::EqlQuery;

/// Stable schema tag for plan-tree hashing.
const PLAN_TREE_HASH_DOMAIN: &[u8] = b"ee.search.plan_cache.tree.v1";

/// Stable schema tag for EQL request hashing.
const EQL_HASH_DOMAIN: &[u8] = b"ee.search.plan_cache.eql.v1";

/// Stable schema tag for the search-config hash callers pass in.
const SEARCH_CONFIG_HASH_DOMAIN: &[u8] = b"ee.search.plan_cache.search_config.v1";

/// Default cache size used when callers do not override via configuration.
///
/// Matches the bead acceptance default for `EE_QUERY_PLAN_CACHE_ENTRIES`.
pub const DEFAULT_PLAN_CACHE_ENTRIES: usize = 1024;

/// Hard upper bound on cache size to keep memory bounded even when callers
/// hand in a misconfigured value.
pub const MAX_PLAN_CACHE_ENTRIES: usize = 1 << 20;

// bd-25yao: RwLock (was Mutex) so cache-hit reads via `PlanCache::get`
// can take `.read()` and run concurrently. Mirrors bd-2lin9 (PPR
// prefetch cache) and bd-1nan9 (IN_MEMORY_ALGORITHM_RESULTS).
static PROCESS_PLAN_CACHE: OnceLock<RwLock<PlanCache>> = OnceLock::new();

/// Composite key for the EQL plan cache. All fields are 64-bit content hashes
/// so the key itself is cheap to compare and clone.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlanCacheKey {
    /// 64-bit blake3 prefix of the canonical EQL request bytes.
    pub eql_hash: u64,
    /// Live search index manifest version. Bump invalidates entries.
    pub index_manifest_version: u64,
    /// Caller-supplied hash of the resolved search configuration.
    pub search_config_hash: u64,
}

impl PlanCacheKey {
    #[must_use]
    pub const fn new(eql_hash: u64, index_manifest_version: u64, search_config_hash: u64) -> Self {
        Self {
            eql_hash,
            index_manifest_version,
            search_config_hash,
        }
    }
}

/// Resolved plan-cache payload. Today the persisted payload is the parsed
/// `EqlQuery`; later slices append the bound-index and join-strategy fields
/// as concrete types. Optional fields stay `None` in the current slice so the
/// integration follow-up can populate them without changing the cache shape.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledPlan {
    /// Parsed EQL request.
    pub parsed_query: EqlQuery,
    /// Bound index choice, populated by the follow-up integration bead.
    pub bound_index: Option<String>,
    /// Resolved join strategy descriptor, populated by the follow-up bead.
    pub join_strategy: Option<String>,
}

impl CompiledPlan {
    #[must_use]
    pub fn from_query(parsed_query: EqlQuery) -> Self {
        Self {
            parsed_query,
            bound_index: None,
            join_strategy: None,
        }
    }
}

/// Snapshot of cache observability counters. Counters are monotonic across the
/// cache lifetime; callers compute rate-style metrics by sampling deltas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlanCacheStats {
    pub capacity: usize,
    pub current_size: usize,
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    pub invalidations: u64,
}

/// Outcome of an `insert` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanCacheInsert {
    /// Plan-tree hash of the inserted plan, recomputed at insert time for
    /// callers that want to assert that the same plan deserializes to the
    /// same canonical content.
    pub plan_tree_hash: String,
    /// Keys that were evicted to fit the new entry (LRU order: oldest first).
    pub evicted: Vec<PlanCacheKey>,
}

/// Outcome of a `get` call when the entry was present and self-verified.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanCacheHit {
    pub plan: CompiledPlan,
    pub plan_tree_hash: String,
}

/// Whether a process-cache lookup reused an existing plan or compiled a new
/// one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanCacheDecision {
    Hit,
    Miss,
}

impl PlanCacheDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

/// Result returned by the synchronized process-cache helper.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanCacheLookup {
    pub decision: PlanCacheDecision,
    pub plan: CompiledPlan,
    pub plan_tree_hash: String,
    pub evicted: Vec<PlanCacheKey>,
}

#[derive(Debug)]
struct PlanCacheEntry {
    plan: CompiledPlan,
    plan_tree_hash: String,
    /// bd-25yao: atomic so the cache read path can update the LRU
    /// timestamp through a shared `&self` and the outer
    /// `RwLock<PlanCache>` can be acquired via `.read()` instead of
    /// `.lock()` for cache hits. `Relaxed` everywhere: eviction
    /// reads under the outer write lock (consistent within one pass)
    /// and `next_access_sequence` returns unique values via
    /// `fetch_add`.
    last_used_sequence: AtomicU64,
}

impl PlanCacheEntry {
    fn last_used(&self) -> u64 {
        self.last_used_sequence.load(Ordering::Relaxed)
    }

    fn touch(&self, last_used_sequence: u64) {
        self.last_used_sequence
            .store(last_used_sequence, Ordering::Relaxed);
    }
}

impl Clone for PlanCacheEntry {
    fn clone(&self) -> Self {
        Self {
            plan: self.plan.clone(),
            plan_tree_hash: self.plan_tree_hash.clone(),
            last_used_sequence: AtomicU64::new(self.last_used()),
        }
    }
}

/// Bounded, deterministic LRU cache for compiled EQL plans.
///
/// The cache is not internally synchronized; callers wrap it in
/// `parking_lot`-style or `std::sync::Mutex` when sharing across threads.
/// `&mut self` is used for mutations to match the codebase convention set by
/// `src/graph/ppr_prefetch_cache.rs`.
#[derive(Debug)]
pub struct PlanCache {
    capacity: usize,
    /// bd-25yao: atomic so `next_access_sequence`, hit/miss
    /// bookkeeping, and the LRU touch can all happen through a
    /// shared `&self`. Reads call `fetch_add(1, Relaxed)`; every
    /// caller observes a unique monotonically-increasing sequence
    /// number, and the LRU tie-break on lexical PlanCacheKey order
    /// in `lru_victim_key` keeps eviction deterministic.
    access_sequence: AtomicU64,
    entries: BTreeMap<PlanCacheKey, PlanCacheEntry>,
    // bd-25yao: counters become atomic so they can be incremented
    // from the read path (`get(&self)`). Observability semantics
    // are unchanged — `stats()` still samples a coherent-enough
    // snapshot for diagnostics (Relaxed allows brief inter-counter
    // skew but each counter's value is monotonic).
    hits: AtomicU64,
    misses: AtomicU64,
    inserts: AtomicU64,
    evictions: AtomicU64,
    invalidations: AtomicU64,
}

impl PlanCache {
    /// Build a new plan cache with the requested entry cap.
    ///
    /// A capacity of `0` disables caching: `insert` always evicts the entry
    /// immediately, `get` always reports a miss. Capacities above
    /// [`MAX_PLAN_CACHE_ENTRIES`] are silently clamped.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.min(MAX_PLAN_CACHE_ENTRIES);
        Self {
            capacity,
            access_sequence: AtomicU64::new(0),
            entries: BTreeMap::new(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            inserts: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Try to fetch a cached plan. The entry is self-verified before return;
    /// a corrupted entry (whose recomputed hash differs from the stored hash)
    /// reports a miss; the actual removal is **deferred** to the next
    /// mutating call path (next `insert` for the same key, next
    /// `invalidate_other_generations`, or the next `clear`).
    ///
    /// bd-25yao: this method takes `&self` so the outer
    /// `RwLock<PlanCache>` can be acquired via `.read()` and
    /// concurrent lookups parallelize. LRU bookkeeping, hit/miss
    /// counters, and the `next_access_sequence` bump all happen
    /// through atomics; eviction stays on the write path. Mirrors
    /// the bd-2lin9 / bd-1nan9 refactor; see `PprPrefetchCache::get`
    /// and `load_in_memory_algorithm_result` for the sibling shape.
    pub fn get(&self, key: &PlanCacheKey) -> Option<PlanCacheHit> {
        if let Some(hit) = self.get_without_miss_count(key) {
            return Some(hit);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Insert (or overwrite) the resolved plan for `key`. Returns the freshly
    /// computed plan-tree hash plus any LRU evictions triggered to fit the new
    /// entry.
    pub fn insert(&mut self, key: PlanCacheKey, plan: CompiledPlan) -> PlanCacheInsert {
        let plan_tree_hash = compute_plan_tree_hash(&key, &plan);
        if self.capacity == 0 {
            self.inserts.fetch_add(1, Ordering::Relaxed);
            // Capacity 0 means the cache is disabled; report success but keep
            // no entries so subsequent gets miss as documented.
            let dropped = self.entries.len();
            self.entries.clear();
            if dropped > 0 {
                self.invalidations
                    .fetch_add(dropped as u64, Ordering::Relaxed);
            }
            return PlanCacheInsert {
                plan_tree_hash,
                evicted: Vec::new(),
            };
        }
        let last_used_sequence = self.next_access_sequence();
        // bd-25yao: detect whether the natural BTreeMap::insert
        // overwrites a stale (hash-invalid) entry that earlier
        // `get` calls observed as a miss. Only the overwrite of a
        // hash-invalid prior entry counts as an invalidation; an
        // overwrite of a still-fresh entry is the normal cache
        // refresh path and must not double-count.
        // bd-25yao: present-but-hash-invalid prior entry → overwrite
        // counts as an invalidation. `entry_hash_is_valid` returns
        // false for both \"not present\" and \"present-and-stale\";
        // gate on `contains_key` to distinguish.
        let was_stale_overwrite =
            self.entries.contains_key(&key) && !self.entry_hash_is_valid(&key);
        self.entries.insert(
            key,
            PlanCacheEntry {
                plan,
                plan_tree_hash: plan_tree_hash.clone(),
                last_used_sequence: AtomicU64::new(last_used_sequence),
            },
        );
        if was_stale_overwrite {
            self.invalidations.fetch_add(1, Ordering::Relaxed);
        }
        self.inserts.fetch_add(1, Ordering::Relaxed);
        let evicted = self.evict_to_capacity();
        PlanCacheInsert {
            plan_tree_hash,
            evicted,
        }
    }

    /// Drop every entry whose key does not match `(index_manifest_version,
    /// search_config_hash)`. Useful when a manifest publish or config reload
    /// invalidates older plans without changing the eql hashes themselves.
    pub fn invalidate_other_generations(
        &mut self,
        index_manifest_version: u64,
        search_config_hash: u64,
    ) -> Vec<PlanCacheKey> {
        let stale: Vec<PlanCacheKey> = self
            .entries
            .keys()
            .filter(|key| {
                key.index_manifest_version != index_manifest_version
                    || key.search_config_hash != search_config_hash
            })
            .copied()
            .collect();
        for key in &stale {
            self.entries.remove(key);
        }
        if !stale.is_empty() {
            self.invalidations
                .fetch_add(stale.len() as u64, Ordering::Relaxed);
        }
        stale
    }

    /// Drop every cached plan. Stats counters are preserved so observers can
    /// distinguish "explicit clear" from "first launch".
    pub fn clear(&mut self) -> usize {
        let dropped = self.entries.len();
        self.entries.clear();
        if dropped > 0 {
            self.invalidations
                .fetch_add(dropped as u64, Ordering::Relaxed);
        }
        dropped
    }

    /// Sample current observability counters.
    #[must_use]
    pub fn stats(&self) -> PlanCacheStats {
        PlanCacheStats {
            capacity: self.capacity,
            current_size: self.entries.len(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            inserts: self.inserts.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
        }
    }

    /// Iterate cached keys in deterministic order (sorted). Intended for
    /// `ee diag plan-cache --json` once the diag surface lands.
    pub fn cached_keys(&self) -> impl Iterator<Item = PlanCacheKey> + '_ {
        self.entries.keys().copied()
    }

    fn next_access_sequence(&self) -> u64 {
        // bd-25yao: matches the bd-2lin9 / bd-1nan9 pattern:
        // fetch_add returns the OLD value, so add 1 to preserve
        // the "first call returns 1" semantics of the previous
        // saturating_add path.
        self.access_sequence
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    fn evict_to_capacity(&mut self) -> Vec<PlanCacheKey> {
        let mut evicted = Vec::new();
        while self.entries.len() > self.capacity {
            let Some(victim) = self.lru_victim_key() else {
                break;
            };
            self.entries.remove(&victim);
            self.evictions.fetch_add(1, Ordering::Relaxed);
            evicted.push(victim);
        }
        evicted
    }

    fn lru_victim_key(&self) -> Option<PlanCacheKey> {
        self.entries
            .iter()
            .min_by(|(left_key, left_entry), (right_key, right_entry)| {
                // bd-25yao: Relaxed atomic load. Eviction always
                // runs under the outer write lock (insert /
                // invalidate_other_generations / clear all take
                // `&mut self`), so the snapshot read here is
                // consistent within one eviction pass.
                left_entry
                    .last_used()
                    .cmp(&right_entry.last_used())
                    .then_with(|| left_key.cmp(right_key))
            })
            .map(|(key, _)| *key)
    }

    fn entry_hash_is_valid(&self, key: &PlanCacheKey) -> bool {
        let Some(entry) = self.entries.get(key) else {
            return false;
        };
        compute_plan_tree_hash(key, &entry.plan) == entry.plan_tree_hash
    }

    fn get_without_miss_count(&self, key: &PlanCacheKey) -> Option<PlanCacheHit> {
        if self.capacity == 0 {
            return None;
        }
        if !self.entry_hash_is_valid(key) {
            // Safety-critical contract: corrupted hits never leak.
            // Eviction is deferred — the next mutating call path
            // (`insert`, `invalidate_other_generations`, `clear`)
            // reclaims the slot and bumps `invalidations` so actual
            // removals, not observed-stale reads, are counted.
            return None;
        }
        let entry = self.entries.get(key)?;
        entry.touch(self.next_access_sequence());
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(PlanCacheHit {
            plan: entry.plan.clone(),
            plan_tree_hash: entry.plan_tree_hash.clone(),
        })
    }
}

/// Look up a compiled plan in the process-wide cache, compiling and inserting
/// on miss. The `capacity` argument is the resolved runtime cap; changing it
/// resets the process cache so diagnostics and search behavior agree.
pub fn lookup_or_insert_process_plan<F>(
    capacity: usize,
    key: PlanCacheKey,
    compile: F,
) -> PlanCacheLookup
where
    F: FnOnce() -> CompiledPlan,
{
    let cache = PROCESS_PLAN_CACHE.get_or_init(|| RwLock::new(PlanCache::new(capacity)));
    let bounded_capacity = capacity.min(MAX_PLAN_CACHE_ENTRIES);

    // bd-25yao: double-checked locking. Take the shared lock on the
    // hot path so concurrent cache-hit lookups parallelize. We must
    // verify the capacity hasn't changed under the shared lock; if
    // it has, fall through to the write path to reset the cache.
    {
        let read_guard = cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if read_guard.capacity() == bounded_capacity
            && let Some(hit) = read_guard.get(&key)
        {
            return PlanCacheLookup {
                decision: PlanCacheDecision::Hit,
                plan: hit.plan,
                plan_tree_hash: hit.plan_tree_hash,
                evicted: Vec::new(),
            };
        }
    }

    let mut guard = cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.capacity() != bounded_capacity {
        *guard = PlanCache::new(capacity);
    }

    // Re-check after acquiring the write lock: another writer may
    // have inserted the same key while we were waiting. Do not call
    // the public `get` here: the shared-lock lookup already charged
    // the miss for this logical request, and an absent write-lock
    // recheck must not double-count it.
    if let Some(hit) = guard.get_without_miss_count(&key) {
        return PlanCacheLookup {
            decision: PlanCacheDecision::Hit,
            plan: hit.plan,
            plan_tree_hash: hit.plan_tree_hash,
            evicted: Vec::new(),
        };
    }

    let plan = compile();
    let inserted = guard.insert(key, plan.clone());
    PlanCacheLookup {
        decision: PlanCacheDecision::Miss,
        plan,
        plan_tree_hash: inserted.plan_tree_hash,
        evicted: inserted.evicted,
    }
}

/// Build a diagnostic snapshot from the live process cache.
#[must_use]
pub fn process_plan_cache_diag_report(
    capacity: usize,
    env_var_value_source: EnvVarValueSource,
    top_keys_limit: usize,
) -> PlanCacheDiagReport {
    let cache = PROCESS_PLAN_CACHE.get_or_init(|| RwLock::new(PlanCache::new(capacity)));
    let mut guard = cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.capacity() != capacity.min(MAX_PLAN_CACHE_ENTRIES) {
        *guard = PlanCache::new(capacity);
    }
    guard.diag_report(env_var_value_source, top_keys_limit)
}

/// Test-only reset hook for the process cache. Kept public so integration tests
/// can pin the live-counter contract without reaching into private statics.
#[doc(hidden)]
pub fn reset_process_plan_cache_for_tests(capacity: usize) {
    let cache = PROCESS_PLAN_CACHE.get_or_init(|| RwLock::new(PlanCache::new(capacity)));
    let mut guard = cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = PlanCache::new(capacity);
}

/// Compute the 64-bit EQL request hash used as the first cache-key component.
///
/// Callers pass the canonical bytes of the request (e.g. the serialized JSON
/// EQL document). The hash is domain-separated so it cannot collide with
/// other plan-cache hashes.
#[must_use]
pub fn compute_eql_hash(canonical_request_bytes: &[u8]) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EQL_HASH_DOMAIN);
    hasher.update(&(canonical_request_bytes.len() as u64).to_le_bytes());
    hasher.update(canonical_request_bytes);
    truncate_to_u64(hasher.finalize().as_bytes())
}

/// Compute the 64-bit search-config hash used as the third cache-key
/// component. Callers serialize the resolved `SearchScoringConfig` (or
/// equivalent) before hashing.
#[must_use]
pub fn compute_search_config_hash(canonical_config_bytes: &[u8]) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SEARCH_CONFIG_HASH_DOMAIN);
    hasher.update(&(canonical_config_bytes.len() as u64).to_le_bytes());
    hasher.update(canonical_config_bytes);
    truncate_to_u64(hasher.finalize().as_bytes())
}

/// Compute the canonical plan-tree hash used both for entry verification and
/// for cross-process equality checks ("identical EQL → identical plan-tree
/// hash" per bead acceptance #1).
#[must_use]
pub fn compute_plan_tree_hash(key: &PlanCacheKey, plan: &CompiledPlan) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PLAN_TREE_HASH_DOMAIN);
    hasher.update(&key.eql_hash.to_le_bytes());
    hasher.update(&key.index_manifest_version.to_le_bytes());
    hasher.update(&key.search_config_hash.to_le_bytes());
    let CompiledPlan {
        parsed_query,
        bound_index,
        join_strategy,
    } = plan;
    // bd-2lzls: destructure EqlQuery exhaustively so any field added later
    // fails to compile here until it is folded into the plan-tree hash, instead
    // of being silently dropped (the failure mode that omitted
    // time/confidence/graph). Hash order is unchanged.
    let EqlQuery {
        q,
        workspace,
        levels,
        kinds,
        tags,
        tags_mode,
        scope,
        time,
        confidence,
        graph,
        limit,
        speed,
        rerank,
        return_subgraph,
        explain,
    } = parsed_query;
    hash_str(&mut hasher, q);
    hash_optional_str(&mut hasher, workspace.as_deref());
    hash_str_list(&mut hasher, levels);
    hash_str_list(&mut hasher, kinds);
    hash_str_list(&mut hasher, tags);
    hasher.update(tags_mode.as_str().as_bytes());
    hash_str_list(&mut hasher, scope);
    hash_optional_time_filter(&mut hasher, time.as_ref());
    hash_optional_confidence_filter(&mut hasher, confidence.as_ref());
    hash_optional_graph_filter(&mut hasher, graph.as_ref());
    hasher.update(&limit.to_le_bytes());
    hasher.update(speed.as_str().as_bytes());
    hasher.update(&[u8::from(*rerank)]);
    hasher.update(&[u8::from(*return_subgraph)]);
    hasher.update(&[u8::from(*explain)]);
    hash_optional_str(&mut hasher, bound_index.as_deref());
    hash_optional_str(&mut hasher, join_strategy.as_deref());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_str(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_optional_str(hasher: &mut blake3::Hasher, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_str(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_time_filter(
    hasher: &mut blake3::Hasher,
    value: Option<&crate::models::query::EqlTimeFilter>,
) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_optional_str(hasher, value.since.as_deref());
            hash_optional_str(hasher, value.until.as_deref());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_confidence_filter(
    hasher: &mut blake3::Hasher,
    value: Option<&crate::models::query::EqlConfidenceFilter>,
) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_optional_f64(hasher, value.min);
            hash_optional_f64(hasher, value.max);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_graph_filter(
    hasher: &mut blake3::Hasher,
    value: Option<&crate::models::query::EqlGraphFilter>,
) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_optional_str(hasher, value.center.as_deref());
            match value.hops {
                Some(hops) => {
                    hasher.update(&[1]);
                    hasher.update(&hops.to_le_bytes());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
            hash_str_list(hasher, &value.relations);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_optional_f64(hasher: &mut blake3::Hasher, value: Option<f64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_bits().to_le_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_str_list(hasher: &mut blake3::Hasher, values: &[String]) {
    hasher.update(&(values.len() as u64).to_le_bytes());
    for value in values {
        hash_str(hasher, value);
    }
}

fn truncate_to_u64(hash: &[u8; 32]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&hash[0..8]);
    u64::from_le_bytes(buf)
}

/// Stable schema tag emitted by `ee diag plan-cache --json`. Matches the
/// `schemaTag` const in `docs/schemas/ee.diag.plan_cache.v1.json`.
pub const PLAN_CACHE_DIAG_SCHEMA_V1: &str = "ee.diag.plan_cache.v1";

/// Stable name of the environment variable that controls cache capacity.
/// Mirrored in `src/config/env_registry.rs`; declared here so the diag
/// report payload stays self-contained.
pub const PLAN_CACHE_ENV_VAR_NAME: &str = "EE_QUERY_PLAN_CACHE_ENTRIES";

/// How the resolved capacity value was sourced. Renders as the
/// `envVarValueSource` field in the diag JSON contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvVarValueSource {
    /// Capacity came from the `EnvVar::default_value` registry entry.
    RegistryDefault,
    /// Capacity came from a workspace or user TOML config override.
    OperatorOverride,
    /// Capacity came from the `EE_QUERY_PLAN_CACHE_ENTRIES` process env var.
    ProcessEnv,
}

/// Serializable cache key shape used by the diag report. Field names match
/// the camelCase keys declared in `docs/schemas/ee.diag.plan_cache.v1.json`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCacheDiagKey {
    pub eql_hash: u64,
    pub index_manifest_version: u64,
    pub search_config_hash: u64,
}

impl From<PlanCacheKey> for PlanCacheDiagKey {
    fn from(value: PlanCacheKey) -> Self {
        Self {
            eql_hash: value.eql_hash,
            index_manifest_version: value.index_manifest_version,
            search_config_hash: value.search_config_hash,
        }
    }
}

/// Serializable diagnostic report for the EQL plan cache. Designed to be
/// dropped straight into the `data.report` slot of the
/// `ee.diag.plan_cache.v1` response envelope.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCacheDiagReport {
    pub schema_tag: &'static str,
    pub enabled: bool,
    pub capacity: usize,
    pub current_size: usize,
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    pub invalidations: u64,
    pub hit_rate: Option<f64>,
    pub env_var_name: &'static str,
    pub env_var_value_source: EnvVarValueSource,
    pub top_keys: Vec<PlanCacheDiagKey>,
}

impl PlanCache {
    /// Build a [`PlanCacheDiagReport`] from the current cache state and the
    /// declared configuration source.
    ///
    /// `top_keys_limit` caps the number of cached keys reported back. Pass
    /// `usize::MAX` for "no limit". Keys are returned in the deterministic
    /// sort order produced by [`PlanCache::cached_keys`].
    #[must_use]
    pub fn diag_report(
        &self,
        env_var_value_source: EnvVarValueSource,
        top_keys_limit: usize,
    ) -> PlanCacheDiagReport {
        let stats = self.stats();
        let hit_rate = compute_hit_rate(stats.hits, stats.misses);
        let top_keys = self
            .cached_keys()
            .take(top_keys_limit)
            .map(PlanCacheDiagKey::from)
            .collect();
        PlanCacheDiagReport {
            schema_tag: PLAN_CACHE_DIAG_SCHEMA_V1,
            enabled: stats.capacity > 0,
            capacity: stats.capacity,
            current_size: stats.current_size,
            hits: stats.hits,
            misses: stats.misses,
            inserts: stats.inserts,
            evictions: stats.evictions,
            invalidations: stats.invalidations,
            hit_rate,
            env_var_name: PLAN_CACHE_ENV_VAR_NAME,
            env_var_value_source,
            top_keys,
        }
    }
}

fn compute_hit_rate(hits: u64, misses: u64) -> Option<f64> {
    let hits = hits as f64;
    let misses = misses as f64;
    let total = hits + misses;
    if total == 0.0 || !total.is_finite() {
        return None;
    }
    Some((hits / total).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::query::{EqlSpeedMode, EqlTagsMode};

    fn sample_query(q: &str) -> EqlQuery {
        EqlQuery {
            q: q.to_string(),
            workspace: None,
            levels: Vec::new(),
            kinds: Vec::new(),
            tags: Vec::new(),
            tags_mode: EqlTagsMode::Any,
            scope: Vec::new(),
            time: None,
            confidence: None,
            graph: None,
            limit: 10,
            speed: EqlSpeedMode::Default,
            rerank: false,
            return_subgraph: false,
            explain: false,
        }
    }

    fn sample_plan(q: &str) -> CompiledPlan {
        CompiledPlan::from_query(sample_query(q))
    }

    fn key(eql: u64, manifest: u64, cfg: u64) -> PlanCacheKey {
        PlanCacheKey::new(eql, manifest, cfg)
    }

    #[test]
    fn time_confidence_graph_changes_plan_tree_hash_bd_2lzls() {
        use crate::models::query::{EqlConfidenceFilter, EqlGraphFilter, EqlTimeFilter};

        // bd-2lzls: plans that differ only in time/confidence/graph must produce
        // distinct plan-tree hashes. Before the fix these fields were not hashed,
        // so distinct plans collided on the hash. Hold the key fixed so the
        // distinction can only come from the hashed plan fields.
        let k = key(1, 10, 100);
        let base_hash = compute_plan_tree_hash(&k, &sample_plan("alpha"));

        let mut time_query = sample_query("alpha");
        time_query.time = Some(EqlTimeFilter {
            since: Some("2026-01-01T00:00:00Z".to_string()),
            until: None,
        });
        assert_ne!(
            base_hash,
            compute_plan_tree_hash(&k, &CompiledPlan::from_query(time_query)),
            "time filter must change the plan-tree hash"
        );

        let mut confidence_query = sample_query("alpha");
        confidence_query.confidence = Some(EqlConfidenceFilter {
            min: Some(0.5),
            max: None,
        });
        assert_ne!(
            base_hash,
            compute_plan_tree_hash(&k, &CompiledPlan::from_query(confidence_query)),
            "confidence filter must change the plan-tree hash"
        );

        let mut graph_query = sample_query("alpha");
        graph_query.graph = Some(EqlGraphFilter {
            center: Some("m1".to_string()),
            hops: Some(2),
            relations: vec!["rel".to_string()],
        });
        assert_ne!(
            base_hash,
            compute_plan_tree_hash(&k, &CompiledPlan::from_query(graph_query)),
            "graph filter must change the plan-tree hash"
        );
    }

    #[test]
    fn new_cache_is_empty_and_records_default_stats() {
        let cache = PlanCache::new(4);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
        let stats = cache.stats();
        assert_eq!(stats.capacity, 4);
        assert_eq!(stats.current_size, 0);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.inserts, 0);
        assert_eq!(stats.evictions, 0);
    }

    #[test]
    fn insert_then_get_returns_the_same_plan_and_increments_hits() {
        let mut cache = PlanCache::new(4);
        let plan = sample_plan("alpha");
        let inserted = cache.insert(key(1, 10, 100), plan.clone());
        let hit = cache.get(&key(1, 10, 100)).expect("expected hit");
        assert_eq!(hit.plan, plan);
        assert_eq!(hit.plan_tree_hash, inserted.plan_tree_hash);
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.inserts, 1);
    }

    #[test]
    fn get_with_unknown_key_misses_and_increments_misses() {
        let mut cache = PlanCache::new(2);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        assert!(cache.get(&key(2, 10, 100)).is_none());
        assert!(cache.get(&key(1, 11, 100)).is_none());
        assert!(cache.get(&key(1, 10, 101)).is_none());
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 3);
    }

    #[test]
    fn identical_eql_yields_identical_plan_tree_hash() {
        // Bead acceptance #1: same EQL must produce the same plan-tree hash.
        let mut cache_a = PlanCache::new(4);
        let mut cache_b = PlanCache::new(4);
        let plan_a = sample_plan("release rules");
        let plan_b = sample_plan("release rules");
        let inserted_a = cache_a.insert(key(7, 42, 17), plan_a);
        let inserted_b = cache_b.insert(key(7, 42, 17), plan_b);
        assert_eq!(inserted_a.plan_tree_hash, inserted_b.plan_tree_hash);
    }

    #[test]
    fn different_query_text_produces_different_plan_tree_hash() {
        let mut cache = PlanCache::new(4);
        let inserted_a = cache.insert(key(1, 10, 100), sample_plan("alpha"));
        let inserted_b = cache.insert(key(2, 10, 100), sample_plan("beta"));
        assert_ne!(inserted_a.plan_tree_hash, inserted_b.plan_tree_hash);
    }

    #[test]
    fn manifest_version_bump_invalidates_old_entries_via_key() {
        // Bead acceptance #2: bumping manifest invalidates entries by key.
        let mut cache = PlanCache::new(4);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        assert!(cache.get(&key(1, 11, 100)).is_none());
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn search_config_hash_change_invalidates_entries_via_key() {
        // Bead acceptance #3: search-config hash bump invalidates via key.
        let mut cache = PlanCache::new(4);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        assert!(cache.get(&key(1, 10, 999)).is_none());
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn lru_eviction_drops_oldest_entry_when_capacity_exceeded() {
        let mut cache = PlanCache::new(2);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        cache.insert(key(2, 10, 100), sample_plan("beta"));
        // Touch alpha so beta becomes the LRU victim when gamma arrives.
        let _ = cache.get(&key(1, 10, 100));
        let inserted = cache.insert(key(3, 10, 100), sample_plan("gamma"));
        assert_eq!(inserted.evicted, vec![key(2, 10, 100)]);
        assert!(cache.get(&key(1, 10, 100)).is_some());
        assert!(cache.get(&key(2, 10, 100)).is_none());
        assert!(cache.get(&key(3, 10, 100)).is_some());
        let stats = cache.stats();
        assert_eq!(stats.evictions, 1);
        assert_eq!(stats.current_size, 2);
    }

    #[test]
    fn zero_capacity_disables_storage_and_every_get_is_a_miss() {
        let mut cache = PlanCache::new(0);
        let inserted = cache.insert(key(1, 10, 100), sample_plan("alpha"));
        assert!(inserted.plan_tree_hash.starts_with("blake3:"));
        assert!(cache.get(&key(1, 10, 100)).is_none());
        assert_eq!(cache.len(), 0);
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.inserts, 1);
    }

    #[test]
    fn invalidate_other_generations_drops_non_matching_keys() {
        let mut cache = PlanCache::new(8);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        cache.insert(key(2, 11, 100), sample_plan("beta"));
        cache.insert(key(3, 10, 200), sample_plan("gamma"));
        let stale = cache.invalidate_other_generations(10, 100);
        assert_eq!(stale.len(), 2);
        assert!(stale.contains(&key(2, 11, 100)));
        assert!(stale.contains(&key(3, 10, 200)));
        assert!(cache.get(&key(1, 10, 100)).is_some());
    }

    #[test]
    fn clear_drops_all_entries_and_records_invalidation_count() {
        let mut cache = PlanCache::new(4);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        cache.insert(key(2, 10, 100), sample_plan("beta"));
        let dropped = cache.clear();
        assert_eq!(dropped, 2);
        assert!(cache.is_empty());
        assert_eq!(cache.stats().invalidations, 2);
    }

    #[test]
    fn compute_eql_hash_is_domain_separated_from_search_config_hash() {
        let bytes = b"{\"q\":\"alpha\"}";
        let eql = compute_eql_hash(bytes);
        let cfg = compute_search_config_hash(bytes);
        assert_ne!(
            eql, cfg,
            "domain-separated hashes must not collide for identical inputs"
        );
    }

    #[test]
    fn compute_eql_hash_is_deterministic_across_calls() {
        let bytes = b"{\"q\":\"release rules\"}";
        assert_eq!(compute_eql_hash(bytes), compute_eql_hash(bytes));
    }

    #[test]
    fn cached_keys_iterates_in_sorted_order() {
        let mut cache = PlanCache::new(4);
        cache.insert(key(3, 10, 100), sample_plan("gamma"));
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        cache.insert(key(2, 10, 100), sample_plan("beta"));
        let keys: Vec<_> = cache.cached_keys().collect();
        assert_eq!(
            keys,
            vec![key(1, 10, 100), key(2, 10, 100), key(3, 10, 100),]
        );
    }

    // bd-25yao: after the RwLock + atomic-LRU refactor, `get(&self)`
    // CANNOT remove the corrupted entry in place — eviction stays on
    // the mutating call path. The safety-critical contract is
    // preserved (corrupted hits never leak; `get` returns None), and
    // the actual removal + invalidation counter bump fires when the
    // next `insert` for the same key overwrites the stale slot or
    // when `invalidate_other_generations` / `clear` runs.
    #[test]
    fn corrupted_entry_returns_miss_and_lingers_until_next_insert() {
        let mut cache = PlanCache::new(2);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        // Reach into the entry and rewrite the persisted plan-tree hash to a
        // value that no longer matches the stored plan. The next get must
        // detect the mismatch and report a miss.
        let entry = cache
            .entries
            .get_mut(&key(1, 10, 100))
            .expect("entry inserted above");
        entry.plan_tree_hash = "blake3:deadbeef".to_string();

        // Safety-critical: corrupted hit returns None.
        assert!(cache.get(&key(1, 10, 100)).is_none());
        // Stale entry lingers; invalidation counter has NOT been
        // bumped on the read path (deferred semantics).
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.stats().invalidations, 0);

        // A subsequent insert for the same key overwrites the stale
        // entry and the deferred invalidation counter fires.
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        assert_eq!(cache.stats().invalidations, 1);
        // Post-refresh hit succeeds against the new entry.
        assert!(cache.get(&key(1, 10, 100)).is_some());
    }

    #[test]
    fn capacity_is_clamped_to_documented_max() {
        let cache = PlanCache::new(usize::MAX);
        assert_eq!(cache.capacity(), MAX_PLAN_CACHE_ENTRIES);
    }

    #[test]
    fn default_plan_cache_entries_matches_documented_default() {
        // Acceptance: bounded memory (default 1024 entries) is documented and
        // exposed as a public constant so the env-registry default stays in
        // sync with this module's intent.
        assert_eq!(DEFAULT_PLAN_CACHE_ENTRIES, 1024);
    }

    #[test]
    fn diag_report_reports_enabled_state_and_default_counters() {
        let cache = PlanCache::new(4);
        let report = cache.diag_report(EnvVarValueSource::RegistryDefault, 8);
        assert_eq!(report.schema_tag, PLAN_CACHE_DIAG_SCHEMA_V1);
        assert!(report.enabled);
        assert_eq!(report.capacity, 4);
        assert_eq!(report.current_size, 0);
        assert_eq!(report.hits, 0);
        assert_eq!(report.misses, 0);
        assert_eq!(report.hit_rate, None);
        assert_eq!(report.env_var_name, PLAN_CACHE_ENV_VAR_NAME);
        assert_eq!(
            report.env_var_value_source,
            EnvVarValueSource::RegistryDefault
        );
        assert!(report.top_keys.is_empty());
    }

    #[test]
    fn diag_report_reports_disabled_state_when_capacity_is_zero() {
        let cache = PlanCache::new(0);
        let report = cache.diag_report(EnvVarValueSource::OperatorOverride, 8);
        assert!(!report.enabled);
        assert_eq!(report.capacity, 0);
        assert_eq!(
            report.env_var_value_source,
            EnvVarValueSource::OperatorOverride
        );
    }

    #[test]
    fn diag_report_computes_hit_rate_after_observed_lookups() {
        let mut cache = PlanCache::new(4);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        cache.insert(key(2, 10, 100), sample_plan("beta"));
        // 2 hits, 1 miss
        assert!(cache.get(&key(1, 10, 100)).is_some());
        assert!(cache.get(&key(2, 10, 100)).is_some());
        assert!(cache.get(&key(3, 10, 100)).is_none());

        let report = cache.diag_report(EnvVarValueSource::ProcessEnv, 8);
        assert_eq!(report.hits, 2);
        assert_eq!(report.misses, 1);
        let rate = report
            .hit_rate
            .expect("hit_rate present after observed lookups");
        assert!((rate - (2.0 / 3.0)).abs() < 1e-12);
        assert_eq!(report.current_size, 2);
        assert_eq!(report.top_keys.len(), 2);
        assert_eq!(
            report.top_keys.first(),
            Some(&PlanCacheDiagKey::from(key(1, 10, 100))),
        );
    }

    #[test]
    fn diag_report_caps_top_keys_at_caller_limit() {
        let mut cache = PlanCache::new(8);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        cache.insert(key(2, 10, 100), sample_plan("beta"));
        cache.insert(key(3, 10, 100), sample_plan("gamma"));
        let report = cache.diag_report(EnvVarValueSource::RegistryDefault, 2);
        assert_eq!(report.top_keys.len(), 2);
        // Confirm the first cap returns the two smallest keys in sort order:
        assert_eq!(
            report.top_keys,
            vec![
                PlanCacheDiagKey::from(key(1, 10, 100)),
                PlanCacheDiagKey::from(key(2, 10, 100)),
            ],
        );
    }

    #[test]
    fn diag_report_serializes_to_camel_case_json_matching_schema() {
        let mut cache = PlanCache::new(2);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        let report = cache.diag_report(EnvVarValueSource::RegistryDefault, 4);
        let json = serde_json::to_value(&report).expect("report serializes");
        let object = json.as_object().expect("report is a JSON object");
        // Schema-required field names (all camelCase):
        for required in [
            "schemaTag",
            "enabled",
            "capacity",
            "currentSize",
            "hits",
            "misses",
            "inserts",
            "evictions",
            "invalidations",
            "hitRate",
            "envVarName",
            "envVarValueSource",
            "topKeys",
        ] {
            assert!(
                object.contains_key(required),
                "missing field {required} in {json}"
            );
        }
        // Per-key camelCase fields:
        let first_key = object
            .get("topKeys")
            .and_then(|value| value.as_array())
            .and_then(|array| array.first())
            .expect("topKeys has at least one entry");
        for required in ["eqlHash", "indexManifestVersion", "searchConfigHash"] {
            assert!(
                first_key
                    .as_object()
                    .map(|inner| inner.contains_key(required))
                    .unwrap_or(false),
                "missing cache-key field {required} in {first_key}"
            );
        }
        // env_var_value_source uses snake_case via serde rename:
        assert_eq!(
            object.get("envVarValueSource").and_then(|v| v.as_str()),
            Some("registry_default")
        );
        assert_eq!(
            object.get("schemaTag").and_then(|v| v.as_str()),
            Some("ee.diag.plan_cache.v1"),
        );
    }

    #[test]
    fn compute_hit_rate_returns_none_when_no_observations() {
        assert_eq!(compute_hit_rate(0, 0), None);
    }

    #[test]
    fn compute_hit_rate_returns_zero_when_only_misses() {
        assert_eq!(compute_hit_rate(0, 5), Some(0.0));
    }

    #[test]
    fn compute_hit_rate_returns_one_when_only_hits() {
        assert_eq!(compute_hit_rate(5, 0), Some(1.0));
    }

    #[test]
    fn compute_hit_rate_handles_large_counters_without_integer_overflow() {
        let rate = compute_hit_rate(u64::MAX, u64::MAX)
            .expect("large observed counters still have a rate");

        assert!(
            (rate - 0.5).abs() < 1e-12,
            "hit_rate should not disappear when integer totals overflow"
        );
    }
}
