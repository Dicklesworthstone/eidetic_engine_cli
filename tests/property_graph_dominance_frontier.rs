use std::collections::{BTreeMap, BTreeSet};

use fnx_algorithms::{dominance_frontiers, immediate_dominators};
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

fn dominates(idoms: &BTreeMap<String, String>, dominator: &str, node: &str) -> bool {
    let mut current = node;
    let mut seen = BTreeSet::new();
    loop {
        if current == dominator {
            return true;
        }
        if !seen.insert(current.to_owned()) {
            return false;
        }
        let Some(parent) = idoms.get(current) else {
            return false;
        };
        if parent == current {
            return false;
        }
        current = parent;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn dominance_frontier_nodes_have_dominated_predecessor_but_are_not_strictly_dominated(
        node_count in 1usize..=18,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = dag_from_seed(node_count, density_percent, seed);
        let start = "n00";
        let idoms = immediate_dominators(&graph, start)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let frontiers = dominance_frontiers(&graph, start)
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        for (dominator, frontier_nodes) in frontiers {
            for frontier_node in frontier_nodes {
                let predecessor_dominated = graph
                    .predecessors(&frontier_node)
                    .unwrap_or_default()
                    .into_iter()
                    .any(|predecessor| dominates(&idoms, &dominator, predecessor));
                prop_assert!(
                    predecessor_dominated,
                    "frontier node {} should have a predecessor dominated by {}",
                    frontier_node,
                    dominator
                );
                prop_assert!(
                    dominator == frontier_node || !dominates(&idoms, &dominator, &frontier_node),
                    "frontier node {} should not be strictly dominated by {}",
                    frontier_node,
                    dominator
                );
            }
        }
    }
}
