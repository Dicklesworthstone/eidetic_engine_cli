use std::collections::BTreeMap;

use ee::graph::minhash_rank::{MinHashRankPolicy, compute_minhash_rank_with_policy};
use fnx_algorithms::pagerank_directed;
use fnx_classes::digraph::DiGraph;
use fnx_runtime::CompatibilityMode;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

type TestResult = Result<(), String>;

fn add_edge(graph: &mut DiGraph, source: &str, target: &str) -> TestResult {
    graph
        .add_edge(source, target)
        .map_err(|error| format!("add edge {source}->{target}: {error}"))
}

fn generated_digraph(edges: &[(u8, u8)]) -> Result<DiGraph, String> {
    let mut graph = DiGraph::new(CompatibilityMode::Strict);
    for node in 0_u8..8 {
        graph.add_node(format!("mem_{node:02}"));
    }
    for (source, target) in edges {
        if source != target {
            add_edge(
                &mut graph,
                &format!("mem_{source:02}"),
                &format!("mem_{target:02}"),
            )?;
        }
    }
    Ok(graph)
}

fn rank_map<I>(nodes: I) -> BTreeMap<String, usize>
where
    I: IntoIterator<Item = String>,
{
    nodes
        .into_iter()
        .enumerate()
        .map(|(index, node)| (node, index + 1))
        .collect()
}

fn spearman_correlation(left: &BTreeMap<String, usize>, right: &BTreeMap<String, usize>) -> f64 {
    let shared = left
        .iter()
        .filter_map(|(node, left_rank)| right.get(node).map(|right_rank| (*left_rank, *right_rank)))
        .collect::<Vec<_>>();
    let n = shared.len();
    if n < 2 {
        return 1.0;
    }
    let d_squared_sum = shared
        .iter()
        .map(|(left_rank, right_rank)| {
            let delta = *left_rank as f64 - *right_rank as f64;
            delta * delta
        })
        .sum::<f64>();
    let n = n as f64;
    1.0 - (6.0 * d_squared_sum) / (n * (n * n - 1.0))
}

#[test]
fn minhash_rank_correlates_with_pagerank_on_ordered_fixture() -> TestResult {
    let mut graph = DiGraph::new(CompatibilityMode::Strict);
    for node in 0_u8..12 {
        graph.add_node(format!("mem_{node:02}"));
    }
    for source in 0_u8..12 {
        for target in 0_u8..source {
            add_edge(
                &mut graph,
                &format!("mem_{source:02}"),
                &format!("mem_{target:02}"),
            )?;
        }
    }

    let minhash = compute_minhash_rank_with_policy(
        &graph,
        MinHashRankPolicy {
            signature_count: 64,
            top_k: 12,
        },
    )
    .map_err(|error| error.to_string())?;
    let pagerank = pagerank_directed(&graph);

    let minhash_ranks = rank_map(minhash.scores.into_iter().map(|score| score.node));
    let mut pagerank_scores = pagerank.scores;
    pagerank_scores.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.node.cmp(&right.node))
    });
    let pagerank_ranks = rank_map(pagerank_scores.into_iter().map(|score| score.node));
    let correlation = spearman_correlation(&minhash_ranks, &pagerank_ranks);

    assert!(
        correlation >= 0.90,
        "minhash rank should correlate with PageRank on ordered fixture, got {correlation}"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(80))]

    #[test]
    fn minhash_rank_is_stable_and_sequential_for_generated_graphs(
        edges in prop::collection::vec((0_u8..8, 0_u8..8), 0..48),
    ) {
        let graph = generated_digraph(&edges).map_err(TestCaseError::fail)?;
        let policy = MinHashRankPolicy {
            signature_count: 32,
            top_k: 8,
        };

        let first = compute_minhash_rank_with_policy(&graph, policy)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let second = compute_minhash_rank_with_policy(&graph, policy)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;

        prop_assert_eq!(&first, &second);
        prop_assert_eq!(first.scores.len(), graph.nodes_ordered().len());
        for (index, score) in first.scores.iter().enumerate() {
            prop_assert_eq!(score.rank, index + 1);
            prop_assert_eq!(score.signature.len(), policy.signature_count);
        }
        for pair in first.scores.windows(2) {
            prop_assert!(
                pair[0].signature_density >= pair[1].signature_density,
                "signature density must be descending"
            );
        }
    }
}
