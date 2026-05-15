use fnx_algorithms::{HitsCentralityResult, hits_centrality_directed};
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

fn score_sum(scores: &[fnx_algorithms::CentralityScore]) -> f64 {
    scores.iter().map(|score| score.score).sum()
}

fn hits_l1_normalization_error(result: &HitsCentralityResult) -> Option<String> {
    if let Some(score) = result
        .hubs
        .iter()
        .chain(result.authorities.iter())
        .find(|score| !score.score.is_finite() || score.score < 0.0)
    {
        return Some(format!(
            "HITS score for {} must be finite and nonnegative",
            score.node
        ));
    }

    let hub_sum = score_sum(&result.hubs);
    if (hub_sum - 1.0).abs() > 1.0e-6 {
        return Some(format!("hub sum drifted: {hub_sum}"));
    }

    let authority_sum = score_sum(&result.authorities);
    if (authority_sum - 1.0).abs() > 1.0e-6 {
        return Some(format!("authority sum drifted: {authority_sum}"));
    }

    None
}

fn hits_signature(result: &HitsCentralityResult) -> Vec<(String, String, String)> {
    result
        .hubs
        .iter()
        .zip(result.authorities.iter())
        .map(|(hub, authority)| {
            (
                hub.node.clone(),
                format!("{:.12}", hub.score),
                format!("{:.12}", authority.score),
            )
        })
        .collect()
}

#[test]
fn hits_l1_guard_rejects_deliberate_normalization_regression() -> Result<(), String> {
    let graph = generated_digraph(8, 35, 23)?;
    let mut result = hits_centrality_directed(&graph);
    assert_eq!(hits_l1_normalization_error(&result), None);

    let Some(first) = result.hubs.first_mut() else {
        return Err("HITS fixture should produce at least one hub score".to_owned());
    };
    first.score += 0.01;

    let error = hits_l1_normalization_error(&result)
        .ok_or_else(|| "deliberately denormalized HITS scores were accepted".to_owned())?;
    assert!(
        error.contains("hub sum drifted"),
        "unexpected guard error: {error}"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn hits_scores_are_nonnegative_normalized_and_stable(
        node_count in 1usize..=18,
        density_percent in 0u8..=100,
        seed in any::<u64>(),
    ) {
        let graph = generated_digraph(node_count, density_percent, seed)
            .map_err(TestCaseError::fail)?;

        let first = hits_centrality_directed(&graph);
        let second = hits_centrality_directed(&graph);

        prop_assert_eq!(first.hubs.len(), graph.node_count());
        prop_assert_eq!(first.authorities.len(), graph.node_count());
        prop_assert_eq!(first.hubs.len(), first.authorities.len());
        prop_assert_eq!(hits_l1_normalization_error(&first), None);
        prop_assert_eq!(hits_signature(&first), hits_signature(&second));
    }
}
