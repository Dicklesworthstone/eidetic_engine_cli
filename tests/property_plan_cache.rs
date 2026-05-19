//! Property tests for the EQL query plan cache (`bd-2mey5`).
//!
//! Strengthens the deterministic-hash and bounded-capacity contracts on top of
//! the example-based coverage already shipping inline in
//! `src/search/plan_cache.rs` and `tests/plan_cache_unit.rs`. Properties:
//!
//! 1. `compute_plan_tree_hash` is deterministic across repeated calls and is
//!    insensitive to which `PlanCache` instance computed it.
//! 2. Round-trip insert/get preserves the plan and the plan-tree hash.
//! 3. `cache.len() <= cache.capacity()` after any insert sequence.
//! 4. Distinct cache keys always produce distinct plan-tree hashes (because
//!    the key is hashed alongside the plan); same key + same plan produces
//!    the same hash.
//! 5. `compute_eql_hash` and `compute_search_config_hash` are domain-separated
//!    across arbitrary byte inputs.
//!
//! Property #1, #4, #5 together cover bead acceptance #1
//! ("Identical EQL → identical plan-tree hash on cache hit") under a wider
//! input distribution than the example tests can exercise.

use ee::models::query::{EqlQuery, EqlSpeedMode, EqlTagsMode};
use ee::search::plan_cache::{
    CompiledPlan, MAX_PLAN_CACHE_ENTRIES, PlanCache, PlanCacheKey, compute_eql_hash,
    compute_plan_tree_hash, compute_search_config_hash,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn arbitrary_query_string() -> impl Strategy<Value = String> {
    // Keep generated queries short so each proptest iteration stays cheap.
    "[ -~]{1,32}"
        .prop_map(|s| s.trim().to_string())
        .prop_filter("query strings must not be empty after trim", |s| {
            !s.is_empty()
        })
}

fn arbitrary_query() -> impl Strategy<Value = EqlQuery> {
    (
        arbitrary_query_string(),
        1u32..=100,
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(q, limit, rerank, return_subgraph, explain)| EqlQuery {
            q,
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
            rerank,
            return_subgraph,
            explain,
        })
}

fn arbitrary_key() -> impl Strategy<Value = PlanCacheKey> {
    (any::<u64>(), any::<u64>(), any::<u64>())
        .prop_map(|(eql, manifest, config)| PlanCacheKey::new(eql, manifest, config))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Property #1: same (key, plan) always produces the same plan-tree
    /// hash. This is the property bead `bd-2mey5` acceptance #1 ultimately
    /// pins.
    #[test]
    fn plan_tree_hash_is_deterministic_for_same_input(
        key in arbitrary_key(),
        plan in arbitrary_query().prop_map(CompiledPlan::from_query),
    ) {
        let first = compute_plan_tree_hash(&key, &plan);
        let second = compute_plan_tree_hash(&key, &plan);
        let third = compute_plan_tree_hash(&key, &plan.clone());
        prop_assert_eq!(&first, &second);
        prop_assert_eq!(&first, &third);
        prop_assert!(first.starts_with("blake3:"));
    }

    /// Property #2: a freshly inserted plan round-trips through `get` with the
    /// same plan content and the same plan-tree hash.
    #[test]
    fn insert_then_get_round_trips_the_plan(
        key in arbitrary_key(),
        plan in arbitrary_query().prop_map(CompiledPlan::from_query),
        capacity in 1usize..=32,
    ) {
        let mut cache = PlanCache::new(capacity);
        let inserted = cache.insert(key, plan.clone());
        let hit = cache.get(&key).expect("round-trip should hit");
        prop_assert_eq!(&hit.plan, &plan);
        prop_assert_eq!(&hit.plan_tree_hash, &inserted.plan_tree_hash);
    }

    /// Property #3: after any sequence of inserts, `cache.len() <=
    /// cache.capacity()`. Tests the LRU bound under arbitrary input ordering.
    #[test]
    fn cache_length_never_exceeds_capacity(
        capacity in 1usize..=8,
        keys in proptest::collection::vec(arbitrary_key(), 0..32),
        plans in proptest::collection::vec(arbitrary_query(), 0..32),
    ) {
        let mut cache = PlanCache::new(capacity);
        for (key, query) in keys.iter().zip(plans.iter()) {
            cache.insert(*key, CompiledPlan::from_query(query.clone()));
            prop_assert!(cache.len() <= cache.capacity());
        }
        prop_assert!(cache.capacity() <= MAX_PLAN_CACHE_ENTRIES);
    }

    /// Property #4: keys that differ in any component produce different
    /// plan-tree hashes even when the plan payload is identical, because the
    /// key is hashed alongside the plan.
    #[test]
    fn different_keys_with_identical_plan_hash_differently(
        key_a in arbitrary_key(),
        key_b in arbitrary_key(),
        plan in arbitrary_query().prop_map(CompiledPlan::from_query),
    ) {
        prop_assume!(key_a != key_b);
        let hash_a = compute_plan_tree_hash(&key_a, &plan);
        let hash_b = compute_plan_tree_hash(&key_b, &plan);
        prop_assert_ne!(hash_a, hash_b);
    }

    /// Property #5: `compute_eql_hash` and `compute_search_config_hash` are
    /// domain-separated for any byte input.
    #[test]
    fn eql_and_search_config_hashes_are_domain_separated(
        bytes in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let eql = compute_eql_hash(&bytes);
        let cfg = compute_search_config_hash(&bytes);
        prop_assert_ne!(eql, cfg);
    }
}

// Non-proptest sanity tests that complement the property cases. Keep these
// inexpensive so the file's overall runtime stays modest.

#[test]
fn empty_payload_eql_and_search_config_hashes_are_still_separated() {
    assert_ne!(compute_eql_hash(b""), compute_search_config_hash(b""));
}

#[test]
fn capacity_clamps_to_documented_max() {
    let cache = PlanCache::new(MAX_PLAN_CACHE_ENTRIES + 1);
    assert_eq!(cache.capacity(), MAX_PLAN_CACHE_ENTRIES);
}
