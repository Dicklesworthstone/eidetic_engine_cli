//! Criterion benchmark for speculative CASS prefetch prediction (bd-1cc1c).
//!
//! The predictor runs in daemon idle slots under a small per-call budget. Keep
//! histories outside the measured closure so the benchmark tracks prediction
//! work, not fixture construction.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ee::core::cass_prefetch::{
    CassPrefetchHistory, DEFAULT_PREFETCH_TOP_K, MAX_PREFETCH_HISTORY,
    RecencyWeightedFrequencyPredictor, SpeculativePrefetch,
};
use std::hint::black_box;

const BENCH_GROUP_NAME: &str = "cass_prefetch";
const BENCH_AGENT_SCOPE: &str = "agent:bench";

fn canonical_repeated_history() -> CassPrefetchHistory {
    CassPrefetchHistory::from_topics(
        BENCH_AGENT_SCOPE,
        [
            "current",
            "refactor",
            "debug",
            "refactor",
            "doc_update",
            "debug",
            "refactor",
            "release",
            "debug",
            "doc_update",
        ],
    )
}

fn duplicate_dense_max_history() -> CassPrefetchHistory {
    let mut topics = Vec::with_capacity(MAX_PREFETCH_HISTORY);
    topics.push("current".to_string());
    for index in 1..MAX_PREFETCH_HISTORY {
        let topic = match index % 4 {
            0 => "refactor",
            1 => "debug",
            2 => "release",
            _ => "doc_update",
        };
        topics.push(topic.to_string());
    }
    CassPrefetchHistory::from_topics(BENCH_AGENT_SCOPE, topics)
}

fn distinct_max_history() -> CassPrefetchHistory {
    let mut topics = Vec::with_capacity(MAX_PREFETCH_HISTORY);
    topics.push("current".to_string());
    for index in 1..MAX_PREFETCH_HISTORY {
        topics.push(format!("topic_{index:02}"));
    }
    CassPrefetchHistory::from_topics(BENCH_AGENT_SCOPE, topics)
}

fn bench_cass_prefetch(criterion: &mut Criterion) {
    let predictor = RecencyWeightedFrequencyPredictor::new();
    let scenarios = [
        ("canonical_repeated_window", canonical_repeated_history()),
        ("duplicate_dense_max_window", duplicate_dense_max_history()),
        ("distinct_max_window", distinct_max_history()),
    ];

    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
    for (label, history) in scenarios {
        group.bench_with_input(
            BenchmarkId::new("predict_next_n", label),
            &history,
            |bench, history| {
                bench.iter(|| {
                    let candidates = predictor
                        .predict_next_n(black_box(history), black_box(DEFAULT_PREFETCH_TOP_K));
                    black_box(candidates);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_cass_prefetch);
criterion_main!(benches);
