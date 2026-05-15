use std::collections::{BTreeMap, BTreeSet, VecDeque};

use fnx_algorithms::{min_cost_flow, transitive_closure};
use fnx_runtime::CgseValue;
use serde::{Deserialize, Serialize};

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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CausalAncestor {
    pub memory_id: String,
    pub path_length: usize,
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
    if !graph.has_node(failure_id) {
        return CausalAncestry {
            failure_id: failure_id.to_owned(),
            ancestors: Vec::new(),
        };
    }

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
    ancestors.sort_by(|left, right| {
        left.path_length
            .cmp(&right.path_length)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    CausalAncestry {
        failure_id: failure_id.to_owned(),
        ancestors,
    }
}

#[must_use]
pub fn compute_min_cost_explanation(
    graph: &DiGraph,
    failure_id: &str,
) -> Option<MinCostExplanation> {
    if !graph.has_node(failure_id) {
        return None;
    }

    terminal_ancestors(graph, failure_id)
        .into_iter()
        .filter_map(|candidate| flow_explanation_for_candidate(graph, failure_id, &candidate))
        .min_by(compare_explanations)
}

fn compare_explanations(
    left: &MinCostExplanation,
    right: &MinCostExplanation,
) -> std::cmp::Ordering {
    left.total_cost
        .partial_cmp(&right.total_cost)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| left.cause_id.cmp(&right.cause_id))
        .then_with(|| explanation_path_key(left).cmp(&explanation_path_key(right)))
}

fn explanation_path_key(explanation: &MinCostExplanation) -> Vec<(&str, &str)> {
    explanation
        .path
        .iter()
        .map(|step| (step.source.as_str(), step.target.as_str()))
        .collect()
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
        let next = flow_edges
            .keys()
            .filter(|(edge_source, _)| edge_source == &current)
            .map(|(_, edge_target)| edge_target)
            .min()?
            .clone();
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
    let mut lengths = BTreeMap::new();
    let mut queue = VecDeque::new();
    lengths.insert(source.to_owned(), 0);
    queue.push_back(source.to_owned());

    while let Some(current) = queue.pop_front() {
        let next_length = lengths[&current].saturating_add(1);
        let mut successors: Vec<_> = graph.successors(&current).unwrap_or_default();
        successors.sort_unstable();
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

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_runtime::CompatibilityMode;

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
        graph.add_edge_with_attrs(source, target, attrs).unwrap();
    }

    fn path_pairs(explanation: &MinCostExplanation) -> Vec<(&str, &str)> {
        explanation
            .path
            .iter()
            .map(|step| (step.source.as_str(), step.target.as_str()))
            .collect()
    }

    #[test]
    fn causal_ancestry_empty_graph_is_empty() {
        let graph = graph();

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert_eq!(ancestry.failure_id, "failure");
        assert!(ancestry.ancestors.is_empty());
    }

    #[test]
    fn causal_ancestry_single_edge_returns_direct_cause() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause", 0.75);

        let ancestry = compute_causal_ancestry(&graph, "failure");

        assert_eq!(
            ancestry.ancestors,
            vec![CausalAncestor {
                memory_id: "cause".to_owned(),
                path_length: 1,
            }]
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
            ancestry.ancestors,
            vec![
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
            ]
        );
    }

    #[test]
    fn min_cost_explanation_single_edge_returns_direct_path() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "cause", 0.8);

        let explanation = compute_min_cost_explanation(&graph, "failure").unwrap();

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

        let explanation = compute_min_cost_explanation(&graph, "failure").unwrap();

        assert_eq!(explanation.cause_id, "root_cause");
        assert!((explanation.total_cost - 0.1).abs() < COST_EPSILON);
        assert_eq!(
            path_pairs(&explanation),
            vec![("failure", "credible_mid"), ("credible_mid", "root_cause")]
        );
    }

    #[test]
    fn min_cost_explanation_respects_dag_acyclic_path() {
        let mut graph = graph();
        add_causal_edge(&mut graph, "failure", "left", 0.85);
        add_causal_edge(&mut graph, "failure", "right", 0.65);
        add_causal_edge(&mut graph, "left", "root", 0.85);
        add_causal_edge(&mut graph, "right", "root", 0.99);

        let explanation = compute_min_cost_explanation(&graph, "failure").unwrap();

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
}
