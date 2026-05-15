use std::collections::BTreeMap;

use fnx_algorithms::topological_sort;
use fnx_classes::digraph::DiGraph;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn dag_from_seed(node_count: usize, density_percent: u8, seed: u64) -> DiGraph {
    let mut graph = DiGraph::strict();
    for node in 0..node_count {
        let _ = graph.add_node(format!("n{node:02}"));
    }
    for source in 0..node_count {
        for target in (source + 1)..node_count {
            if include_edge(seed, source, target, density_percent) {
                let _ = graph.add_edge(format!("n{source:02}"), format!("n{target:02}"));
            }
        }
    }
    graph
}

fn include_edge(seed: u64, source: usize, target: usize, density_percent: u8) -> bool {
    let mixed = seed
        ^ ((source as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
        ^ ((target as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9));
    mixed % 100 < u64::from(density_percent)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn topological_sort_respects_every_dag_edge(
        node_count in 0usize..=24,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = dag_from_seed(node_count, density_percent, seed);
        let Some(sorted) = topological_sort(&graph) else {
            return Err(TestCaseError::fail("generated DAG unexpectedly had a cycle"));
        };
        let positions = sorted
            .order
            .iter()
            .enumerate()
            .map(|(index, node)| (node.as_str(), index))
            .collect::<BTreeMap<_, _>>();

        prop_assert_eq!(positions.len(), graph.node_count());
        for edge in graph.edges_ordered() {
            let Some(source_position) = positions.get(edge.left.as_str()).copied() else {
                return Err(TestCaseError::fail(format!(
                    "topological order omitted source {}",
                    edge.left
                )));
            };
            let Some(target_position) = positions.get(edge.right.as_str()).copied() else {
                return Err(TestCaseError::fail(format!(
                    "topological order omitted target {}",
                    edge.right
                )));
            };
            prop_assert!(
                source_position < target_position,
                "edge {} -> {} violates topological order",
                edge.left,
                edge.right
            );
        }
    }
}
