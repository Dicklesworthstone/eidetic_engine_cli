use std::collections::{BTreeMap, BTreeSet, VecDeque};

use asupersync::Cx;
use fnx_algorithms::{find_cycle_directed, min_cost_flow, transitive_closure};
use fnx_runtime::CgseValue;
use serde::{Deserialize, Serialize};

use crate::graph::algorithms::{DEFAULT_BACKGROUND_BUDGET, current_or_testing_cx, run_with_budget};
use crate::graph::{GraphError, GraphResult};
use crate::models::degradation::GRAPH_CAUSAL_NO_EVIDENCE_CODE;
use crate::util::radix_ulid_sort::{
    compare_ulid_payload_or_lexical, sort_by_ulid_payload_or_lexical,
};

use super::{AttrMap, DiGraph};

const CONTRIBUTION_SCORE_ATTR: &str = "contribution_score";
const FLOW_DEMAND_ATTR: &str = "causal_demand";
const FLOW_CAPACITY_ATTR: &str = "causal_capacity";
const FLOW_WEIGHT_ATTR: &str = "causal_cost";
const FLOW_UNIT: f64 = 1.0;
const COST_EPSILON: f64 = 1.0e-9;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CausalAncestry {
    pub failure_id: String,
    pub ancestors: Vec<CausalAncestor>,
    pub degraded: Vec<CausalGraphDegradation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CausalAncestor {
    pub memory_id: String,
    pub path_length: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CausalGraphDegradation {
    pub code: String,
    pub severity: String,
    pub cycle_members: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinCostExplanation {
    pub failure_id: String,
    pub cause_id: String,
    pub total_cost: f64,
    pub path: Vec<CausalExplanationStep>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CausalExplanationStep {
    pub source: String,
    pub target: String,
    pub contribution_score: f64,
    pub cost: f64,
    pub evidence_count: Option<i64>,
    pub edge_id: Option<String>,
}

#[must_use]
pub fn compute_causal_ancestry(graph: &DiGraph, failure_id: &str) -> CausalAncestry {
    try_compute_causal_ancestry(graph, failure_id).unwrap_or_else(|error| CausalAncestry {
        failure_id: failure_id.to_owned(),
        ancestors: Vec::new(),
        degraded: vec![causal_algorithm_degradation(&error)],
    })
}

pub fn try_compute_causal_ancestry(
    graph: &DiGraph,
    failure_id: &str,
) -> GraphResult<CausalAncestry> {
    let cx = current_or_testing_cx();
    compute_causal_ancestry_with_cx(&cx, graph, failure_id)
}

pub fn compute_causal_ancestry_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    failure_id: &str,
) -> GraphResult<CausalAncestry> {
    let graph = graph.clone();
    let failure_id = failure_id.to_owned();
    run_with_budget(
        cx,
        "causal_ancestry",
        DEFAULT_BACKGROUND_BUDGET,
        move || compute_causal_ancestry_unbudgeted(&graph, &failure_id),
    )
}

#[must_use]
fn compute_causal_ancestry_unbudgeted(graph: &DiGraph, failure_id: &str) -> CausalAncestry {
    if !graph.has_node(failure_id) {
        return CausalAncestry {
            failure_id: failure_id.to_owned(),
            ancestors: Vec::new(),
            degraded: vec![causal_no_evidence_degradation()],
        };
    }

    let degraded = causal_degradations(graph, failure_id);
    let closure = transitive_closure(graph, Some(false));
    let path_lengths = shortest_path_lengths(graph, failure_id);
    let mut ancestors: Vec<_> = closure
        .successors(failure_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|ancestor| *ancestor != failure_id)
        .filter_map(|ancestor| {
            path_lengths
                .get(ancestor)
                .map(|path_length| CausalAncestor {
                    memory_id: ancestor.to_owned(),
                    path_length: *path_length,
                })
        })
        .collect();
    sort_by_ulid_payload_or_lexical(&mut ancestors, |ancestor| ancestor.memory_id.as_str());
    ancestors.sort_by_key(|ancestor| ancestor.path_length);

    CausalAncestry {
        failure_id: failure_id.to_owned(),
        ancestors,
        degraded,
    }
}

#[must_use]
pub fn compute_min_cost_explanation(
    graph: &DiGraph,
    failure_id: &str,
) -> Option<MinCostExplanation> {
    try_compute_min_cost_explanation(graph, failure_id)
        .ok()
        .flatten()
}

pub fn try_compute_min_cost_explanation(
    graph: &DiGraph,
    failure_id: &str,
) -> GraphResult<Option<MinCostExplanation>> {
    let cx = current_or_testing_cx();
    compute_min_cost_explanation_with_cx(&cx, graph, failure_id)
}

pub fn compute_min_cost_explanation_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    failure_id: &str,
) -> GraphResult<Option<MinCostExplanation>> {
    let graph = graph.clone();
    let failure_id = failure_id.to_owned();
    run_with_budget(
        cx,
        "causal_min_cost_explanation",
        DEFAULT_BACKGROUND_BUDGET,
        move || compute_min_cost_explanation_unbudgeted(&graph, &failure_id),
    )
}

#[must_use]
fn compute_min_cost_explanation_unbudgeted(
    graph: &DiGraph,
    failure_id: &str,
) -> Option<MinCostExplanation> {
    if !graph.has_node(failure_id) {
        return None;
    }
    if find_cycle_directed(graph).is_some() {
        return None;
    }

    let mut explanations: Vec<_> = terminal_ancestors(graph, failure_id)
        .into_iter()
        .filter_map(|candidate| flow_explanation_for_candidate(graph, failure_id, &candidate))
        .collect();
    sort_by_ulid_payload_or_lexical(&mut explanations, |explanation| {
        explanation.cause_id.as_str()
    });
    // Stable sort preserves radix cause-id order for equal-cost explanations.
    explanations.sort_by(compare_explanation_cost);
    explanations.into_iter().next()
}

fn causal_degradations(graph: &DiGraph, failure_id: &str) -> Vec<CausalGraphDegradation> {
    let mut degraded = Vec::new();
    if graph.successors(failure_id).unwrap_or_default().is_empty() {
        degraded.push(causal_no_evidence_degradation());
    }
    if let Some(cycle_members) = find_cycle_directed(graph) {
        degraded.push(CausalGraphDegradation {
            code: "graph.causal_cycle".to_owned(),
            severity: "warning".to_owned(),
            cycle_members,
        });
    }
    degraded
}

fn causal_no_evidence_degradation() -> CausalGraphDegradation {
    CausalGraphDegradation {
        code: GRAPH_CAUSAL_NO_EVIDENCE_CODE.to_owned(),
        severity: "low".to_owned(),
        cycle_members: Vec::new(),
    }
}

fn causal_algorithm_degradation(error: &GraphError) -> CausalGraphDegradation {
    CausalGraphDegradation {
        code: error.kind_str().to_owned(),
        severity: "warning".to_owned(),
        cycle_members: Vec::new(),
    }
}

fn compare_explanation_cost(
    left: &MinCostExplanation,
    right: &MinCostExplanation,
) -> std::cmp::Ordering {
    // `total_cmp` over `partial_cmp(...).unwrap_or(Equal)` for two
    // reasons:
    //
    // 1. Determinism with NaN. `partial_cmp(NaN, x)` returns `None` and
    //    `unwrap_or(Equal)` collapses every NaN-vs-finite pair onto the
    //    same equivalence class. Without an in-comparator tiebreaker
    //    (this function has none — the only sort key is total_cost),
    //    the result violates transitivity (NaN == 1.0, NaN == 2.0,
    //    but 1.0 < 2.0). Rust's `sort_by` documents the order as
    //    "unspecified" when the comparator is not a total order. The
    //    caller at line 187 (`explanations.sort_by(compare_explanation_cost)`)
    //    relies on stable order across runs — `ee why <id> --causal-explain`
    //    is part of the deterministic context-pack surface — so an
    //    unspecified sort here would silently scramble the emitted
    //    `causalExplanation` block across invocations.
    //
    // 2. Live NaN risk. `total_cost` comes from `min_cost_flow.cost`
    //    (line 278). `compute_min_cost_explanation_unbudgeted` clamps
    //    edge weights via `contribution_score`/`causal_cost` (lines
    //    390-404) so the production path is currently NaN-free, but
    //    `min_cost_flow` is an upstream `fnx_algorithms` routine and
    //    a future change to its summation order, an unclamped
    //    contribution_score, or a malformed graph edge could let NaN
    //    leak into the cost field without breaking any compile-time
    //    check.
    //
    // Same defense-in-depth pattern that `src/core/conformal.rs:158`,
    // `src/core/focus_suggest.rs:569`, `src/core/plan.rs:1355`, and
    // `src/core/situation.rs:1268` migrated to. The pre-sort by
    // `sort_by_ulid_payload_or_lexical` at line 183-185 still
    // provides the cause_id-order tiebreak for ordinary equal-cost
    // explanations via Rust's stable `sort_by`; repeat that tiebreak
    // explicitly here because `total_cmp` orders distinct NaN payloads,
    // and those payload bits are not a useful public contract.
    left.total_cost
        .total_cmp(&right.total_cost)
        .then_with(|| compare_ulid_payload_or_lexical(&left.cause_id, &right.cause_id))
}

fn terminal_ancestors(graph: &DiGraph, failure_id: &str) -> Vec<String> {
    let ancestry = compute_causal_ancestry(graph, failure_id);
    let reachable: BTreeSet<_> = ancestry
        .ancestors
        .iter()
        .map(|ancestor| ancestor.memory_id.clone())
        .collect();
    ancestry
        .ancestors
        .into_iter()
        .filter(|ancestor| {
            graph
                .successors(&ancestor.memory_id)
                .unwrap_or_default()
                .into_iter()
                .all(|successor| !reachable.contains(successor))
        })
        .map(|ancestor| ancestor.memory_id)
        .collect()
}

fn flow_explanation_for_candidate(
    graph: &DiGraph,
    failure_id: &str,
    candidate: &str,
) -> Option<MinCostExplanation> {
    let flow_graph = build_unit_flow_graph(graph, failure_id, candidate)?;
    let flow = min_cost_flow(
        &flow_graph,
        FLOW_DEMAND_ATTR,
        FLOW_CAPACITY_ATTR,
        FLOW_WEIGHT_ATTR,
    )?;
    let flow_edges = flow
        .flow
        .into_iter()
        .filter(|(_, flow)| *flow > COST_EPSILON)
        .collect();
    let path = reconstruct_flow_path(graph, failure_id, candidate, flow_edges)?;
    let path_cost: f64 = path.iter().map(|step| step.cost).sum();
    if (path_cost - flow.cost).abs() > COST_EPSILON {
        return None;
    }

    Some(MinCostExplanation {
        failure_id: failure_id.to_owned(),
        cause_id: candidate.to_owned(),
        total_cost: flow.cost,
        path,
    })
}

fn build_unit_flow_graph(graph: &DiGraph, source: &str, target: &str) -> Option<DiGraph> {
    let mut flow_graph = DiGraph::with_runtime_policy(graph.runtime_policy().clone());
    for node in graph.nodes_ordered() {
        let mut attrs = graph.node_attrs(node).cloned().unwrap_or_default();
        let demand = if node == source {
            -FLOW_UNIT
        } else if node == target {
            FLOW_UNIT
        } else {
            0.0
        };
        attrs.insert(FLOW_DEMAND_ATTR.to_owned(), CgseValue::Float(demand));
        flow_graph.add_node_with_attrs(node.to_owned(), attrs);
    }

    for edge in graph.edges_ordered() {
        let mut attrs = edge.attrs;
        attrs.insert(FLOW_CAPACITY_ATTR.to_owned(), CgseValue::Float(FLOW_UNIT));
        attrs.insert(
            FLOW_WEIGHT_ATTR.to_owned(),
            CgseValue::Float(edge_cost(&attrs)),
        );
        flow_graph
            .add_edge_with_attrs(edge.left, edge.right, attrs)
            .ok()?;
    }

    Some(flow_graph)
}

fn reconstruct_flow_path(
    graph: &DiGraph,
    source: &str,
    target: &str,
    flow_edges: BTreeMap<(String, String), f64>,
) -> Option<Vec<CausalExplanationStep>> {
    let mut path = Vec::new();
    let mut current = source.to_owned();
    let mut visited = BTreeSet::new();
    visited.insert(current.clone());

    while current != target {
        let mut next_candidates: Vec<_> = flow_edges
            .keys()
            .filter(|(edge_source, _)| edge_source == &current)
            .map(|(_, edge_target)| edge_target)
            .collect();
        sort_by_ulid_payload_or_lexical(&mut next_candidates, |candidate| candidate.as_str());
        let next = next_candidates.into_iter().next()?.clone();
        if !visited.insert(next.clone()) {
            return None;
        }

        path.push(explanation_step(graph, &current, &next)?);
        current = next;
    }

    Some(path)
}

fn explanation_step(graph: &DiGraph, source: &str, target: &str) -> Option<CausalExplanationStep> {
    let attrs = graph.edge_attrs(source, target)?;
    let contribution_score = contribution_score(attrs);
    Some(CausalExplanationStep {
        source: source.to_owned(),
        target: target.to_owned(),
        contribution_score,
        cost: causal_cost(contribution_score),
        evidence_count: attrs
            .get("evidence_count")
            .and_then(CgseValue::as_f64)
            .map(|value| {
                if value.is_sign_negative() {
                    0
                } else {
                    value.trunc() as i64
                }
            }),
        edge_id: attrs.get("edge_id").map(CgseValue::as_str),
    })
}

fn shortest_path_lengths(graph: &DiGraph, source: &str) -> BTreeMap<String, usize> {
    let mut lengths: BTreeMap<String, usize> = BTreeMap::new();
    let mut queue = VecDeque::new();
    lengths.insert(source.to_owned(), 0_usize);
    queue.push_back(source.to_owned());

    while let Some(current) = queue.pop_front() {
        let next_length = lengths[&current].saturating_add(1);
        let mut successors: Vec<_> = graph.successors(&current).unwrap_or_default();
        sort_by_ulid_payload_or_lexical(&mut successors, |successor| *successor);
        for successor in successors {
            if !lengths.contains_key(successor) {
                lengths.insert(successor.to_owned(), next_length);
                queue.push_back(successor.to_owned());
            }
        }
    }

    lengths
}

fn edge_cost(attrs: &AttrMap) -> f64 {
    causal_cost(contribution_score(attrs))
}

fn contribution_score(attrs: &AttrMap) -> f64 {
    let score = attrs
        .get(CONTRIBUTION_SCORE_ATTR)
        .and_then(CgseValue::as_f64)
        .unwrap_or(0.0);
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn causal_cost(contribution_score: f64) -> f64 {
    1.0 - contribution_score.clamp(0.0, 1.0)
}

/// Configuration for Causal-Ancestry PPR pre-warming (bd-1n0np.19.1): caps and
/// the upstream weight decay used when turning query task/bead IDs into a
/// Personalized PageRank seed map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CausalPprSeedConfig {
    /// Maximum query seed ids to expand (the rest are ignored).
    pub max_seeds: usize,
    /// Maximum backward-causal ancestors to boost per seed.
    pub max_ancestors_per_seed: usize,
    /// Seed weight for each query id in the PPR personalization vector.
    pub seed_weight: f64,
    /// Per-hop decay applied to an ancestor's weight (`decay^path_length`), so
    /// nearer upstream tasks are boosted more than distant ones.
    pub ancestor_decay: f64,
}

impl Default for CausalPprSeedConfig {
    fn default() -> Self {
        Self {
            max_seeds: 8,
            max_ancestors_per_seed: 16,
            seed_weight: 1.0,
            ancestor_decay: 0.5,
        }
    }
}

/// Extract bead/task ids (`bd-...`) from a free-text query (bd-1n0np.19.1).
///
/// Pure and deterministic: scans for `bd-` prefixed tokens (alphanumeric + `.`),
/// trims trailing dots, deduplicates, and returns them sorted. These seed the
/// Causal-Ancestry PPR pre-warming so a query that names upstream tasks pulls in
/// their hard-won lessons.
#[must_use]
pub fn extract_causal_seed_ids(query: &str) -> Vec<String> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for (start, _) in query.match_indices("bd-") {
        let rest = &query[start..];
        let end = rest
            .char_indices()
            .skip(3)
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '.'))
            .map_or(rest.len(), |(idx, _)| idx);
        let id = rest[..end].trim_end_matches('.');
        if id.len() > 3 {
            ids.insert(id.to_owned());
        }
    }
    ids.into_iter().collect()
}

/// Build a Personalized PageRank seed map from query bead ids by walking BACKWARD
/// along causal-ancestry edges (bd-1n0np.19.1), reusing
/// [`compute_causal_ancestry`]. Each query seed gets `seed_weight`; each upstream
/// ancestor gets `ancestor_decay^path_length` so nearer lessons dominate. Caps
/// bound the work; an empty `query_seed_ids` is a graceful no-op (empty map).
/// Deterministic — the result feeds `graph::ppr::personalized_pagerank`.
#[must_use]
pub fn causal_ancestry_ppr_seed_map(
    graph: &DiGraph,
    query_seed_ids: &[String],
    config: CausalPprSeedConfig,
) -> BTreeMap<String, f64> {
    let mut seed_map: BTreeMap<String, f64> = BTreeMap::new();
    for seed in query_seed_ids.iter().take(config.max_seeds) {
        *seed_map.entry(seed.clone()).or_insert(0.0) += config.seed_weight;
        let ancestry = compute_causal_ancestry(graph, seed);
        for ancestor in ancestry
            .ancestors
            .iter()
            .take(config.max_ancestors_per_seed)
        {
            let exponent = i32::try_from(ancestor.path_length).unwrap_or(i32::MAX);
            let weight = config.ancestor_decay.powi(exponent);
            *seed_map.entry(ancestor.memory_id.clone()).or_insert(0.0) += weight;
        }
    }
    seed_map
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_runtime::CompatibilityMode;

    type TestResult = Result<(), String>;

    fn graph() -> DiGraph {
        DiGraph::new(CompatibilityMode::Strict)
    }

    fn add_causal_edge(graph: &mut DiGraph, source: &str, target: &str, contribution_score: f64) {
        let mut attrs = AttrMap::new();
        attrs.insert(
            CONTRIBUTION_SCORE_ATTR.to_owned(),
            CgseValue::Float(contribution_score),
        );
        attrs.insert("evidence_count".to_owned(), CgseValue::Int(2));
        attrs.insert(
            "edge_id".to_owned(),
            CgseValue::String(format!("{source}->{target}")),
        );
        if let Err(error) = graph.add_edge_with_attrs(source, target, attrs) {
            panic!("test causal edge should be valid: {error}");
        }
    }

    fn require_min_cost_explanation(graph: &DiGraph, failure_id: &str) -> MinCostExplanation {
        match compute_min_cost_explanation(graph, failure_id) {
            Some(explanation) => explanation,
            None => panic!("expected min-cost explanation for {failure_id}"),
        }
    }

    fn path_pairs(explanation: &MinCostExplanation) -> Vec<(&str, &str)> {
        explanation
            .path
            .iter()
            .map(|step| (step.source.as_str(), step.target.as_str()))
            .collect()
    }

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    #[test]
    fn causal_budget_wrappers_preserve_existing_outputs() -> TestResult {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "mid", 0.8);
        add_causal_edge(&mut graph, "mid", "root", 0.9);

        let cx = Cx::for_testing();
        let ancestry = graph_result(compute_causal_ancestry_with_cx(&cx, &graph, "failure"))?;
        let explanation =
            graph_result(compute_min_cost_explanation_with_cx(&cx, &graph, "failure"))?
                .ok_or_else(|| "expected min-cost explanation through budget wrapper".to_owned())?;

        assert_eq!(
            ancestry.ancestors,
            vec![
                CausalAncestor {
                    memory_id: "mid".to_owned(),
                    path_length: 1,
                },
                CausalAncestor {
                    memory_id: "root".to_owned(),
                    path_length: 2,
                },
            ]
        );
        assert!(ancestry.degraded.is_empty());
        assert_eq!(explanation.cause_id, "root");
        assert_eq!(
            path_pairs(&explanation),
            vec![("failure", "mid"), ("mid", "root")]
        );
        Ok(())
    }

    #[test]
    fn causal_ancestry_empty_graph_is_empty() {
        let graph = graph();

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert_eq!(ancestry.failure_id, "failure");
        assert!(ancestry.ancestors.is_empty());
        assert_eq!(ancestry.degraded.len(), 1);
        assert_eq!(ancestry.degraded[0].code, GRAPH_CAUSAL_NO_EVIDENCE_CODE);
        assert_eq!(ancestry.degraded[0].severity, "low");
        assert!(ancestry.degraded[0].cycle_members.is_empty());
    }

    #[test]
    fn causal_ancestry_node_without_causal_edges_reports_no_evidence() {
        let mut graph = graph();
        graph.add_node("failure");

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert!(ancestry.ancestors.is_empty());
        assert_eq!(ancestry.degraded.len(), 1);
        assert_eq!(ancestry.degraded[0].code, GRAPH_CAUSAL_NO_EVIDENCE_CODE);
        assert_eq!(ancestry.degraded[0].severity, "low");
        assert!(ancestry.degraded[0].cycle_members.is_empty());
    }

    #[test]
    fn causal_ancestry_single_edge_returns_direct_cause() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause", 0.75);

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert_eq!(
            ancestry,
            CausalAncestry {
                failure_id: "failure".to_owned(),
                ancestors: vec![CausalAncestor {
                    memory_id: "cause".to_owned(),
                    path_length: 1,
                }],
                degraded: Vec::new(),
            }
        );
    }

    #[test]
    fn causal_ancestry_multi_hop_returns_transitive_causes() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause_a", 0.8);
        add_causal_edge(&mut graph, "cause_a", "root", 0.7);
        add_causal_edge(&mut graph, "failure", "cause_b", 0.6);

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert_eq!(
            ancestry,
            CausalAncestry {
                failure_id: "failure".to_owned(),
                ancestors: vec![
                    CausalAncestor {
                        memory_id: "cause_a".to_owned(),
                        path_length: 1,
                    },
                    CausalAncestor {
                        memory_id: "cause_b".to_owned(),
                        path_length: 1,
                    },
                    CausalAncestor {
                        memory_id: "root".to_owned(),
                        path_length: 2,
                    },
                ],
                degraded: Vec::new(),
            }
        );
    }

    #[test]
    fn causal_ancestry_same_depth_ties_accept_radix_public_ids() {
        let mut graph = graph();
        add_causal_edge(
            &mut graph,
            "failure",
            "rule_01J0000000000000000000000C",
            0.8,
        );
        add_causal_edge(
            &mut graph,
            "failure",
            "note_01J0000000000000000000000A",
            0.8,
        );
        add_causal_edge(&mut graph, "failure", "mem_01J0000000000000000000000B", 0.8);

        let ancestry = compute_causal_ancestry(&graph, "failure");
        let ids = ancestry
            .ancestors
            .iter()
            .map(|ancestor| ancestor.memory_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            vec![
                "note_01J0000000000000000000000A",
                "mem_01J0000000000000000000000B",
                "rule_01J0000000000000000000000C",
            ]
        );
    }

    #[test]
    fn causal_ancestry_diamond_deduplicates_shared_root() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "left", 0.8);
        add_causal_edge(&mut graph, "failure", "right", 0.7);
        add_causal_edge(&mut graph, "left", "root", 0.9);
        add_causal_edge(&mut graph, "right", "root", 0.6);

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert_eq!(
            ancestry.ancestors,
            vec![
                CausalAncestor {
                    memory_id: "left".to_owned(),
                    path_length: 1,
                },
                CausalAncestor {
                    memory_id: "right".to_owned(),
                    path_length: 1,
                },
                CausalAncestor {
                    memory_id: "root".to_owned(),
                    path_length: 2,
                },
            ]
        );
        assert!(ancestry.degraded.is_empty());
    }

    #[test]
    fn causal_cycle_is_reported_and_blocks_min_cost_flow() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause", 0.8);
        add_causal_edge(&mut graph, "cause", "failure", 0.7);

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert_eq!(ancestry.degraded.len(), 1);
        assert_eq!(ancestry.degraded[0].code, "graph.causal_cycle");
        assert_eq!(ancestry.degraded[0].severity, "warning");
        assert_eq!(
            ancestry.degraded[0]
                .cycle_members
                .first()
                .map(String::as_str),
            Some("failure")
        );
        assert_eq!(
            ancestry.degraded[0]
                .cycle_members
                .last()
                .map(String::as_str),
            Some("failure")
        );
        assert!(
            ancestry.degraded[0]
                .cycle_members
                .iter()
                .any(|node| node == "cause")
        );
        assert!(compute_min_cost_explanation(&graph, "failure").is_none());
    }

    #[test]
    fn min_cost_explanation_single_edge_returns_direct_path() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause", 0.8);

        let explanation = require_min_cost_explanation(&graph, "failure");

        assert_eq!(explanation.cause_id, "cause");
        assert!((explanation.total_cost - 0.2).abs() < COST_EPSILON);
        assert_eq!(path_pairs(&explanation), vec![("failure", "cause")]);
        assert_eq!(explanation.path[0].evidence_count, Some(2));
        assert_eq!(
            explanation.path[0].edge_id.as_deref(),
            Some("failure->cause")
        );
    }

    #[test]
    fn min_cost_explanation_picks_high_confidence_path() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "noisy_direct", 0.1);
        add_causal_edge(&mut graph, "failure", "credible_mid", 0.95);
        add_causal_edge(&mut graph, "credible_mid", "root_cause", 0.95);

        let explanation = require_min_cost_explanation(&graph, "failure");

        assert_eq!(explanation.cause_id, "root_cause");
        assert!((explanation.total_cost - 0.1).abs() < COST_EPSILON);
        assert_eq!(
            path_pairs(&explanation),
            vec![("failure", "credible_mid"), ("credible_mid", "root_cause")]
        );
    }

    #[test]
    fn min_cost_explanation_equal_cost_tiebreaks_by_radix_cause_id() {
        let mut graph = graph();
        add_causal_edge(
            &mut graph,
            "failure",
            "rule_01J0000000000000000000000C",
            0.8,
        );
        add_causal_edge(&mut graph, "failure", "mem_01J0000000000000000000000B", 0.8);
        add_causal_edge(
            &mut graph,
            "failure",
            "note_01J0000000000000000000000A",
            0.8,
        );

        let explanation = require_min_cost_explanation(&graph, "failure");

        assert_eq!(explanation.cause_id, "note_01J0000000000000000000000A");
        assert_eq!(
            path_pairs(&explanation),
            vec![("failure", "note_01J0000000000000000000000A")]
        );
    }

    #[test]
    fn flow_path_reconstruction_uses_radix_next_edge_ties() {
        let mut graph = graph();
        add_causal_edge(
            &mut graph,
            "failure",
            "note_01J0000000000000000000000A",
            0.8,
        );
        add_causal_edge(&mut graph, "failure", "mem_01J0000000000000000000000B", 0.8);
        let mut flow_edges = BTreeMap::new();
        flow_edges.insert(
            (
                "failure".to_owned(),
                "mem_01J0000000000000000000000B".to_owned(),
            ),
            FLOW_UNIT,
        );
        flow_edges.insert(
            (
                "failure".to_owned(),
                "note_01J0000000000000000000000A".to_owned(),
            ),
            FLOW_UNIT,
        );

        let path = reconstruct_flow_path(
            &graph,
            "failure",
            "note_01J0000000000000000000000A",
            flow_edges,
        )
        .expect("radix-ordered next edge should reach target");
        let pairs = path
            .iter()
            .map(|step| (step.source.as_str(), step.target.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(pairs, vec![("failure", "note_01J0000000000000000000000A")]);
    }

    #[test]
    fn min_cost_explanation_respects_dag_acyclic_path() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "left", 0.85);
        add_causal_edge(&mut graph, "failure", "right", 0.65);
        add_causal_edge(&mut graph, "left", "root", 0.85);
        add_causal_edge(&mut graph, "right", "root", 0.99);

        let explanation = require_min_cost_explanation(&graph, "failure");

        assert_eq!(explanation.cause_id, "root");
        assert_eq!(
            path_pairs(&explanation),
            vec![("failure", "left"), ("left", "root")]
        );
    }

    #[test]
    fn min_cost_explanation_non_failure_target_returns_none() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause", 0.8);

        assert!(compute_min_cost_explanation(&graph, "cause").is_none());
    }

    /// Regression guard for the NaN-determinism defense in
    /// `compare_explanation_cost`. Before the migration to `total_cmp`,
    /// the comparator used `partial_cmp(...).unwrap_or(Ordering::Equal)`
    /// which collapses every NaN-vs-finite pair onto the same
    /// equivalence class. With no in-comparator tiebreaker (this
    /// comparator has none — `total_cost` is the only key), that
    /// violates transitivity (NaN == 1.0, NaN == 2.0, but 1.0 < 2.0)
    /// and Rust's `sort_by` documents the resulting order as
    /// "unspecified". `total_cmp` is a total order on f64 (positive
    /// NaN sorts above +Inf, negative NaN sorts below -Inf, with
    /// payloads ordered lexicographically), so the result is stable
    /// across runs regardless of NaN inputs.
    ///
    /// Production `total_cost` comes from `min_cost_flow.cost` and is
    /// currently NaN-free because `contribution_score` clamps to
    /// [0.0, 1.0] and `causal_cost = 1.0 - clamped` is in [0.0, 1.0].
    /// This test exercises the comparator directly with synthetic
    /// inputs to lock the post-fix behavior against a future change
    /// that lets NaN reach the cost field (e.g., an unclamped
    /// contribution score or a refactor of fnx_algorithms' summation
    /// order).
    #[test]
    fn compare_explanation_cost_is_total_under_nan() {
        fn make(cause: &str, cost: f64) -> MinCostExplanation {
            MinCostExplanation {
                failure_id: "failure".to_owned(),
                cause_id: cause.to_owned(),
                total_cost: cost,
                path: Vec::new(),
            }
        }

        // Transitivity probe: NaN, 1.0, 2.0 must produce a consistent
        // ordering. `partial_cmp(...).unwrap_or(Equal)` would return
        // Equal for both NaN comparisons and Less for 1.0-vs-2.0,
        // which is non-transitive (NaN == 1.0, NaN == 2.0, but
        // 1.0 < 2.0). `total_cmp` returns a deterministic Greater for
        // positive NaN-vs-finite (positive NaN sorts above +Inf).
        let nan = make("nan_cause", f64::NAN);
        let one = make("one_cause", 1.0);
        let two = make("two_cause", 2.0);

        let nan_vs_one = compare_explanation_cost(&nan, &one);
        let one_vs_two = compare_explanation_cost(&one, &two);
        let nan_vs_two = compare_explanation_cost(&nan, &two);

        // 1.0 < 2.0 must always hold (the only finite-vs-finite comparison).
        assert_eq!(one_vs_two, std::cmp::Ordering::Less);
        // The comparator MUST give consistent (non-Equal) answers for
        // NaN-vs-finite so that the transitive chain holds. `total_cmp`
        // returns Greater for positive NaN above finite values; either
        // direction is acceptable as long as it's consistent across
        // both NaN-vs-finite pairs.
        assert_ne!(
            nan_vs_one,
            std::cmp::Ordering::Equal,
            "NaN-vs-finite must not collapse to Equal under total_cmp"
        );
        assert_eq!(
            nan_vs_one, nan_vs_two,
            "NaN must compare consistently against all finite values"
        );

        // End-to-end sort lock: shuffled inputs must produce the same
        // byte-identical order after `sort_by(compare_explanation_cost)`.
        // This is the actual call-site invariant at line 187.
        let mut order_a = [
            make("nan_a", f64::NAN),
            make("two_b", 2.0),
            make("one_c", 1.0),
            make("nan_d", f64::NAN),
        ];
        let mut order_b = [
            make("two_b", 2.0),
            make("nan_d", f64::NAN),
            make("one_c", 1.0),
            make("nan_a", f64::NAN),
        ];
        order_a.sort_by(compare_explanation_cost);
        order_b.sort_by(compare_explanation_cost);
        let cause_ids_a: Vec<_> = order_a.iter().map(|e| e.cause_id.clone()).collect();
        let cause_ids_b: Vec<_> = order_b.iter().map(|e| e.cause_id.clone()).collect();
        assert_eq!(
            cause_ids_a, cause_ids_b,
            "compare_explanation_cost must sort to the same order regardless of input permutation, even with NaN present"
        );
    }

    #[test]
    fn extract_causal_seed_ids_parses_dedups_and_sorts() {
        let query = "continue bd-1n0np.19.1 after bd-17c65.10.17 broke it; see bd-1n0np.19.1.";
        assert_eq!(
            extract_causal_seed_ids(query),
            vec!["bd-17c65.10.17".to_owned(), "bd-1n0np.19.1".to_owned()],
            "bead ids are parsed, trailing dot trimmed, deduped, and sorted"
        );
        assert!(
            extract_causal_seed_ids("no ids in this query").is_empty(),
            "a query with no bead ids yields no seeds"
        );
    }

    #[test]
    fn causal_ppr_seed_map_seeds_query_and_boosts_decaying_ancestors() {
        // failure -> cause_a -> root: ancestry of `failure` is cause_a (1), root (2).
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause_a", 0.8);
        add_causal_edge(&mut graph, "cause_a", "root", 0.7);

        let seed_map = causal_ancestry_ppr_seed_map(
            &graph,
            &["failure".to_owned()],
            CausalPprSeedConfig::default(),
        );
        let weight = |id: &str| *seed_map.get(id).unwrap_or(&-1.0);
        assert!((weight("failure") - 1.0).abs() < 1e-9, "seed weight is 1.0");
        assert!(
            (weight("cause_a") - 0.5).abs() < 1e-9,
            "direct ancestor decays by 0.5^1"
        );
        assert!(
            (weight("root") - 0.25).abs() < 1e-9,
            "transitive ancestor decays by 0.5^2"
        );
    }

    #[test]
    fn causal_ppr_seed_map_is_graceful_no_op_on_empty_query() {
        let graph = graph();
        let seed_map = causal_ancestry_ppr_seed_map(&graph, &[], CausalPprSeedConfig::default());
        assert!(seed_map.is_empty(), "no query seeds is a graceful no-op");
    }

    #[test]
    fn causal_ppr_seed_map_respects_caps() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause_a", 0.8);
        add_causal_edge(&mut graph, "cause_a", "root", 0.7);

        // Cap ancestors to 1: only the nearest ancestor is boosted, the seed stays.
        let capped = causal_ancestry_ppr_seed_map(
            &graph,
            &["failure".to_owned()],
            CausalPprSeedConfig {
                max_ancestors_per_seed: 1,
                ..CausalPprSeedConfig::default()
            },
        );
        assert!(capped.contains_key("failure"));
        assert!(
            capped.len() <= 2,
            "max_ancestors_per_seed=1 keeps the seed + at most one ancestor"
        );

        // Cap seeds to 0: nothing is expanded.
        let none = causal_ancestry_ppr_seed_map(
            &graph,
            &["failure".to_owned()],
            CausalPprSeedConfig {
                max_seeds: 0,
                ..CausalPprSeedConfig::default()
            },
        );
        assert!(none.is_empty(), "max_seeds=0 expands nothing");
    }
}
