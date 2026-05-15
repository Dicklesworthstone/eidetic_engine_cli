//! Criterion benchmark for Gomory-Hu proximity tree build/query (bd-igvt.6).
//!
//! Group name: `graph_gomory_hu`

#![allow(clippy::expect_used)]

use std::time::Instant;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::graph::gomory_hu::{GOMORY_HU_WEIGHT_ATTR, build_gomory_hu_tree, query_min_cut};
use fnx_classes::Graph;
use fnx_runtime::CgseValue;

const BENCH_GROUP_NAME: &str = "graph_gomory_hu";
const BUILD_BUDGET_P50_MS: f64 = 5000.0;
const BUILD_BUDGET_P99_MS: f64 = 9000.0;
const QUERY_BUDGET_P50_MS: f64 = 0.1;
const QUICK_WARMUP_ITERS: usize = 2;
const QUICK_MEASURE_ITERS: usize = 7;
const COMPONENT_NODE_COUNT: usize = 16;
const SCALES: &[usize] = &[10, 100, 1000];

#[derive(Clone, Copy, Debug)]
struct QuickStats {
    p50_ms: f64,
    p99_ms: f64,
}

fn seeded_graph(node_count: usize) -> Graph {
    let mut graph = Graph::strict();
    for index in 0..node_count {
        graph.add_node(index.to_string());
    }
    for component_start in (0..node_count).step_by(COMPONENT_NODE_COUNT) {
        let component_end = (component_start + COMPONENT_NODE_COUNT).min(node_count);
        for index in component_start..component_end.saturating_sub(1) {
            graph
                .add_edge_with_attrs(index.to_string(), (index + 1).to_string(), edge_attrs(1.0))
                .expect("gomory-hu benchmark chain edge should insert");
        }
    }
    graph
}

fn edge_attrs(weight: f64) -> fnx_classes::AttrMap {
    let mut attrs = fnx_classes::AttrMap::new();
    attrs.insert(GOMORY_HU_WEIGHT_ATTR.to_owned(), CgseValue::Float(weight));
    attrs
}

fn run_build_once(node_count: usize) -> f64 {
    let graph = seeded_graph(node_count);
    let start = Instant::now();
    let tree = build_gomory_hu_tree(&graph).unwrap_or_else(|error| {
        panic!("gomory-hu benchmark succeeds for {node_count} memories: {error:?}");
    });
    black_box(tree);
    start.elapsed().as_secs_f64() * 1000.0
}

fn run_query_once(node_count: usize) -> f64 {
    let graph = seeded_graph(node_count);
    let tree = build_gomory_hu_tree(&graph).unwrap_or_else(|error| {
        panic!("gomory-hu benchmark succeeds for {node_count} memories: {error:?}");
    });
    let nodes = graph.nodes_ordered();
    let left = nodes.first().copied().unwrap_or("0");
    let right_index = node_count.min(COMPONENT_NODE_COUNT).saturating_sub(1);
    let right = nodes.get(right_index).copied().unwrap_or(left);
    let start = Instant::now();
    let cut = query_min_cut(&tree, left, right);
    black_box(cut);
    start.elapsed().as_secs_f64() * 1000.0
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    let last_index = sorted_samples.len() - 1;
    sorted_samples[(percentile * last_index as f64).round() as usize]
}

fn quick_stats(node_count: usize, runner: fn(usize) -> f64) -> QuickStats {
    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = runner(node_count);
    }
    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(runner(node_count));
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

fn assert_build_budget(scale: usize, stats: QuickStats) {
    assert!(
        stats.p50_ms <= BUILD_BUDGET_P50_MS,
        "graph_gomory_hu build p50 exceeded for {scale} memories: {:.3}ms > {:.3}ms",
        stats.p50_ms,
        BUILD_BUDGET_P50_MS
    );
    assert!(
        stats.p99_ms <= BUILD_BUDGET_P99_MS,
        "graph_gomory_hu build p99 exceeded for {scale} memories: {:.3}ms > {:.3}ms",
        stats.p99_ms,
        BUILD_BUDGET_P99_MS
    );
}

fn assert_query_budget(scale: usize, stats: QuickStats) {
    assert!(
        stats.p50_ms <= QUERY_BUDGET_P50_MS,
        "graph_gomory_hu query p50 exceeded for {scale} memories: {:.3}ms > {:.3}ms",
        stats.p50_ms,
        QUERY_BUDGET_P50_MS
    );
}

fn bench_graph_gomory_hu(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        for &scale in SCALES {
            assert_build_budget(scale, quick_stats(scale, run_build_once));
            assert_query_budget(scale, quick_stats(scale, run_query_once));
        }
        return;
    }

    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    for &scale in SCALES {
        group.bench_with_input(
            BenchmarkId::new("gomory_hu_build", format!("{scale}_memories")),
            &scale,
            |b, &node_count| b.iter(|| black_box(run_build_once(node_count))),
        );
        group.bench_with_input(
            BenchmarkId::new("gomory_hu_query", format!("{scale}_memories")),
            &scale,
            |b, &node_count| b.iter(|| black_box(run_query_once(node_count))),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_gomory_hu);
criterion_main!(benches);
