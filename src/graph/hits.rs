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

use fnx_algorithms::hits_centrality_directed;

use crate::graph::DiGraph;
use crate::graph::GraphResult;
use crate::graph::algorithms::{DEFAULT_BACKGROUND_BUDGET, run_with_budget};

/// Deterministic HITS hub and authority scores for a memory-link DiGraph.
///
/// Each map keys on the memory ID string and orders by `BTreeMap` so
/// downstream serialization is byte-stable.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HitsScores {
    /// Hub score per memory ID (a memory is a "good hub" when it points
    /// to many high-authority memories).
    pub hubs: BTreeMap<String, f64>,
    /// Authority score per memory ID (a memory is a "good authority"
    /// when many good hubs point to it).
    pub authorities: BTreeMap<String, f64>,
}

/// Compute HITS hub/authority scores on a memory-link directed graph.
///
/// Runs `fnx_algorithms::hits_centrality_directed` under the shared
/// background budget so a runaway iteration cannot starve other graph
/// work. The returned `HitsScores` are deterministically ordered by
/// memory ID.
pub fn compute_hits(graph: &DiGraph) -> GraphResult<HitsScores> {
    let graph = graph.clone();
    run_with_budget("hits_centrality", DEFAULT_BACKGROUND_BUDGET, move || {
        let result = hits_centrality_directed(&graph);
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
