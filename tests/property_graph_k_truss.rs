use std::collections::BTreeSet;

use fnx_algorithms::k_truss;
use fnx_classes::Graph;
use fnx_generators::GraphGenerator;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn generated_graph(node_count: usize, density_percent: u8, seed: u64) -> Result<Graph, String> {
    let mut generator = GraphGenerator::strict();
    generator
        .gnp_random_graph(node_count, f64::from(density_percent) / 100.0, seed)
        .map(|report| report.graph)
        .map_err(|error| error.to_string())
}

fn nodes_at_k(graph: &Graph, k: usize) -> BTreeSet<String> {
    k_truss(graph, k).nodes.into_iter().collect()
}

fn edges_at_k(graph: &Graph, k: usize) -> BTreeSet<(String, String)> {
    k_truss(graph, k).edges.into_iter().collect()
}

fn max_nonempty_truss_k(graph: &Graph) -> usize {
    (3..=8)
        .filter(|k| !k_truss(graph, *k).nodes.is_empty())
        .max()
        .unwrap_or(2)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn k_truss_membership_is_monotone_under_higher_k(
        node_count in 0usize..=18,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
        k in 3usize..=7,
    ) {
        let graph = generated_graph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;

        let lower_nodes = nodes_at_k(&graph, k);
        let higher_nodes = nodes_at_k(&graph, k + 1);
        let lower_edges = edges_at_k(&graph, k);
        let higher_edges = edges_at_k(&graph, k + 1);

        prop_assert!(
            higher_nodes.is_subset(&lower_nodes),
            "k+1 truss nodes must be subset of k-truss nodes"
        );
        prop_assert!(
            higher_edges.is_subset(&lower_edges),
            "k+1 truss edges must be subset of k-truss edges"
        );
    }

    #[test]
    fn removing_an_edge_never_increases_observed_truss_depth(
        node_count in 2usize..=18,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = generated_graph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;
        let Some(edge) = graph.edges_ordered().into_iter().next() else {
            return Ok(());
        };

        let before = max_nonempty_truss_k(&graph);
        let mut pruned = graph.clone();
        let _ = pruned.remove_edge(&edge.left, &edge.right);
        let after = max_nonempty_truss_k(&pruned);

        prop_assert!(
            after <= before,
            "removing an edge should not increase max observed k-truss depth"
        );
    }
}
