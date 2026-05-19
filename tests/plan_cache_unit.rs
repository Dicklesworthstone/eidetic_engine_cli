//! Integration smoke tests for the EQL query plan cache.
//!
//! Inline `#[cfg(test)]` units in `src/search/plan_cache.rs` cover the cache's
//! happy path, edge cases, and self-verification. This file pins the
//! public-API guarantees from outside the crate (the shape an eventual
//! `run_search_inner` integration will rely on) and bumps coverage on the
//! determinism contract that bead `bd-2mey5` requires.

use ee::models::query::{EqlQuery, EqlSpeedMode, EqlTagsMode};
use ee::search::plan_cache::{
    CompiledPlan, DEFAULT_PLAN_CACHE_ENTRIES, EnvVarValueSource, MAX_PLAN_CACHE_ENTRIES,
    PLAN_CACHE_DIAG_SCHEMA_V1, PLAN_CACHE_ENV_VAR_NAME, PlanCache, PlanCacheDiagKey, PlanCacheKey,
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

#[test]
fn diag_report_public_constants_match_schema_contract() {
    // These constants are what `ee diag plan-cache --json` will emit; they
    // must stay byte-identical to the schema at
    // docs/schemas/ee.diag.plan_cache.v1.json.
    assert_eq!(PLAN_CACHE_DIAG_SCHEMA_V1, "ee.diag.plan_cache.v1");
    assert_eq!(PLAN_CACHE_ENV_VAR_NAME, "EE_QUERY_PLAN_CACHE_ENTRIES");
}

#[test]
fn diag_report_round_trips_through_serde_json_to_envelope_shape() {
    let mut cache = PlanCache::new(DEFAULT_PLAN_CACHE_ENTRIES);
    cache.insert(PlanCacheKey::new(7, 42, 17), sample_plan("release", 10));
    let report = cache.diag_report(EnvVarValueSource::RegistryDefault, 4);
    let json = serde_json::to_value(&report).expect("serialize");
    let envelope = serde_json::json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "diag plan-cache",
            "report": json,
        },
        "degraded": [],
    });
    let data = envelope
        .get("data")
        .and_then(|v| v.as_object())
        .expect("envelope data is an object");
    let report_value = data.get("report").expect("envelope.data.report set");
    assert_eq!(
        report_value.get("schemaTag").and_then(|v| v.as_str()),
        Some("ee.diag.plan_cache.v1"),
    );
    assert_eq!(
        report_value.get("envVarName").and_then(|v| v.as_str()),
        Some("EE_QUERY_PLAN_CACHE_ENTRIES"),
    );
}

#[test]
fn diag_report_key_conversion_preserves_components() {
    let from = PlanCacheKey::new(11, 22, 33);
    let into: PlanCacheDiagKey = from.into();
    assert_eq!(into.eql_hash, 11);
    assert_eq!(into.index_manifest_version, 22);
    assert_eq!(into.search_config_hash, 33);
}
