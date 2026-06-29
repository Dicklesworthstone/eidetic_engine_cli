//! Criterion benchmark for cooperative graph centrality refresh (bd-dre9v).
//!
//! Group name: `graph_refresh_cooperative`
//!
//! The 25k-link fixture is stress-only by default; set
//! `EE_BENCH_INCLUDE_STRESS=1` or run the stress profile to include it.

#![allow(clippy::expect_used)]

use std::time::{Duration, Instant};

use asupersync::Cx;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ee::graph::cooperative_refresh::refresh_centrality_cooperative;
use ee::graph::hits::compute_hits;
use ee::graph::{AttrMap, DiGraph, MemoryGraphProjection, compute_betweenness, compute_pagerank};
use fnx_runtime::CgseValue;
use std::hint::black_box;

const BENCH_GROUP_NAME: &str = "graph_refresh_cooperative";
const REQUIRED_MANYCORE_SPEEDUP: f64 = 3.0;
const QUICK_WARMUP_ITERS: usize = 2;
const QUICK_MEASURE_ITERS: usize = 7;
const DEFAULT_SCALES: &[usize] = &[1000, 5000];
const STRESS_SCALES: &[usize] = &[1000, 5000, 25000];

#[derive(Clone, Copy, Debug)]
struct ComparisonStats {
    sequential_p50_ms: f64,
    cooperative_p50_ms: f64,
    cooperative_p99_ms: f64,
}

impl ComparisonStats {
    fn speedup(self) -> f64 {
        if self.cooperative_p50_ms <= f64::EPSILON {
            return f64::INFINITY;
        }
        self.sequential_p50_ms / self.cooperative_p50_ms
    }
}

fn memory_id(index: usize) -> String {
    format!("mem_{index:026}")
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

fn seeded_projection(link_count: usize) -> MemoryGraphProjection {
    let node_count = link_count.saturating_add(1).max(2);
    let mut graph = DiGraph::strict();
    for index in 0..node_count {
        graph.add_node(memory_id(index));
    }
    for index in 0..link_count {
        let source = memory_id(index % node_count);
        let mut target_index = (index + 1) % node_count;
        if index % 5 == 0 {
            target_index = (index + 7) % node_count;
        }
        if target_index == index % node_count {
            target_index = (target_index + 1) % node_count;
        }
        graph
            .add_edge_with_attrs(source, memory_id(target_index), edge_attrs(1.0))
            .expect("seed edge should be valid");
    }
    MemoryGraphProjection {
        graph,
        node_count,
        edge_count: link_count,
        build_ms: 0.0,
        snapshot_version: 0,
    }
}

fn run_sequential_once(projection: &MemoryGraphProjection) -> f64 {
    let start = Instant::now();
    let pagerank = compute_pagerank(projection).expect("sequential pagerank succeeds");
    let betweenness = compute_betweenness(projection).expect("sequential betweenness succeeds");
    let hits = compute_hits(&projection.graph).expect("sequential hits succeeds");
    black_box((
        pagerank.scores.len(),
        betweenness.scores.len(),
        hits.hubs.len(),
        hits.authorities.len(),
    ));
    start.elapsed().as_secs_f64() * 1000.0
}

fn run_cooperative_once(projection: &MemoryGraphProjection) -> f64 {
    let start = Instant::now();
    let report = refresh_centrality_cooperative(
        &Cx::for_testing(),
        projection,
        Instant::now(),
        Duration::from_secs(30),
    )
    .expect("cooperative centrality refresh succeeds");
    black_box((
        report.scores.len(),
        report.top_pagerank.len(),
        report.top_betweenness.len(),
        report.top_hubs.len(),
        report.top_authorities.len(),
    ));
    start.elapsed().as_secs_f64() * 1000.0
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    let last_index = sorted_samples.len() - 1;
    sorted_samples[(percentile * last_index as f64).round() as usize]
}

fn sample_times<F>(mut run: F) -> Vec<f64>
where
    F: FnMut() -> f64,
{
    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = run();
    }
    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(run());
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    samples
}

fn quick_stats(link_count: usize) -> ComparisonStats {
    let projection = seeded_projection(link_count);
    let sequential = sample_times(|| run_sequential_once(&projection));
    let cooperative = sample_times(|| run_cooperative_once(&projection));
    ComparisonStats {
        sequential_p50_ms: percentile(&sequential, 0.50),
        cooperative_p50_ms: percentile(&cooperative, 0.50),
        cooperative_p99_ms: percentile(&cooperative, 0.99),
    }
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn stress_scales_enabled() -> bool {
    std::env::var("EE_BENCH_INCLUDE_STRESS")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("EE_BENCH_PROFILE")
            .map(|value| value == "stress")
            .unwrap_or(false)
}

fn benchmark_scales() -> &'static [usize] {
    if stress_scales_enabled() {
        STRESS_SCALES
    } else {
        DEFAULT_SCALES
    }
}

fn manycore_speedup_required() -> bool {
    if std::env::var("EE_BENCH_REQUIRE_COOPERATIVE_SPEEDUP")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }
    std::env::var("EE_BENCH_HARDWARE_CLASS")
        .map(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("64") || value.contains("manycore") || value.contains("256gb")
        })
        .unwrap_or(false)
}

fn assert_manycore_speedup(link_count: usize, stats: ComparisonStats) {
    if !manycore_speedup_required() {
        return;
    }
    assert!(
        stats.speedup() >= REQUIRED_MANYCORE_SPEEDUP,
        "cooperative centrality refresh speedup below target for {link_count} links: {:.2}x < {:.2}x (sequential p50 {:.3}ms, cooperative p50 {:.3}ms, cooperative p99 {:.3}ms)",
        stats.speedup(),
        REQUIRED_MANYCORE_SPEEDUP,
        stats.sequential_p50_ms,
        stats.cooperative_p50_ms,
        stats.cooperative_p99_ms,
    );
}

fn bench_graph_refresh_cooperative(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        for &link_count in benchmark_scales() {
            assert_manycore_speedup(link_count, quick_stats(link_count));
        }
        return;
    }

    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    for &link_count in benchmark_scales() {
        let projection = seeded_projection(link_count);
        let label = format!("{link_count}_links");
        group.bench_with_input(
            BenchmarkId::new("sequential", &label),
            &projection,
            |b, projection| b.iter(|| black_box(run_sequential_once(projection))),
        );
        group.bench_with_input(
            BenchmarkId::new("cooperative", &label),
            &projection,
            |b, projection| b.iter(|| black_box(run_cooperative_once(projection))),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_refresh_cooperative);
criterion_main!(benches);
