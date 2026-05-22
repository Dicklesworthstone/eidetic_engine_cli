use fnx_algorithms::edge_connectivity_edmonds_karp;
use fnx_classes::Graph;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

use ee::graph::gomory_hu::{GOMORY_HU_WEIGHT_ATTR, build_gomory_hu_tree, query_min_cut};

#[path = "support/graph_generator.rs"]
mod graph_generator;

use graph_generator::deterministic_graph;

fn generated_graph(node_count: usize, density_percent: u8, seed: u64) -> Result<Graph, String> {
    deterministic_graph(node_count, f64::from(density_percent) / 100.0, seed)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn gomory_hu_tree_path_cut_bounds_pair_edge_connectivity(
        node_count in 2usize..=12,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = generated_graph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;
        let tree = build_gomory_hu_tree(&graph).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let nodes = graph
            .nodes_ordered()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();

        for (left_index, left) in nodes.iter().enumerate() {
            for right in nodes.iter().skip(left_index + 1) {
                let tree_cut = query_min_cut(&tree, left, right)
                    .ok_or_else(|| TestCaseError::fail(format!("missing tree cut for {left}/{right}")))?;
                let connectivity = edge_connectivity_edmonds_karp(
                    &graph,
                    left,
                    right,
                    GOMORY_HU_WEIGHT_ATTR,
                )
                .map_err(|error| TestCaseError::fail(error.to_string()))?;

                prop_assert!(
                    tree_cut <= connectivity.value + 1.0e-9,
                    "tree path cut {} exceeded edge connectivity {} for {}/{}",
                    tree_cut,
                    connectivity.value,
                    left,
                    right
                );
            }
        }
    }
}
