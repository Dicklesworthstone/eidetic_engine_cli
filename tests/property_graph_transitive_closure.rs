use std::collections::BTreeSet;

use fnx_algorithms::transitive_closure;
use fnx_classes::digraph::DiGraph;
use fnx_generators::GraphGenerator;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn generated_digraph(node_count: usize, density_percent: u8, seed: u64) -> Result<DiGraph, String> {
    let mut generator = GraphGenerator::strict();
    generator
        .fast_gnp_random_digraph(node_count, f64::from(density_percent) / 100.0, seed)
        .map(|report| report.graph)
        .map_err(|error| error.to_string())
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
