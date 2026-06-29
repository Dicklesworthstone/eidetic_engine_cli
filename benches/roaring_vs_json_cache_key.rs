//! Size bench for graph result-cache params encoded as Roaring bitmaps.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::Value as JsonValue;
use std::hint::black_box;

use ee::graph::result_cache_keys::{
    GraphAlgorithmParamBitmap, canonical_graph_algorithm_params_json,
    encode_graph_algorithm_params_bitmap,
};

const BENCH_GROUP_NAME: &str = "bench_roaring_vs_json_cache_key";
const MIN_SIZE_REDUCTION_RATIO: f64 = 3.0;

fn fixture_params(seed_count: usize) -> JsonValue {
    let seed_memories: Vec<_> = (0..seed_count)
        .map(|index| format!("mem_{index:026}"))
        .collect();
    serde_json::json!({
        "algorithm": "personalized_pagerank",
        "damping": 0.85,
        "maxIterations": 50,
        "tolerance": 0.000001,
        "seedMemories": seed_memories,
        "weights": {
            "recency": 0.25,
            "trust": 0.35,
            "structural": 0.40
        },
        "filters": {
            "minConfidence": 0.55,
            "includeExpired": false,
            "relationKinds": ["supports", "depends_on", "contradicts"]
        },
        "queryProfile": "agent-context-refresh-with-graph-structural-reranking",
        "workspaceScope": "local-first-derived-graph-cache-for-maintenance-jobs",
        "decisionPath": "seeded-ppr-before-pack-dna-before-hits-authority-profile"
    })
}

fn canonical_params_json(params: &JsonValue) -> String {
    match canonical_graph_algorithm_params_json(params) {
        Ok(json) => json,
        Err(error) => panic!("canonical params JSON should serialize: {error}"),
    }
}

fn encoded_params_bitmap(params: &JsonValue) -> GraphAlgorithmParamBitmap {
    match encode_graph_algorithm_params_bitmap(params) {
        Ok(bitmap) => bitmap,
        Err(error) => panic!("params bitmap should encode: {error}"),
    }
}

fn serialized_bitmap_bytes(bitmap: &GraphAlgorithmParamBitmap) -> Vec<u8> {
    match bitmap.serialized_bytes() {
        Ok(bytes) => bytes,
        Err(error) => panic!("params bitmap should serialize: {error}"),
    }
}

fn assert_size_ratio(params: &JsonValue) {
    let json_len = canonical_params_json(params).len();
    let roaring_len = encoded_params_bitmap(params).serialized_len();
    let ratio = json_len as f64 / roaring_len.max(1) as f64;
    assert!(
        ratio >= MIN_SIZE_REDUCTION_RATIO,
        "expected >= {MIN_SIZE_REDUCTION_RATIO}x cache-key size reduction; json={json_len} roaring={roaring_len} ratio={ratio:.2}"
    );
}

fn bench_roaring_vs_json_cache_key(c: &mut Criterion) {
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    for seed_count in [4usize, 16, 64] {
        let params = fixture_params(seed_count);
        assert_size_ratio(&params);

        group.bench_with_input(
            BenchmarkId::new("canonical_json", seed_count),
            &params,
            |bench, params| {
                bench.iter(|| {
                    black_box(canonical_params_json(black_box(params)));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("roaring_bitmap", seed_count),
            &params,
            |bench, params| {
                bench.iter(|| {
                    let bitmap = encoded_params_bitmap(black_box(params));
                    black_box(serialized_bitmap_bytes(&bitmap));
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_roaring_vs_json_cache_key);
criterion_main!(benches);
