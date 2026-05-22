#![no_main]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 131_072;
const SEARCH_CONFIG_BYTES: &[u8] = b"eql_parser_fuzz_default_search_config_v1";

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(input) else {
        return;
    };

    let Ok(query) = ee::models::query::parse_eql_query(&value) else {
        return;
    };

    assert!(query.limit > 0);
    exercise_plan_cache(&value, &query, data);

    let metadata = serde_json::json!({
        "workspace": "workspace-a",
        "level": "procedural",
        "kind": "rule",
        "scope": ["repo", "release"],
        "tags": ["release", "format", "unicode-\u{03c0}"],
        "confidence": 0.91,
        "createdAt": "2026-05-06T00:00:00Z",
        "ageDays": 2,
        "graph": {
            "center": "mem_00000000000000000000000000",
            "hops": 2,
            "relations": ["supports", "derived_from"]
        }
    });
    let sparse_metadata = serde_json::json!({
        "tags": "release",
        "created_at": "2026-05-01T00:00:00Z"
    });
    let empty_metadata = serde_json::json!({});
    let candidates = [metadata, sparse_metadata, empty_metadata];

    let _ = query.metadata_filters();
    for candidate in &candidates {
        let _ = query.matches_metadata(Some(candidate));
    }
    let selected = query.execute_metadata(candidates.iter());
    assert!(selected.len() <= candidates.len());
});

fn exercise_plan_cache(value: &Value, query: &ee::models::query::EqlQuery, data: &[u8]) {
    let Ok(canonical_request) = serde_json::to_vec(value) else {
        return;
    };

    let key = ee::search::plan_cache::PlanCacheKey::new(
        ee::search::plan_cache::compute_eql_hash(&canonical_request),
        manifest_version_from(data),
        ee::search::plan_cache::compute_search_config_hash(SEARCH_CONFIG_BYTES),
    );
    let plan = ee::search::plan_cache::CompiledPlan::from_query(query.clone());
    let plan_tree_hash = ee::search::plan_cache::compute_plan_tree_hash(&key, &plan);
    assert!(plan_tree_hash.starts_with("blake3:"));
    assert_eq!(
        ee::search::plan_cache::compute_plan_tree_hash(&key, &plan),
        plan_tree_hash
    );

    let mut cache = ee::search::plan_cache::PlanCache::new(2);
    let inserted = cache.insert(key, plan.clone());
    assert_eq!(inserted.plan_tree_hash, plan_tree_hash);

    let hit = cache.get(&key);
    assert!(hit.is_some());
    if let Some(hit) = hit {
        assert_eq!(hit.plan, plan);
        assert_eq!(hit.plan_tree_hash, plan_tree_hash);
    }
    let stats = cache.stats();
    assert_eq!(stats.current_size, 1);
    assert_eq!(stats.inserts, 1);
    assert_eq!(stats.hits, 1);
}

fn manifest_version_from(data: &[u8]) -> u64 {
    let mut bytes = [0_u8; 8];
    for (target, source) in bytes.iter_mut().zip(data.iter().copied()) {
        *target = source;
    }
    u64::from_le_bytes(bytes)
}
