use std::collections::BTreeSet;

use fnx_algorithms::transitive_closure;
use fnx_classes::digraph::DiGraph;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

#[path = "support/graph_generator.rs"]
mod graph_generator;

use graph_generator::deterministic_digraph;

fn generated_digraph(node_count: usize, density_percent: u8, seed: u64) -> Result<DiGraph, String> {
    deterministic_digraph(node_count, f64::from(density_percent) / 100.0, seed)
}

fn edge_set(graph: &DiGraph) -> BTreeSet<(String, String)> {
    graph
        .edges_ordered()
        .into_iter()
        .map(|edge| (edge.left, edge.right))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn transitive_closure_is_idempotent(
        node_count in 0usize..=18,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = generated_digraph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;

        let once = transitive_closure(&graph, Some(false));
        let twice = transitive_closure(&once, Some(false));

        prop_assert_eq!(once.nodes_ordered(), twice.nodes_ordered());
        prop_assert_eq!(edge_set(&once), edge_set(&twice));
    }
}
