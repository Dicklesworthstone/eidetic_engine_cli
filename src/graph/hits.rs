//! HITS (Hyperlink-Induced Topic Search) wrapper for memory-link graphs
//! (bd-jy4w.1 / G10.a).
//!
//! `compute_hits` returns deterministic hub and authority scores for a
//! directed memory-link graph by delegating to
//! `fnx_algorithms::hits_centrality_directed`. The result is stored in
//! `BTreeMap<String, f64>` so the same graph + same algorithm parameters
//! produce byte-identical output (the J7 determinism contract).
//!
//! The function executes under the shared algorithm-budget wrapper so
//! cancellation, panic capture, and budget accounting match every other
//! graph wrapper (PageRank, betweenness, Gomory-Hu, causal explanation).
//!
//! Snapshot caching during `refresh_centrality` (the second half of
//! G10.a's acceptance) is intentionally deferred to a follow-on slice so
//! this file remains self-contained and small. The wrapper here is the
//! computational primitive the snapshot job will call.
//!
//! Downstream consumers (G10.b `ee context --profile grounding`,
//! G10.c `ee insights --section hubs/authorities`) consume this same
//! `HitsScores` shape.

use std::collections::BTreeMap;

use asupersync::Cx;
use fnx_algorithms::{HitsCentralityResult, hits_centrality_directed};
use serde::{Serialize, Serializer};

use crate::core::degraded_aggregation::{
    AggregatedDegradation, DegradationAggregationInput, aggregate_degraded_entries,
};
use crate::graph::DiGraph;
use crate::graph::GraphResult;
use crate::graph::algorithms::{DEFAULT_BACKGROUND_BUDGET, current_or_testing_cx, run_with_budget};
use crate::models::degradation::GRAPH_HITS_CONVERGENCE_FAILURE_CODE;

pub const HITS_REPORT_SCHEMA_V1: &str = "ee.graph.hits.v1";

const HITS_CONVERGENCE_ITERATION_CAP: usize = 100;

/// Deterministic HITS hub and authority scores for a memory-link DiGraph.
///
/// Each map keys on the memory ID string and orders by `BTreeMap` so
/// downstream serialization is byte-stable.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HitsScores {
    /// Hub score per memory ID (a memory is a "good hub" when it points
    /// to many high-authority memories).
    pub hubs: BTreeMap<String, f64>,
    /// Authority score per memory ID (a memory is a "good authority"
    /// when many good hubs point to it).
    pub authorities: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HitsReport {
    pub schema: &'static str,
    pub scores: HitsScores,
    #[serde(serialize_with = "serialize_hits_degraded")]
    pub degraded: Vec<HitsDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HitsDegradation {
    pub code: String,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

fn serialize_hits_degraded<S>(
    degraded: &[HitsDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_hits_degraded(degraded).serialize(serializer)
}

fn aggregate_hits_degraded(degraded: &[HitsDegradation]) -> Vec<AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "hits",
            entry.code.clone(),
            entry.severity,
            entry.message.clone(),
            entry
                .repair
                .clone()
                .unwrap_or_else(|| "Refresh graph HITS diagnostics.".to_owned()),
        )
    }))
}

/// Compute HITS hub/authority scores on a memory-link directed graph.
///
/// Runs `fnx_algorithms::hits_centrality_directed` under the shared
/// background budget so a runaway iteration cannot starve other graph
/// work. The returned `HitsScores` are deterministically ordered by
/// memory ID.
pub fn compute_hits(graph: &DiGraph) -> GraphResult<HitsScores> {
    compute_hits_result(graph).map(hits_scores_from_result)
}

pub fn compute_hits_report(graph: &DiGraph) -> GraphResult<HitsReport> {
    let result = compute_hits_result(graph)?;
    let degraded = hits_convergence_degradations(graph.nodes_ordered().len(), &result);
    Ok(HitsReport {
        schema: HITS_REPORT_SCHEMA_V1,
        scores: hits_scores_from_result(result),
        degraded,
    })
}

fn compute_hits_result(graph: &DiGraph) -> GraphResult<HitsCentralityResult> {
    let cx = current_or_testing_cx();
    compute_hits_result_with_cx(&cx, graph)
}

fn compute_hits_result_with_cx(cx: &Cx, graph: &DiGraph) -> GraphResult<HitsCentralityResult> {
    let graph = graph.clone();
    run_with_budget(
        cx,
        "hits_centrality",
        DEFAULT_BACKGROUND_BUDGET,
        move || hits_centrality_directed(&graph),
    )
}

fn hits_scores_from_result(result: HitsCentralityResult) -> HitsScores {
    let hubs = result
        .hubs
        .into_iter()
        .map(|score| (score.node, score.score))
        .collect::<BTreeMap<_, _>>();
    let authorities = result
        .authorities
        .into_iter()
        .map(|score| (score.node, score.score))
        .collect::<BTreeMap<_, _>>();
    HitsScores { hubs, authorities }
}

fn hits_convergence_degradations(
    node_count: usize,
    result: &HitsCentralityResult,
) -> Vec<HitsDegradation> {
    let Some(iterations) = hits_iteration_count(node_count, result) else {
        return Vec::new();
    };
    if iterations < HITS_CONVERGENCE_ITERATION_CAP {
        return Vec::new();
    }

    vec![HitsDegradation {
        code: GRAPH_HITS_CONVERGENCE_FAILURE_CODE.to_owned(),
        severity: "warning",
        message: format!(
            "HITS centrality reached the {HITS_CONVERGENCE_ITERATION_CAP}-iteration cap before a convergence witness was available."
        ),
        repair: Some("ee graph snapshot refresh --workspace .".to_owned()),
    }]
}

fn hits_iteration_count(node_count: usize, result: &HitsCentralityResult) -> Option<usize> {
    if node_count <= 1 {
        return None;
    }
    let denominator = node_count.saturating_mul(2);
    (denominator > 0).then_some(result.witness.nodes_touched / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_algorithms::{CentralityScore, ComplexityWitness};
    use fnx_runtime::CompatibilityMode;

    type TestResult = Result<(), String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    fn empty_digraph() -> DiGraph {
        DiGraph::new(CompatibilityMode::Strict)
    }

    fn add_edge(graph: &mut DiGraph, source: &str, target: &str) {
        graph
            .add_edge(source, target)
            .unwrap_or_else(|error| panic!("test edge {source}->{target} should add: {error:?}"));
    }

    #[test]
    fn hits_empty_graph_returns_empty_scores() -> TestResult {
        let graph = empty_digraph();

        let scores = graph_result(compute_hits(&graph))?;

        assert!(scores.hubs.is_empty(), "empty graph must yield empty hubs");
        assert!(
            scores.authorities.is_empty(),
            "empty graph must yield empty authorities"
        );
        Ok(())
    }

    #[test]
    fn hits_single_node_assigns_uniform_one() -> TestResult {
        let mut graph = empty_digraph();
        graph.add_node("solo");

        let scores = graph_result(compute_hits(&graph))?;

        assert_eq!(scores.hubs.get("solo").copied(), Some(1.0));
        assert_eq!(scores.authorities.get("solo").copied(), Some(1.0));
        assert_eq!(scores.hubs.len(), 1);
        assert_eq!(scores.authorities.len(), 1);
        Ok(())
    }

    #[test]
    fn hits_star_authority_dominates_center() -> TestResult {
        // a, c, d each point to b. b is the single authority; a, c, d
        // are hubs of equal hub score; b's hub score is the floor since
        // it has no outgoing edge.
        let mut graph = empty_digraph();
        for source in ["a", "c", "d"] {
            add_edge(&mut graph, source, "b");
        }

        let scores = graph_result(compute_hits(&graph))?;

        let center_authority = scores.authorities.get("b").copied().unwrap_or(0.0);
        let center_hub = scores.hubs.get("b").copied().unwrap_or(0.0);
        for spoke in ["a", "c", "d"] {
            let spoke_authority = scores.authorities.get(spoke).copied().unwrap_or(0.0);
            let spoke_hub = scores.hubs.get(spoke).copied().unwrap_or(0.0);
            assert!(
                spoke_hub > spoke_authority,
                "spoke {spoke} should be a stronger hub ({spoke_hub}) than authority ({spoke_authority})"
            );
            assert!(
                center_authority > spoke_authority,
                "center should out-score spoke {spoke} as authority ({center_authority} vs {spoke_authority})"
            );
            assert!(
                spoke_hub > center_hub,
                "spoke {spoke} should out-score center as hub ({spoke_hub} vs {center_hub})"
            );
        }
        // All three spokes share the same hub score by symmetry.
        let spoke_hubs: Vec<f64> = ["a", "c", "d"]
            .iter()
            .map(|spoke| scores.hubs.get(*spoke).copied().unwrap_or(0.0))
            .collect();
        for value in &spoke_hubs[1..] {
            assert!(
                (value - spoke_hubs[0]).abs() < 1.0e-9,
                "symmetric spokes must share hub score: {value} vs {}",
                spoke_hubs[0]
            );
        }
        Ok(())
    }

    #[test]
    fn hits_complete_directed_graph_is_uniform() -> TestResult {
        // Complete directed graph (no self-loops): by symmetry every
        // node has the same hub and authority score.
        let nodes = ["a", "b", "c", "d"];
        let mut graph = empty_digraph();
        for source in &nodes {
            for target in &nodes {
                if source != target {
                    add_edge(&mut graph, source, target);
                }
            }
        }

        let scores = graph_result(compute_hits(&graph))?;

        assert_eq!(scores.hubs.len(), nodes.len());
        assert_eq!(scores.authorities.len(), nodes.len());
        let hub_values: Vec<f64> = scores.hubs.values().copied().collect();
        let authority_values: Vec<f64> = scores.authorities.values().copied().collect();
        let first_hub = hub_values[0];
        for value in &hub_values[1..] {
            assert!(
                (value - first_hub).abs() < 1.0e-9,
                "complete graph must have uniform hub scores: {value} vs {first_hub}"
            );
        }
        let first_authority = authority_values[0];
        for value in &authority_values[1..] {
            assert!(
                (value - first_authority).abs() < 1.0e-9,
                "complete graph must have uniform authority scores: {value} vs {first_authority}"
            );
        }
        Ok(())
    }

    #[test]
    fn hits_deterministic_across_three_runs() -> TestResult {
        // Non-trivial directed graph with a back edge so HITS power
        // iteration produces non-uniform scores. Three repeated calls
        // must produce byte-identical `HitsScores`.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "b", "c");
        add_edge(&mut graph, "c", "a");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "d", "b");

        let first = graph_result(compute_hits(&graph))?;
        let second = graph_result(compute_hits(&graph))?;
        let third = graph_result(compute_hits(&graph))?;

        assert_eq!(first, second, "HITS must be deterministic across two runs");
        assert_eq!(
            second, third,
            "HITS must be deterministic across three runs"
        );
        // Sanity check non-uniformity so the determinism test is not
        // trivially satisfied by a degenerate output.
        let hub_values: Vec<f64> = first.hubs.values().copied().collect();
        let max = hub_values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let min = hub_values.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(
            max - min > 1.0e-6,
            "deterministic-run fixture should produce non-uniform hubs (range {min}..={max})"
        );
        Ok(())
    }

    #[test]
    fn hits_report_emits_convergence_failure_when_iteration_cap_reached() {
        let result = HitsCentralityResult {
            hubs: vec![CentralityScore {
                node: "a".to_owned(),
                score: 0.5,
            }],
            authorities: vec![CentralityScore {
                node: "a".to_owned(),
                score: 0.5,
            }],
            witness: ComplexityWitness {
                algorithm: "hits_centrality_power_iteration".to_owned(),
                complexity_claim: "O(k * (|V| + |E|))".to_owned(),
                nodes_touched: 2_usize
                    .saturating_mul(2)
                    .saturating_mul(HITS_CONVERGENCE_ITERATION_CAP),
                edges_scanned: 0,
                queue_peak: 0,
            },
        };

        let degraded = hits_convergence_degradations(2, &result);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, GRAPH_HITS_CONVERGENCE_FAILURE_CODE);
        assert_eq!(degraded[0].severity, "warning");
        assert!(
            degraded[0].message.contains("iteration cap"),
            "message should explain the convergence cap: {}",
            degraded[0].message
        );
    }

    #[test]
    fn hits_report_degraded_entries_are_aggregated() {
        let report = HitsReport {
            schema: HITS_REPORT_SCHEMA_V1,
            scores: HitsScores::default(),
            degraded: vec![
                HitsDegradation {
                    code: GRAPH_HITS_CONVERGENCE_FAILURE_CODE.to_owned(),
                    severity: "warning",
                    message: "HITS convergence warning.".to_owned(),
                    repair: Some("Refresh HITS snapshot.".to_owned()),
                },
                HitsDegradation {
                    code: GRAPH_HITS_CONVERGENCE_FAILURE_CODE.to_owned(),
                    severity: "medium",
                    message: "HITS convergence failed at the cap.".to_owned(),
                    repair: Some("Rebuild HITS snapshot.".to_owned()),
                },
            ],
        };

        let value = serde_json::to_value(&report).expect("hits report serializes");
        let degraded = value["degraded"].as_array().expect("degraded array");

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0]["code"], GRAPH_HITS_CONVERGENCE_FAILURE_CODE);
        assert_eq!(degraded[0]["severity"], "medium");
        assert_eq!(degraded[0]["sources"], serde_json::json!(["hits"]));
    }
}
