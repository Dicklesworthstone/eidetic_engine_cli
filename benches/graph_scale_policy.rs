//! Lightweight scale-policy benchmark for graph fixtures (bd-bife.17).
//!
//! This intentionally benchmarks the admission policy rather than executing
//! 100k-node algorithms during normal Criterion runs.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ee::graph::scale_policy::{graph_scale_decisions, graph_scale_total_budget_ms};
use std::hint::black_box;

const BENCH_GROUP_NAME: &str = "graph_scale_policy";
const SCALES: &[(&str, usize, usize)] = &[
    ("10k", 10_000, 25_000),
    ("50k", 50_000, 125_000),
    ("100k", 100_000, 250_000),
];

fn bench_graph_scale_policy(c: &mut Criterion) {
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    for &(label, nodes, edges) in SCALES {
        group.bench_with_input(
            BenchmarkId::new("decide", label),
            &(nodes, edges),
            |b, input| {
                b.iter(|| {
                    let decisions = graph_scale_decisions(input.0, input.1);
                    let total_budget = graph_scale_total_budget_ms(input.0, input.1);
                    black_box((decisions, total_budget));
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_scale_policy);
criterion_main!(benches);
