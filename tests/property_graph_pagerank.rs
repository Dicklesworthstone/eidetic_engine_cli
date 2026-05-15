use fnx_algorithms::{PageRankResult, pagerank};
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

fn pagerank_signature(result: &PageRankResult) -> Vec<(String, String)> {
    result
        .scores
        .iter()
        .map(|score| (score.node.clone(), format!("{:.12}", score.score)))
        .collect()
}

fn pagerank_probability_weight_error(result: &PageRankResult) -> Option<String> {
    let score_sum = result.scores.iter().map(|score| score.score).sum::<f64>();
    if (score_sum - 1.0).abs() > 1.0e-6 {
        return Some(format!("pagerank score sum should be 1.0, got {score_sum}"));
    }

    result.scores.iter().find_map(|score| {
        if !score.score.is_finite() {
            Some(format!("pagerank score for {} must be finite", score.node))
        } else if score.score <= 0.0 {
            Some(format!(
                "pagerank score for {} must be positive",
                score.node
            ))
        } else {
            None
        }
    })
}

#[test]
fn pagerank_probability_guard_rejects_deliberate_normalization_regression() -> Result<(), String> {
    let graph = generated_graph(8, 45, 17)?;
    let mut result = pagerank(&graph);
    assert_eq!(pagerank_probability_weight_error(&result), None);

    let Some(first) = result.scores.first_mut() else {
        return Err("pagerank fixture should produce at least one score".to_owned());
    };
    first.score += 0.01;

    let error = pagerank_probability_weight_error(&result)
        .ok_or_else(|| "deliberately denormalized PageRank scores were accepted".to_owned())?;
    assert!(
        error.contains("score sum should be 1.0"),
        "unexpected guard error: {error}"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn pagerank_scores_are_positive_stable_probability_weights(
        node_count in 1usize..=24,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = generated_graph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;

        let first = pagerank(&graph);
        let second = pagerank(&graph);

        prop_assert_eq!(first.scores.len(), graph.node_count());
        prop_assert_eq!(pagerank_probability_weight_error(&first), None);
        prop_assert_eq!(pagerank_signature(&first), pagerank_signature(&second));
    }
}
