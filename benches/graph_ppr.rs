//! Criterion benchmark for personalized PageRank graph reranking (bd-igvt.6).
//!
//! Group name: `graph_ppr`

#![allow(clippy::expect_used)]

use std::collections::HashMap;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::graph::ppr::compute_personalized_pagerank;
use ee::graph::{AttrMap, DiGraph};
use ee::models::MemoryId;
use fnx_runtime::CgseValue;
use uuid::Uuid;

const BENCH_GROUP_NAME: &str = "graph_ppr";
const BUDGET_P50_MS: f64 = 30.0;
const BUDGET_P99_MS: f64 = 120.0;
const QUICK_WARMUP_ITERS: usize = 3;
const QUICK_MEASURE_ITERS: usize = 11;
const SCALES: &[usize] = &[10, 100, 1000];

#[derive(Clone, Copy, Debug)]
struct QuickStats {
    p50_ms: f64,
    p99_ms: f64,
}

fn memory_id(index: usize) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(index.saturating_add(1) as u128))
}

fn edge_attrs(weight: f64) -> AttrMap {
    let mut attrs = AttrMap::new();
    attrs.insert(
        "relation".to_owned(),
        CgseValue::String("supports".to_owned()),
    );
    attrs.insert("weight".to_owned(), CgseValue::Float(weight));
    attrs.insert("confidence".to_owned(), CgseValue::Float(1.0));
    attrs
}

fn seeded_graph(node_count: usize) -> (DiGraph, HashMap<MemoryId, f64>) {
    let mut graph = DiGraph::strict();
    for index in 0..node_count {
        graph.add_node(memory_id(index).to_string());
    }
    for index in 0..node_count.saturating_sub(1) {
        graph
            .add_edge_with_attrs(
                memory_id(index).to_string(),
                memory_id(index + 1).to_string(),
                edge_attrs(1.0),
            )
            .expect("seed edge should be valid");
        if index + 7 < node_count {
            graph
                .add_edge_with_attrs(
                    memory_id(index).to_string(),
                    memory_id(index + 7).to_string(),
                    edge_attrs(0.35),
                )
                .expect("skip edge should be valid");
        }
    }
    let seeds = HashMap::from([(memory_id(0), 1.0)]);
    (graph, seeds)
}

fn run_once(node_count: usize) -> f64 {
    let (graph, seeds) = seeded_graph(node_count);
    let start = Instant::now();
    let scores = compute_personalized_pagerank(&graph, &seeds).expect("ppr benchmark succeeds");
    black_box(scores);
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
        "graph_ppr p50 exceeded for {scale} memories: {:.3}ms > {:.3}ms",
        stats.p50_ms,
        BUDGET_P50_MS
    );
    assert!(
        stats.p99_ms <= BUDGET_P99_MS,
        "graph_ppr p99 exceeded for {scale} memories: {:.3}ms > {:.3}ms",
        stats.p99_ms,
        BUDGET_P99_MS
    );
}

fn bench_graph_ppr(c: &mut Criterion) {
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
            BenchmarkId::new("personalized_pagerank", format!("{scale}_memories")),
            &scale,
            |b, &node_count| b.iter(|| black_box(run_once(node_count))),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_ppr);
criterion_main!(benches);
