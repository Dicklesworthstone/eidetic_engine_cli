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
//! Bead: `bd-2mey5`. Honesty: this slice ships the cache module, stats, and
//! key/hash discipline; hooking `run_search_inner` to actually consult the
//! cache (and the matching `ee diag plan-cache --json` surface) is tracked
//! separately and will land in a follow-up bead before `bd-2mey5` closes.

use std::collections::BTreeMap;

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

#[derive(Clone, Debug)]
struct PlanCacheEntry {
    plan: CompiledPlan,
    plan_tree_hash: String,
    last_used_sequence: u64,
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
    access_sequence: u64,
    entries: BTreeMap<PlanCacheKey, PlanCacheEntry>,
    hits: u64,
    misses: u64,
    inserts: u64,
    evictions: u64,
    invalidations: u64,
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
            access_sequence: 0,
            entries: BTreeMap::new(),
            hits: 0,
            misses: 0,
            inserts: 0,
            evictions: 0,
            invalidations: 0,
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
    /// is dropped and the call reports a miss.
    pub fn get(&mut self, key: &PlanCacheKey) -> Option<PlanCacheHit> {
        if self.capacity == 0 {
            self.misses = self.misses.saturating_add(1);
            return None;
        }
        if !self.entry_hash_is_valid(key) {
            if self.entries.remove(key).is_some() {
                self.invalidations = self.invalidations.saturating_add(1);
            }
            self.misses = self.misses.saturating_add(1);
            return None;
        }
        let last_used_sequence = self.next_access_sequence();
        let entry = self.entries.get_mut(key)?;
        entry.last_used_sequence = last_used_sequence;
        self.hits = self.hits.saturating_add(1);
        Some(PlanCacheHit {
            plan: entry.plan.clone(),
            plan_tree_hash: entry.plan_tree_hash.clone(),
        })
    }

    /// Insert (or overwrite) the resolved plan for `key`. Returns the freshly
    /// computed plan-tree hash plus any LRU evictions triggered to fit the new
    /// entry.
    pub fn insert(&mut self, key: PlanCacheKey, plan: CompiledPlan) -> PlanCacheInsert {
        let plan_tree_hash = compute_plan_tree_hash(&key, &plan);
        if self.capacity == 0 {
            self.inserts = self.inserts.saturating_add(1);
            // Capacity 0 means the cache is disabled; report success but keep
            // no entries so subsequent gets miss as documented.
            self.entries.clear();
            return PlanCacheInsert {
                plan_tree_hash,
                evicted: Vec::new(),
            };
        }
        let last_used_sequence = self.next_access_sequence();
        self.entries.insert(
            key,
            PlanCacheEntry {
                plan,
                plan_tree_hash: plan_tree_hash.clone(),
                last_used_sequence,
            },
        );
        self.inserts = self.inserts.saturating_add(1);
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
            self.invalidations = self.invalidations.saturating_add(stale.len() as u64);
        }
        stale
    }

    /// Drop every cached plan. Stats counters are preserved so observers can
    /// distinguish "explicit clear" from "first launch".
    pub fn clear(&mut self) -> usize {
        let dropped = self.entries.len();
        self.entries.clear();
        if dropped > 0 {
            self.invalidations = self.invalidations.saturating_add(dropped as u64);
        }
        dropped
    }

    /// Sample current observability counters.
    #[must_use]
    pub fn stats(&self) -> PlanCacheStats {
        PlanCacheStats {
            capacity: self.capacity,
            current_size: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            inserts: self.inserts,
            evictions: self.evictions,
            invalidations: self.invalidations,
        }
    }

    /// Iterate cached keys in deterministic order (sorted). Intended for
    /// `ee diag plan-cache --json` once the diag surface lands.
    pub fn cached_keys(&self) -> impl Iterator<Item = PlanCacheKey> + '_ {
        self.entries.keys().copied()
    }

    fn next_access_sequence(&mut self) -> u64 {
        self.access_sequence = self.access_sequence.saturating_add(1);
        self.access_sequence
    }

    fn evict_to_capacity(&mut self) -> Vec<PlanCacheKey> {
        let mut evicted = Vec::new();
        while self.entries.len() > self.capacity {
            let Some(victim) = self.lru_victim_key() else {
                break;
            };
            self.entries.remove(&victim);
            self.evictions = self.evictions.saturating_add(1);
            evicted.push(victim);
        }
        evicted
    }

    fn lru_victim_key(&self) -> Option<PlanCacheKey> {
        self.entries
            .iter()
            .min_by(|(left_key, left_entry), (right_key, right_entry)| {
                left_entry
                    .last_used_sequence
                    .cmp(&right_entry.last_used_sequence)
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
    hash_str(&mut hasher, &parsed_query.q);
    hash_optional_str(&mut hasher, parsed_query.workspace.as_deref());
    hash_str_list(&mut hasher, &parsed_query.levels);
    hash_str_list(&mut hasher, &parsed_query.kinds);
    hash_str_list(&mut hasher, &parsed_query.tags);
    hasher.update(parsed_query.tags_mode.as_str().as_bytes());
    hash_str_list(&mut hasher, &parsed_query.scope);
    hasher.update(&parsed_query.limit.to_le_bytes());
    hasher.update(parsed_query.speed.as_str().as_bytes());
    hasher.update(&[u8::from(parsed_query.rerank)]);
    hasher.update(&[u8::from(parsed_query.return_subgraph)]);
    hasher.update(&[u8::from(parsed_query.explain)]);
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

    #[test]
    fn corrupted_entry_returns_miss_without_panic() {
        let mut cache = PlanCache::new(2);
        cache.insert(key(1, 10, 100), sample_plan("alpha"));
        // Reach into the entry and rewrite the persisted plan-tree hash to a
        // value that no longer matches the stored plan. The next get must
        // detect the mismatch, drop the entry, and report a miss.
        let entry = cache
            .entries
            .get_mut(&key(1, 10, 100))
            .expect("entry inserted above");
        entry.plan_tree_hash = "blake3:deadbeef".to_string();
        assert!(cache.get(&key(1, 10, 100)).is_none());
        assert!(cache.is_empty());
        assert_eq!(cache.stats().invalidations, 1);
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
}
