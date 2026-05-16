//! Criterion benchmark for minhash rank centrality (bd-3usjw.46).
//!
//! Group name: `graph_minhash_rank`

#![allow(clippy::expect_used)]

use std::time::Instant;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::graph::DiGraph;
use ee::graph::minhash_rank::{MinHashRankPolicy, compute_minhash_rank_with_policy};
use fnx_algorithms::pagerank_directed;

const BENCH_GROUP_NAME: &str = "graph_minhash_rank";
const QUICK_WARMUP_ITERS: usize = 2;
const QUICK_MEASURE_ITERS: usize = 9;
const MINHASH_SPEEDUP_FLOOR: f64 = 5.0;
const TOP_K: usize = 100;
const SIGNATURE_COUNT: usize = 1;
const COMPARE_SCALE: usize = 2_000;
const SCALES: &[usize] = &[100, 1_000, COMPARE_SCALE];

#[derive(Clone, Copy, Debug)]
struct QuickStats {
    minhash_p50_ms: f64,
    pagerank_p50_ms: f64,
}

fn node_id(index: usize) -> String {
    format!("mem_{index:06}")
}

fn seeded_graph(node_count: usize) -> DiGraph {
    let mut graph = DiGraph::strict();
    for index in 0..node_count {
        graph.add_node(node_id(index));
    }
    for source in 0..node_count {
        let fanout = source.min(32);
        for offset in 0..fanout {
            let target = offset;
            if source != target {
                graph
                    .add_edge(node_id(source), node_id(target))
                    .expect("minhash rank benchmark edge should be valid");
            }
        }
    }
    graph
}

fn run_minhash_once(graph: &DiGraph) -> f64 {
    let start = Instant::now();
    let scores = compute_minhash_rank_with_policy(
        graph,
        MinHashRankPolicy {
            signature_count: SIGNATURE_COUNT,
            top_k: TOP_K,
        },
    )
    .expect("minhash rank benchmark succeeds");
    black_box(scores);
    start.elapsed().as_secs_f64() * 1000.0
}

fn run_pagerank_top_k_once(graph: &DiGraph) -> f64 {
    let start = Instant::now();
    let mut scores = pagerank_directed(graph).scores;
    scores.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.node.cmp(&right.node))
    });
    scores.truncate(TOP_K);
    black_box(scores);
    start.elapsed().as_secs_f64() * 1000.0
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    let last_index = sorted_samples.len() - 1;
    sorted_samples[(percentile * last_index as f64).round() as usize]
}

fn quick_stats(node_count: usize) -> QuickStats {
    let graph = seeded_graph(node_count);
    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = run_minhash_once(&graph);
        let _ = run_pagerank_top_k_once(&graph);
    }

    let mut minhash_samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    let mut pagerank_samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        minhash_samples.push(run_minhash_once(&graph));
        pagerank_samples.push(run_pagerank_top_k_once(&graph));
    }
    minhash_samples.sort_by(|left, right| left.total_cmp(right));
    pagerank_samples.sort_by(|left, right| left.total_cmp(right));

    QuickStats {
        minhash_p50_ms: percentile(&minhash_samples, 0.50),
        pagerank_p50_ms: percentile(&pagerank_samples, 0.50),
    }
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn assert_speedup(stats: QuickStats) {
    let speedup = stats.pagerank_p50_ms / stats.minhash_p50_ms.max(f64::EPSILON);
    assert!(
        speedup >= MINHASH_SPEEDUP_FLOOR,
        "minhash rank p50 speedup below floor: {:.2}x < {:.2}x (minhash {:.3}ms, pagerank {:.3}ms)",
        speedup,
        MINHASH_SPEEDUP_FLOOR,
        stats.minhash_p50_ms,
        stats.pagerank_p50_ms
    );
}

fn bench_graph_minhash_rank(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        assert_speedup(quick_stats(COMPARE_SCALE));
        return;
    }

    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    for &scale in SCALES {
        let graph = seeded_graph(scale);
        group.bench_with_input(
            BenchmarkId::new("minhash_rank_top_k", format!("{scale}_memories")),
            &graph,
            |b, graph| b.iter(|| black_box(run_minhash_once(graph))),
        );
        group.bench_with_input(
            BenchmarkId::new("pagerank_top_k", format!("{scale}_memories")),
            &graph,
            |b, graph| b.iter(|| black_box(run_pagerank_top_k_once(graph))),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_minhash_rank);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(super::BENCH_GROUP_NAME, "graph_minhash_rank");
    }

    #[test]
    fn speedup_floor_matches_acceptance_gate() {
        assert!((super::MINHASH_SPEEDUP_FLOOR - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn top_k_matches_bead_acceptance() {
        assert_eq!(super::TOP_K, 100);
    }
}
