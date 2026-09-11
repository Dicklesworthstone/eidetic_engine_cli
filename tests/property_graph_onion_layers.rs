use std::collections::BTreeMap;

use fnx_algorithms::onion_layers;
use fnx_classes::Graph;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

use super::graph_generator::deterministic_graph;

fn generated_graph(node_count: usize, density_percent: u8, seed: u64) -> Result<Graph, String> {
    deterministic_graph(node_count, f64::from(density_percent) / 100.0, seed)
}

fn layer_map(graph: &Graph) -> BTreeMap<String, usize> {
    onion_layers(graph)
        .layers
        .into_iter()
        .map(|layer| (layer.node, layer.layer))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn new_outer_nodes_come_from_the_previous_next_onion_layer(
        node_count in 0usize..=22,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = generated_graph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;
        let original_layers = layer_map(&graph);
        let Some(outer_layer) = original_layers.values().copied().min() else {
            return Ok(());
        };
        let mut pruned = graph.clone();
        for node in original_layers
            .iter()
            .filter_map(|(node, layer)| (*layer == outer_layer).then_some(node.clone()))
        {
            let _ = pruned.remove_node(&node);
        }
        let pruned_layers = layer_map(&pruned);
        prop_assert_eq!(pruned_layers.len(), pruned.node_count());
        prop_assert!(pruned_layers.is_empty() || pruned_layers.values().any(|layer| *layer == 1));

        // Restarting decomposition resets the core threshold and handles newly
        // isolated nodes first. The new outer layer is a subset of the old
        // second layer; the reverse inclusion need not hold.
        for (node, layer) in &pruned_layers {
            if *layer != 1 {
                continue;
            }
            prop_assert_eq!(
                original_layers.get(node).copied(),
                Some(outer_layer + 1),
                "new outer node {} must belong to the previous next onion layer",
                node
            );
        }
    }
}

#[test]
fn restarting_onion_peeling_can_split_the_previous_second_layer() -> Result<(), String> {
    // Independent NetworkX 3.6.1 reference: disjoint P3 and P4. Removing the
    // leaves leaves an isolate and an edge. Only the isolate is in the new
    // outer layer, although all three survivors were in the old second layer.
    // https://networkx.org/documentation/stable/_modules/networkx/algorithms/core.html#onion_layers
    let mut graph = Graph::strict();
    for node in 0..7 {
        let _ = graph.add_node(node.to_string());
    }
    for (left, right) in [(0, 1), (1, 2), (3, 4), (4, 5), (5, 6)] {
        graph
            .add_edge(left.to_string(), right.to_string())
            .map_err(|error| error.to_string())?;
    }
    assert_eq!(
        layer_map(&graph),
        [(0, 1), (1, 2), (2, 1), (3, 1), (4, 2), (5, 2), (6, 1)]
            .into_iter()
            .map(|(node, layer)| (node.to_string(), layer))
            .collect()
    );
    for node in [0, 2, 3, 6] {
        let _ = graph.remove_node(&node.to_string());
    }
    assert_eq!(
        layer_map(&graph),
        [(1, 1), (4, 2), (5, 2)]
            .into_iter()
            .map(|(node, layer)| (node.to_string(), layer))
            .collect()
    );
    Ok(())
}
