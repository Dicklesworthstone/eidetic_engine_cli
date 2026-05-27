//! Adaptive context-pack budget classifier benchmark (bd-1prrl.4).
//!
//! Exercises `classify_adaptive_budget` on representative retrieval
//! distributions and query shapes to keep the per-call cost well under the
//! 2 ms p50 ceiling the swarmx.7 acceptance asks for. The classifier is a
//! pure function over `&[f32]` retrieval scores plus the query text, so
//! the bench stays deterministic and host-portable.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::pack::budget_classifier::{
    AdaptiveBudgetInput, RETRIEVAL_ENTROPY_SAMPLE_LIMIT, classify_adaptive_budget,
};

const BENCH_GROUP_NAME: &str = "adaptive_budget";

fn skewed_scores(len: usize) -> Vec<f32> {
    (0..len).map(|i| 1.0_f32 / ((i as f32) + 1.0)).collect()
}

fn uniform_scores(len: usize) -> Vec<f32> {
    vec![0.75_f32; len]
}

fn scenarios() -> Vec<(&'static str, &'static str, Vec<f32>, f64)> {
    vec![
        (
            "trivial_lookup_empty_retrieval",
            "show memory mem_release_policy",
            Vec::new(),
            0.0,
        ),
        (
            "balanced_small_retrieval",
            "context for release workflow",
            skewed_scores(8),
            1.0,
        ),
        (
            "complex_keyword_uniform_topk",
            "audit refactor migrate security performance",
            uniform_scores(RETRIEVAL_ENTROPY_SAMPLE_LIMIT),
            2.5,
        ),
        (
            "complex_skewed_high_fanout",
            "diagnose hardening regression in retrieval ranking",
            skewed_scores(RETRIEVAL_ENTROPY_SAMPLE_LIMIT),
            12.0,
        ),
    ]
}

fn bench_adaptive_budget(c: &mut Criterion) {
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(50);
    for (label, query, scores, fanout) in scenarios() {
        group.bench_with_input(
            BenchmarkId::new("classify", label),
            &(query, scores, fanout),
            |b, input| {
                b.iter(|| {
                    let decision = classify_adaptive_budget(
                        AdaptiveBudgetInput::new(input.0, input.1.as_slice(), input.2)
                            .with_max_tokens(8_000),
                    );
                    black_box(decision);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_adaptive_budget);
criterion_main!(benches);
