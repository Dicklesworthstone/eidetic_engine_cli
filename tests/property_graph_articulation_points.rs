use fnx_algorithms::{articulation_points, connected_components};
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn removing_articulation_point_increases_component_count(
        node_count in 0usize..=22,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = generated_graph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;
        let before = connected_components(&graph).components.len();
        let articulation = articulation_points(&graph);

        for node in articulation.nodes {
            let mut pruned = graph.clone();
            let removed = pruned.remove_node(&node);
            prop_assert!(removed, "articulation point should be present in graph");
            let after = connected_components(&pruned).components.len();
            prop_assert!(
                after > before,
                "removing articulation point {} should increase components: before={} after={}",
                node,
                before,
                after
            );
        }
    }
}
