//! Integration smoke tests for the EQL query plan cache.
//!
//! Inline `#[cfg(test)]` units in `src/search/plan_cache.rs` cover the cache's
//! happy path, edge cases, and self-verification. This file pins the
//! public-API guarantees from outside the crate (the shape an eventual
//! `run_search_inner` integration will rely on) and bumps coverage on the
//! determinism contract that bead `bd-2mey5` requires.

use ee::models::query::{EqlQuery, EqlSpeedMode, EqlTagsMode};
use ee::search::plan_cache::{
    CompiledPlan, DEFAULT_PLAN_CACHE_ENTRIES, MAX_PLAN_CACHE_ENTRIES, PlanCache, PlanCacheKey,
    compute_eql_hash, compute_plan_tree_hash, compute_search_config_hash,
};

fn sample_query(q: &str, limit: u32) -> EqlQuery {
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
        limit,
        speed: EqlSpeedMode::Default,
        rerank: false,
        return_subgraph: false,
        explain: false,
    }
}

fn sample_plan(q: &str, limit: u32) -> CompiledPlan {
    CompiledPlan::from_query(sample_query(q, limit))
}

#[test]
fn public_api_round_trips_a_plan_under_default_capacity() {
    let mut cache = PlanCache::new(DEFAULT_PLAN_CACHE_ENTRIES);
    let key = PlanCacheKey::new(1, 1, 1);
    let plan = sample_plan("release rules", 10);
    let inserted = cache.insert(key, plan.clone());
    let hit = cache.get(&key).expect("expected the just-inserted plan");
    assert_eq!(hit.plan, plan);
    assert_eq!(hit.plan_tree_hash, inserted.plan_tree_hash);
    assert!(hit.plan_tree_hash.starts_with("blake3:"));
}

#[test]
fn identical_eql_yields_identical_plan_tree_hash_across_independent_caches() {
    // Bead acceptance #1: same input → same plan-tree hash.
    let mut cache_a = PlanCache::new(8);
    let mut cache_b = PlanCache::new(8);
    let key = PlanCacheKey::new(7, 42, 17);
    let plan_a = sample_plan("prepare release", 10);
    let plan_b = sample_plan("prepare release", 10);
    let hash_a = cache_a.insert(key, plan_a).plan_tree_hash;
    let hash_b = cache_b.insert(key, plan_b).plan_tree_hash;
    assert_eq!(hash_a, hash_b);

    // And the same hash drops out of `compute_plan_tree_hash` directly:
    let direct = compute_plan_tree_hash(&key, &sample_plan("prepare release", 10));
    assert_eq!(direct, hash_a);
}

#[test]
fn limit_or_query_change_changes_plan_tree_hash() {
    let key = PlanCacheKey::new(1, 1, 1);
    let base = compute_plan_tree_hash(&key, &sample_plan("alpha", 10));
    let different_query = compute_plan_tree_hash(&key, &sample_plan("beta", 10));
    let different_limit = compute_plan_tree_hash(&key, &sample_plan("alpha", 20));
    assert_ne!(base, different_query);
    assert_ne!(base, different_limit);
}

#[test]
fn manifest_or_config_change_invalidates_cache_lookup_through_the_key() {
    // Bead acceptance #2 and #3.
    let mut cache = PlanCache::new(4);
    cache.insert(PlanCacheKey::new(1, 10, 100), sample_plan("alpha", 10));
    // Manifest bumped:
    assert!(cache.get(&PlanCacheKey::new(1, 11, 100)).is_none());
    // Search config bumped:
    assert!(cache.get(&PlanCacheKey::new(1, 10, 101)).is_none());
    // Original key still hits:
    assert!(cache.get(&PlanCacheKey::new(1, 10, 100)).is_some());
}

#[test]
fn compute_eql_hash_is_domain_separated_from_compute_search_config_hash() {
    let bytes = b"{\"q\":\"alpha\"}";
    assert_ne!(
        compute_eql_hash(bytes),
        compute_search_config_hash(bytes),
        "domain-separated hashes must not collide for identical inputs"
    );
}

#[test]
fn cache_capacity_is_clamped_to_documented_max() {
    let cache = PlanCache::new(usize::MAX);
    assert_eq!(cache.capacity(), MAX_PLAN_CACHE_ENTRIES);
}

#[test]
fn invalidate_other_generations_drops_only_non_matching_entries() {
    let mut cache = PlanCache::new(8);
    cache.insert(PlanCacheKey::new(1, 10, 100), sample_plan("alpha", 10));
    cache.insert(PlanCacheKey::new(2, 11, 100), sample_plan("beta", 10));
    cache.insert(PlanCacheKey::new(3, 10, 200), sample_plan("gamma", 10));
    let stale = cache.invalidate_other_generations(10, 100);
    assert_eq!(stale.len(), 2);
    assert!(cache.get(&PlanCacheKey::new(1, 10, 100)).is_some());
    assert!(cache.get(&PlanCacheKey::new(2, 11, 100)).is_none());
    assert!(cache.get(&PlanCacheKey::new(3, 10, 200)).is_none());
}
