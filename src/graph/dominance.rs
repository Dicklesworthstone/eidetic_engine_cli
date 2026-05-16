//! Dominance + LCA wrappers for the memory-revision DAG
//! (bd-a7mm.1 / G7.a).
//!
//! The revision DAG produced by `build_revision_dag_from_logical_ids`
//! captures the partial order of memory revisions: edges point from
//! older to newer rows within a shared `logical_id` chain, plus
//! `derived_from` cross-chain edges. These wrappers expose three
//! dominance/LCA primitives over that DAG:
//!
//! - [`compute_immediate_dominators`] — for every node reachable from
//!   `start`, return its immediate dominator. A node `D` is the
//!   immediate dominator of `N` when every path from `start` to `N`
//!   passes through `D` and `D` is the closest such node.
//! - [`compute_dominance_frontiers`] — for every node, the set of
//!   nodes whose immediate dominator is NOT an ancestor of the
//!   queried node but which can be reached from a successor that the
//!   queried node dominates. Useful for SSA-style "where does this
//!   revision's influence stop" reasoning.
//! - [`compute_all_pairs_lca`] — for every unordered pair of nodes,
//!   their lowest common ancestor (or `None` when no common ancestor
//!   exists in this DAG).
//!
//! All public maps are `BTreeMap` (or sorted `Vec`) so the same graph
//! plus the same input parameters produce byte-identical output (J7).
//!
//! Downstream surfaces (G7.b `ee why dominator`, G7.c
//! `ee.memory.impact_analysis.v1 schema + ee insights --section
//! revisionFrontiers`) consume the maps produced here.

use std::collections::{BTreeMap, BTreeSet};

use asupersync::Cx;
use fnx_algorithms::{all_pairs_lowest_common_ancestor, dominance_frontiers, immediate_dominators};
use serde::{Serialize, Serializer};

use crate::core::degraded_aggregation::{
    AggregatedDegradation, DegradationAggregationInput, aggregate_degraded_entries,
};
use crate::graph::DiGraph;
use crate::graph::GraphResult;
use crate::graph::algorithms::{DEFAULT_BACKGROUND_BUDGET, current_or_testing_cx, run_with_budget};
use crate::models::degradation::GRAPH_DOMINANCE_NO_REVISION_CHAIN_CODE;

pub const MEMORY_IMPACT_ANALYSIS_SCHEMA_V1: &str = "ee.memory.impact_analysis.v1";

/// Immediate dominators keyed by node ID. The underlying fnx contract
/// includes `start -> start`; nodes unreachable from `start` are omitted.
pub type ImmediateDominators = BTreeMap<String, String>;

/// Dominance frontier per node keyed by node ID. The value is the
/// sorted, deduplicated list of frontier node IDs.
pub type DominanceFrontiers = BTreeMap<String, Vec<String>>;

/// All-pairs LCA result keyed by `(left, right)` with `left <= right`
/// lexicographically, including self-pairs. The value is `Some(lca)`
/// when the two nodes share a common ancestor in the DAG, otherwise
/// `None`.
pub type AllPairsLca = BTreeMap<(String, String), Option<String>>;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryImpactAnalysisReport {
    pub schema: &'static str,
    pub memory_id: String,
    pub snapshot_version: u64,
    pub revision_lineage: Vec<RevisionLineageItem>,
    pub impact_analysis: RevisionImpactAnalysis,
    pub frontiers: Vec<RevisionFrontierItem>,
    #[serde(serialize_with = "serialize_dominance_degraded")]
    pub degraded: Vec<DominanceDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionLineageItem {
    pub memory_id: String,
    pub logical_id: String,
    pub depth: usize,
    pub relation: String,
    pub valid_from: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionImpactAnalysis {
    pub immediate_dominator: Option<String>,
    pub dominance_frontier: Vec<String>,
    pub affected_memory_count: usize,
    pub validation_status: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionFrontierItem {
    pub memory_id: String,
    pub dominance_frontier_size: usize,
    pub affected_memory_ids: Vec<String>,
    pub evidence: RevisionFrontierEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionFrontierEvidence {
    pub algorithm: &'static str,
    pub snapshot_version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominanceDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

fn serialize_dominance_degraded<S>(
    degraded: &[DominanceDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_dominance_degraded(degraded).serialize(serializer)
}

fn aggregate_dominance_degraded(degraded: &[DominanceDegradation]) -> Vec<AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "graph_dominance",
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry
                .repair
                .clone()
                .unwrap_or_else(|| "Refresh graph dominance diagnostics.".to_owned()),
        )
    }))
}

/// Compute immediate dominators starting from `start`. Returns an
/// empty map when `start` is missing from the graph.
pub fn compute_immediate_dominators(
    graph: &DiGraph,
    start: &str,
) -> GraphResult<ImmediateDominators> {
    let cx = current_or_testing_cx();
    compute_immediate_dominators_with_cx(&cx, graph, start)
}

pub fn compute_immediate_dominators_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    start: &str,
) -> GraphResult<ImmediateDominators> {
    let graph = graph.clone();
    let start = start.to_owned();
    run_with_budget(
        cx,
        "immediate_dominators",
        DEFAULT_BACKGROUND_BUDGET,
        move || {
            immediate_dominators(&graph, &start)
                .into_iter()
                .collect::<ImmediateDominators>()
        },
    )
}

/// Compute dominance frontiers starting from `start`. Each frontier
/// list is sorted and deduplicated for deterministic output.
pub fn compute_dominance_frontiers(
    graph: &DiGraph,
    start: &str,
) -> GraphResult<DominanceFrontiers> {
    let cx = current_or_testing_cx();
    compute_dominance_frontiers_with_cx(&cx, graph, start)
}

pub fn compute_dominance_frontiers_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    start: &str,
) -> GraphResult<DominanceFrontiers> {
    let graph = graph.clone();
    let start = start.to_owned();
    run_with_budget(
        cx,
        "dominance_frontiers",
        DEFAULT_BACKGROUND_BUDGET,
        move || {
            dominance_frontiers(&graph, &start)
                .into_iter()
                .map(|(node, mut frontier)| {
                    frontier.sort();
                    frontier.dedup();
                    (node, frontier)
                })
                .collect::<DominanceFrontiers>()
        },
    )
}

/// Compute LCA for every unordered pair of nodes in the graph,
/// including self-pairs. Each pair is stored canonically with `left <=
/// right`. Pairs without a common ancestor (e.g. disconnected
/// components) are stored with value `None`.
pub fn compute_all_pairs_lca(graph: &DiGraph) -> GraphResult<AllPairsLca> {
    let cx = current_or_testing_cx();
    compute_all_pairs_lca_with_cx(&cx, graph)
}

pub fn compute_all_pairs_lca_with_cx(cx: &Cx, graph: &DiGraph) -> GraphResult<AllPairsLca> {
    let graph = graph.clone();
    run_with_budget(cx, "all_pairs_lca", DEFAULT_BACKGROUND_BUDGET, move || {
        let mut nodes: Vec<String> = graph
            .nodes_ordered()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        nodes.sort();
        nodes.dedup();
        let mut distinct_pairs: Vec<(String, String)> = Vec::new();
        let mut result: AllPairsLca = BTreeMap::new();
        for (index, left) in nodes.iter().enumerate() {
            result.insert((left.clone(), left.clone()), Some(left.clone()));
            for right in nodes.iter().skip(index + 1) {
                let pair = (left.clone(), right.clone());
                result.insert(pair.clone(), None);
                distinct_pairs.push(pair);
            }
        }
        for (pair, lca) in all_pairs_lowest_common_ancestor(&graph, &distinct_pairs) {
            result.insert(pair, Some(lca));
        }
        result
    })
}

pub fn compute_memory_impact_analysis(
    graph: &DiGraph,
    memory_id: &str,
    snapshot_version: u64,
) -> GraphResult<MemoryImpactAnalysisReport> {
    let cx = current_or_testing_cx();
    compute_memory_impact_analysis_with_cx(&cx, graph, memory_id, snapshot_version)
}

pub fn compute_memory_impact_analysis_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    memory_id: &str,
    snapshot_version: u64,
) -> GraphResult<MemoryImpactAnalysisReport> {
    if !has_revision_chain_context(graph, memory_id) {
        return Ok(MemoryImpactAnalysisReport {
            schema: MEMORY_IMPACT_ANALYSIS_SCHEMA_V1,
            memory_id: memory_id.to_owned(),
            snapshot_version,
            revision_lineage: Vec::new(),
            impact_analysis: RevisionImpactAnalysis {
                immediate_dominator: None,
                dominance_frontier: Vec::new(),
                affected_memory_count: 0,
                validation_status: "unavailable".to_owned(),
            },
            frontiers: Vec::new(),
            degraded: vec![dominance_no_revision_chain_degradation(memory_id)],
        });
    }

    let analysis_start = revision_analysis_start(graph, memory_id);
    let idoms = compute_immediate_dominators_with_cx(cx, graph, &analysis_start)?;
    let frontiers = compute_dominance_frontiers_with_cx(cx, graph, &analysis_start)?;
    let dominance_frontier = frontiers.get(memory_id).cloned().unwrap_or_default();
    let immediate_dominator = idoms
        .get(memory_id)
        .filter(|dominator| dominator.as_str() != memory_id)
        .cloned();
    let affected_memory_count = idoms
        .keys()
        .filter(|node| dominates_node(&idoms, memory_id, node))
        .count();

    Ok(MemoryImpactAnalysisReport {
        schema: MEMORY_IMPACT_ANALYSIS_SCHEMA_V1,
        memory_id: memory_id.to_owned(),
        snapshot_version,
        revision_lineage: revision_lineage_for_query(&idoms, &analysis_start, memory_id),
        impact_analysis: RevisionImpactAnalysis {
            immediate_dominator,
            dominance_frontier: dominance_frontier.clone(),
            affected_memory_count,
            validation_status: "valid".to_owned(),
        },
        frontiers: revision_frontier_items(&frontiers, snapshot_version),
        degraded: Vec::new(),
    })
}

fn revision_analysis_start(graph: &DiGraph, memory_id: &str) -> String {
    let mut seen = BTreeSet::new();
    let mut frontier = BTreeSet::from([memory_id.to_owned()]);
    let mut roots = BTreeSet::new();

    while let Some(node) = frontier.pop_first() {
        if !seen.insert(node.clone()) {
            continue;
        }
        let predecessors = graph.predecessors(&node).unwrap_or_default();
        if predecessors.is_empty() {
            roots.insert(node);
        } else {
            frontier.extend(predecessors.into_iter().map(ToOwned::to_owned));
        }
    }

    roots
        .into_iter()
        .next()
        .unwrap_or_else(|| memory_id.to_owned())
}

fn has_revision_chain_context(graph: &DiGraph, memory_id: &str) -> bool {
    if !graph.has_node(memory_id) {
        return false;
    }
    !graph.successors(memory_id).unwrap_or_default().is_empty()
        || !graph.predecessors(memory_id).unwrap_or_default().is_empty()
}

fn dominance_no_revision_chain_degradation(memory_id: &str) -> DominanceDegradation {
    DominanceDegradation {
        code: GRAPH_DOMINANCE_NO_REVISION_CHAIN_CODE.to_owned(),
        severity: "info".to_owned(),
        message: format!(
            "Memory {memory_id} has no logical_id revision chain for dominance impact analysis."
        ),
        repair: None,
    }
}

fn revision_lineage_for_query(
    idoms: &ImmediateDominators,
    start: &str,
    memory_id: &str,
) -> Vec<RevisionLineageItem> {
    if !idoms.contains_key(memory_id) {
        return Vec::new();
    }

    let mut items = Vec::new();
    let mut current = memory_id.to_owned();
    let mut seen = BTreeSet::new();
    let mut depth = 0usize;

    loop {
        if !seen.insert(current.clone()) {
            break;
        }
        items.push(RevisionLineageItem {
            memory_id: current.clone(),
            logical_id: start.to_owned(),
            depth,
            relation: if depth == 0 {
                "self".to_owned()
            } else {
                "ancestor".to_owned()
            },
            valid_from: None,
        });

        let Some(parent) = idoms.get(&current) else {
            break;
        };
        if parent == &current {
            break;
        }
        current = parent.clone();
        depth = depth.saturating_add(1);
    }

    items
}

fn dominates_node(idoms: &ImmediateDominators, dominator: &str, node: &str) -> bool {
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

fn revision_frontier_items(
    frontiers: &DominanceFrontiers,
    snapshot_version: u64,
) -> Vec<RevisionFrontierItem> {
    frontiers
        .iter()
        .map(|(memory_id, frontier)| RevisionFrontierItem {
            memory_id: memory_id.clone(),
            dominance_frontier_size: frontier.len(),
            affected_memory_ids: frontier.clone(),
            evidence: RevisionFrontierEvidence {
                algorithm: "dominance_frontiers",
                snapshot_version,
            },
        })
        .collect()
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
            .unwrap_or_else(|error| panic!("test edge {source}→{target} should add: {error:?}"));
    }

    #[test]
    fn immediate_dominators_linear_chain_walks_predecessors() -> TestResult {
        // a -> b -> c -> d : every node except `a` is dominated by its
        // immediate predecessor.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "b", "c");
        add_edge(&mut graph, "c", "d");

        let idoms = graph_result(compute_immediate_dominators(&graph, "a"))?;

        assert_eq!(idoms.get("b").map(String::as_str), Some("a"));
        assert_eq!(idoms.get("c").map(String::as_str), Some("b"));
        assert_eq!(idoms.get("d").map(String::as_str), Some("c"));
        // fnx includes the entry node as its own dominator.
        assert_eq!(idoms.get("a").map(String::as_str), Some("a"));
        Ok(())
    }

    #[test]
    fn immediate_dominators_branching_dag_picks_common_predecessor() -> TestResult {
        // Diamond:
        //   a -> b -> d
        //   a -> c -> d
        // d's idom is `a` because both b and c are reachable from a.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "d");
        add_edge(&mut graph, "c", "d");

        let idoms = graph_result(compute_immediate_dominators(&graph, "a"))?;

        assert_eq!(idoms.get("b").map(String::as_str), Some("a"));
        assert_eq!(idoms.get("c").map(String::as_str), Some("a"));
        assert_eq!(idoms.get("d").map(String::as_str), Some("a"));
        Ok(())
    }

    #[test]
    fn dominance_frontiers_chain_with_cross_derive_records_join() -> TestResult {
        // Diamond produces a frontier at the join node.
        //   a -> b -> d
        //   a -> c -> d
        // Frontier(b) and Frontier(c) both include d, because d is
        // reachable from each branch but is not dominated by either.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "d");
        add_edge(&mut graph, "c", "d");

        let frontiers = graph_result(compute_dominance_frontiers(&graph, "a"))?;

        assert_eq!(frontiers.get("b").cloned().unwrap_or_default(), vec!["d"]);
        assert_eq!(frontiers.get("c").cloned().unwrap_or_default(), vec!["d"]);
        // Nodes whose dominance frontier is empty must still be sorted
        // when present.
        if let Some(start_frontier) = frontiers.get("a") {
            let mut sorted = start_frontier.clone();
            sorted.sort();
            assert_eq!(*start_frontier, sorted);
        }
        Ok(())
    }

    #[test]
    fn dominance_frontiers_multi_root_returns_empty_for_unreachable() -> TestResult {
        // Two disjoint chains; computing frontiers from `a` ignores
        // the second chain entirely.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "x", "y");

        let frontiers = graph_result(compute_dominance_frontiers(&graph, "a"))?;

        // `b`'s frontier is empty (it has no successors).
        let b_frontier = frontiers.get("b").cloned().unwrap_or_default();
        assert!(
            b_frontier.is_empty(),
            "linear-chain tail has empty frontier; got {b_frontier:?}"
        );
        // Disconnected nodes are absent from the result rather than
        // mapped to bogus frontiers.
        assert!(
            !frontiers.contains_key("y"),
            "unreachable-from-start nodes must not appear in frontiers map"
        );
        Ok(())
    }

    #[test]
    fn all_pairs_lca_branching_chain_finds_join_ancestor() -> TestResult {
        // Diamond where every pair shares `a` as their lowest common
        // ancestor (a is the only top-of-DAG).
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "d");
        add_edge(&mut graph, "c", "d");
        add_edge(&mut graph, "x", "y");

        let lca = graph_result(compute_all_pairs_lca(&graph))?;

        assert_eq!(
            lca.get(&("b".to_owned(), "b".to_owned()))
                .cloned()
                .unwrap_or(None),
            Some("b".to_owned()),
            "self-pairs must report the node itself as LCA"
        );
        assert_eq!(
            lca.get(&("b".to_owned(), "c".to_owned()))
                .cloned()
                .unwrap_or(None),
            Some("a".to_owned()),
            "siblings b and c must share ancestor a"
        );
        assert_eq!(
            lca.get(&("b".to_owned(), "d".to_owned()))
                .cloned()
                .unwrap_or(None),
            Some("b".to_owned()),
            "ancestor pair (b, d) must report b as LCA"
        );
        assert_eq!(
            lca.get(&("b".to_owned(), "x".to_owned()))
                .cloned()
                .unwrap_or(Some("unexpected".to_owned())),
            None,
            "disconnected roots must not invent an LCA"
        );
        // Canonical key order: every stored pair has left <= right.
        for (left, right) in lca.keys() {
            assert!(
                left <= right,
                "all_pairs LCA keys must be canonical ({left}, {right})"
            );
        }
        Ok(())
    }

    #[test]
    fn memory_impact_without_revision_chain_emits_dominance_sentinel() -> TestResult {
        let mut graph = empty_digraph();
        graph.add_node("mem_standalone");

        let report = graph_result(compute_memory_impact_analysis(&graph, "mem_standalone", 11))?;

        assert_eq!(report.schema, MEMORY_IMPACT_ANALYSIS_SCHEMA_V1);
        assert_eq!(report.memory_id, "mem_standalone");
        assert_eq!(report.snapshot_version, 11);
        assert!(report.revision_lineage.is_empty());
        assert_eq!(report.impact_analysis.validation_status, "unavailable");
        assert_eq!(report.impact_analysis.affected_memory_count, 0);
        assert!(report.frontiers.is_empty());
        assert_eq!(report.degraded.len(), 1);
        let degraded = &report.degraded[0];
        assert_eq!(degraded.code, GRAPH_DOMINANCE_NO_REVISION_CHAIN_CODE);
        assert_eq!(degraded.severity, "info");
        assert!(degraded.message.contains("revision chain"));
        assert!(degraded.message.contains("logical_id"));
        assert_eq!(degraded.repair, None);
        Ok(())
    }

    #[test]
    fn memory_impact_report_serializes_aggregated_degraded_entries() -> TestResult {
        let mut first = dominance_no_revision_chain_degradation("mem_standalone");
        first.message = "first missing-chain warning".to_owned();
        let mut second = dominance_no_revision_chain_degradation("mem_standalone");
        second.message = "second missing-chain warning".to_owned();

        let report = MemoryImpactAnalysisReport {
            schema: MEMORY_IMPACT_ANALYSIS_SCHEMA_V1,
            memory_id: "mem_standalone".to_owned(),
            snapshot_version: 11,
            revision_lineage: Vec::new(),
            impact_analysis: RevisionImpactAnalysis {
                immediate_dominator: None,
                dominance_frontier: Vec::new(),
                affected_memory_count: 0,
                validation_status: "unavailable".to_owned(),
            },
            frontiers: Vec::new(),
            degraded: vec![first, second],
        };

        let value = serde_json::to_value(report).map_err(|error| error.to_string())?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                "serialized memory impact report should include degraded array".to_owned()
            })?;

        assert_eq!(degraded.len(), 1);
        assert_eq!(
            degraded[0].get("code"),
            Some(&serde_json::json!(GRAPH_DOMINANCE_NO_REVISION_CHAIN_CODE))
        );
        assert_eq!(
            degraded[0].get("severity"),
            Some(&serde_json::json!("info"))
        );
        assert_eq!(
            degraded[0].get("repair"),
            Some(&serde_json::json!("Refresh graph dominance diagnostics."))
        );
        assert_eq!(
            degraded[0].get("sources"),
            Some(&serde_json::json!(["graph_dominance"]))
        );
        Ok(())
    }

    #[test]
    fn memory_impact_branch_reports_query_frontier_from_revision_root() -> TestResult {
        let mut graph = empty_digraph();
        add_edge(&mut graph, "root", "left");
        add_edge(&mut graph, "root", "right");
        add_edge(&mut graph, "left", "join");
        add_edge(&mut graph, "right", "join");

        let report = graph_result(compute_memory_impact_analysis(&graph, "left", 17))?;

        assert_eq!(report.schema, MEMORY_IMPACT_ANALYSIS_SCHEMA_V1);
        assert_eq!(report.memory_id, "left");
        assert_eq!(report.snapshot_version, 17);
        assert_eq!(
            report.impact_analysis.immediate_dominator.as_deref(),
            Some("root")
        );
        assert_eq!(report.impact_analysis.dominance_frontier, vec!["join"]);
        assert_eq!(report.impact_analysis.affected_memory_count, 1);
        assert_eq!(
            report
                .revision_lineage
                .iter()
                .map(|item| (item.memory_id.as_str(), item.depth, item.relation.as_str()))
                .collect::<Vec<_>>(),
            vec![("left", 0, "self"), ("root", 1, "ancestor")]
        );

        let left_frontier = report
            .frontiers
            .iter()
            .find(|item| item.memory_id == "left")
            .ok_or_else(|| "left frontier item should be present".to_string())?;
        assert_eq!(left_frontier.affected_memory_ids, vec!["join"]);

        Ok(())
    }

    #[test]
    fn dominance_wrappers_are_deterministic_across_three_runs() -> TestResult {
        // Slightly larger DAG so the determinism check is not trivial.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "root", "a");
        add_edge(&mut graph, "root", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "c");
        add_edge(&mut graph, "c", "leaf");
        add_edge(&mut graph, "a", "leaf");

        let first_idoms = graph_result(compute_immediate_dominators(&graph, "root"))?;
        let second_idoms = graph_result(compute_immediate_dominators(&graph, "root"))?;
        let third_idoms = graph_result(compute_immediate_dominators(&graph, "root"))?;
        assert_eq!(first_idoms, second_idoms);
        assert_eq!(second_idoms, third_idoms);

        let first_frontiers = graph_result(compute_dominance_frontiers(&graph, "root"))?;
        let second_frontiers = graph_result(compute_dominance_frontiers(&graph, "root"))?;
        assert_eq!(first_frontiers, second_frontiers);

        let first_lca = graph_result(compute_all_pairs_lca(&graph))?;
        let second_lca = graph_result(compute_all_pairs_lca(&graph))?;
        assert_eq!(first_lca, second_lca);

        Ok(())
    }
}
