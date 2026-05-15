use std::collections::BTreeMap;

use fnx_algorithms::onion_layers;
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
    fn removing_outer_onion_layer_promotes_next_layer_to_new_outer_layer(
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
        let next_layer_nodes = original_layers
            .iter()
            .filter_map(|(node, layer)| (*layer == outer_layer + 1).then_some(node.clone()))
            .collect::<Vec<_>>();
        if next_layer_nodes.is_empty() {
            return Ok(());
        }

        let mut pruned = graph.clone();
        for node in original_layers
            .iter()
            .filter_map(|(node, layer)| (*layer == outer_layer).then_some(node.clone()))
        {
            let _ = pruned.remove_node(&node);
        }
        let pruned_layers = layer_map(&pruned);

        for node in next_layer_nodes {
            prop_assert_eq!(
                pruned_layers.get(&node).copied(),
                Some(1),
                "node {} from the previous next onion layer should become outer layer",
                node
            );
        }
    }
}
