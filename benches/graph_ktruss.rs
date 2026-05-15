//! Criterion benchmark for k-truss structural health computation (bd-igvt.6).
//!
//! Group name: `graph_ktruss`

#![allow(clippy::expect_used)]

use std::time::Instant;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::graph::health::compute_k_truss;
use fnx_classes::Graph;
use fnx_generators::GraphGenerator;

const BENCH_GROUP_NAME: &str = "graph_ktruss";
const BUDGET_P50_MS: f64 = 150.0;
const BUDGET_P99_MS: f64 = 600.0;
const QUICK_WARMUP_ITERS: usize = 3;
const QUICK_MEASURE_ITERS: usize = 11;
const SCALES: &[usize] = &[10, 100, 1000];

#[derive(Clone, Copy, Debug)]
struct QuickStats {
    p50_ms: f64,
    p99_ms: f64,
}

fn seeded_graph(node_count: usize) -> Graph {
    let mut generator = GraphGenerator::strict();
    let density = 0.02_f64.max(20.0 / node_count.max(1) as f64).min(1.0);
    generator
        .gnp_random_graph(node_count, density, 29)
        .expect("k-truss benchmark graph should generate")
        .graph
}

fn run_once(node_count: usize) -> f64 {
    let graph = seeded_graph(node_count);
    let start = Instant::now();
    let report = compute_k_truss(&graph);
    black_box(report);
    start.elapsed().as_secs_f64() * 1000.0
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    let last_index = sorted_samples.len() - 1;
    sorted_samples[(percentile * last_index as f64).round() as usize]
}

fn quick_stats(node_count: usize) -> QuickStats {
    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = run_once(node_count);
    }
    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(run_once(node_count));
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    QuickStats {
        p50_ms: percentile(&samples, 0.50),
        p99_ms: percentile(&samples, 0.99),
    }
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn assert_budget(scale: usize, stats: QuickStats) {
    assert!(
        stats.p50_ms <= BUDGET_P50_MS,
        "graph_ktruss p50 exceeded for {scale} memories: {:.3}ms > {:.3}ms",
        stats.p50_ms,
        BUDGET_P50_MS
    );
    assert!(
        stats.p99_ms <= BUDGET_P99_MS,
        "graph_ktruss p99 exceeded for {scale} memories: {:.3}ms > {:.3}ms",
        stats.p99_ms,
        BUDGET_P99_MS
    );
}

fn bench_graph_ktruss(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        for &scale in SCALES {
            assert_budget(scale, quick_stats(scale));
        }
        return;
    }

    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    for &scale in SCALES {
        group.bench_with_input(
            BenchmarkId::new("k_truss", format!("{scale}_memories")),
            &scale,
            |b, &node_count| b.iter(|| black_box(run_once(node_count))),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_ktruss);
criterion_main!(benches);
