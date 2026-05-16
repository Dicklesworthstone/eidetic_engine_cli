use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

use asupersync::Cx;
use fnx_classes::Graph;
use serde::{Serialize, Serializer};

use crate::core::degraded_aggregation::{
    AggregatedDegradation, DegradationAggregationInput, aggregate_degraded_entries,
};
use crate::graph::GraphResult;
use crate::graph::algorithms::{current_or_testing_cx, run_with_budget};
use crate::models::degradation::GRAPH_PROXIMITY_UNREACHABLE_CODE;

pub const PROXIMITY_SCHEMA_V1: &str = "ee.proximity.v1";
pub const GOMORY_HU_WEIGHT_ATTR: &str = "weight";
const GOMORY_HU_BUILD_BUDGET: Duration = Duration::from_millis(10_000);

#[derive(Clone, Debug)]
pub struct GomoryHuTree {
    pub tree: Graph,
    query_index: TreeMinCutIndex,
    component_by_node: HashMap<String, usize>,
}

#[derive(Clone, Debug)]
struct TreeMinCutIndex {
    node_index: HashMap<String, usize>,
    component_by_index: Vec<usize>,
    depth_by_index: Vec<usize>,
    ancestor_by_power: Vec<Vec<usize>>,
    min_cut_by_power: Vec<Vec<f64>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProximityReport {
    pub schema: &'static str,
    pub memory_a: String,
    pub memory_b: String,
    pub snapshot_version: u64,
    pub min_cut: Option<f64>,
    pub interpretation: String,
    pub tree_path: Option<Vec<String>>,
    #[serde(serialize_with = "serialize_proximity_degraded")]
    pub degraded: Vec<ProximityDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProximityDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

fn serialize_proximity_degraded<S>(
    degraded: &[ProximityDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_proximity_degraded(degraded).serialize(serializer)
}

fn aggregate_proximity_degraded(degraded: &[ProximityDegradation]) -> Vec<AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "gomory_hu_proximity",
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry
                .repair
                .clone()
                .unwrap_or_else(|| "Refresh graph proximity diagnostics.".to_owned()),
        )
    }))
}

pub fn build_gomory_hu_tree(graph: &Graph) -> GraphResult<GomoryHuTree> {
    let cx = current_or_testing_cx();
    build_gomory_hu_tree_with_cx(&cx, graph)
}

pub fn build_gomory_hu_tree_with_cx(cx: &Cx, graph: &Graph) -> GraphResult<GomoryHuTree> {
    let graph = graph.clone();
    run_with_budget(cx, "gomory_hu_tree", GOMORY_HU_BUILD_BUDGET, move || {
        let tree = fnx_algorithms::gomory_hu_tree(&graph, GOMORY_HU_WEIGHT_ATTR);
        let query_index = TreeMinCutIndex::new(&tree);
        let component_by_node = graph_components(&graph);
        GomoryHuTree {
            tree,
            query_index,
            component_by_node,
        }
    })
}

#[must_use]
pub fn query_min_cut(tree: &GomoryHuTree, left: &str, right: &str) -> Option<f64> {
    if !tree.tree.has_node(left) || !tree.tree.has_node(right) {
        return None;
    }
    if left == right {
        return Some(0.0);
    }
    tree.query_index.query_min_cut(left, right)
}

#[must_use]
pub fn query_proximity(
    tree: &GomoryHuTree,
    left: &str,
    right: &str,
    snapshot_version: u64,
) -> ProximityReport {
    if !tree.tree.has_node(left) || !tree.tree.has_node(right) {
        return ProximityReport {
            schema: PROXIMITY_SCHEMA_V1,
            memory_a: left.to_owned(),
            memory_b: right.to_owned(),
            snapshot_version,
            min_cut: None,
            interpretation: "missing_memory".to_owned(),
            tree_path: None,
            degraded: Vec::new(),
        };
    }

    if !same_original_component(tree, left, right) {
        return ProximityReport {
            schema: PROXIMITY_SCHEMA_V1,
            memory_a: left.to_owned(),
            memory_b: right.to_owned(),
            snapshot_version,
            min_cut: None,
            interpretation: "unreachable".to_owned(),
            tree_path: None,
            degraded: vec![proximity_unreachable_degradation(left, right)],
        };
    }

    let min_cut = query_min_cut(tree, left, right);
    ProximityReport {
        schema: PROXIMITY_SCHEMA_V1,
        memory_a: left.to_owned(),
        memory_b: right.to_owned(),
        snapshot_version,
        min_cut,
        interpretation: proximity_interpretation(min_cut).to_owned(),
        tree_path: tree_path_nodes(&tree.tree, left, right),
        degraded: Vec::new(),
    }
}

impl TreeMinCutIndex {
    fn new(tree: &Graph) -> Self {
        let nodes = tree
            .nodes_ordered()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let node_index = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.clone(), index))
            .collect::<HashMap<_, _>>();
        let node_count = nodes.len();
        let level_count = binary_lifting_level_count(node_count);
        let mut component_by_index = vec![usize::MAX; node_count];
        let mut depth_by_index = vec![0; node_count];
        let mut ancestor_by_power = vec![vec![0; node_count]; level_count];
        let mut min_cut_by_power = vec![vec![f64::INFINITY; node_count]; level_count];
        let mut component_index = 0;

        for root_index in 0..node_count {
            if component_by_index[root_index] != usize::MAX {
                continue;
            }
            component_by_index[root_index] = component_index;
            ancestor_by_power[0][root_index] = root_index;
            let mut queue = VecDeque::from([root_index]);

            while let Some(current_index) = queue.pop_front() {
                let current = &nodes[current_index];
                let Some(neighbors) = tree.neighbors_iter(current) else {
                    continue;
                };
                for neighbor in neighbors {
                    let Some(&neighbor_index) = node_index.get(neighbor) else {
                        continue;
                    };
                    if component_by_index[neighbor_index] != usize::MAX {
                        continue;
                    }
                    component_by_index[neighbor_index] = component_index;
                    depth_by_index[neighbor_index] = depth_by_index[current_index] + 1;
                    ancestor_by_power[0][neighbor_index] = current_index;
                    min_cut_by_power[0][neighbor_index] =
                        gomory_tree_edge_weight(tree, current, neighbor);
                    queue.push_back(neighbor_index);
                }
            }

            component_index += 1;
        }

        for level in 1..level_count {
            for node in 0..node_count {
                let mid_ancestor = ancestor_by_power[level - 1][node];
                ancestor_by_power[level][node] = ancestor_by_power[level - 1][mid_ancestor];
                min_cut_by_power[level][node] = min_cut_by_power[level - 1][node]
                    .min(min_cut_by_power[level - 1][mid_ancestor]);
            }
        }

        Self {
            node_index,
            component_by_index,
            depth_by_index,
            ancestor_by_power,
            min_cut_by_power,
        }
    }

    fn query_min_cut(&self, left: &str, right: &str) -> Option<f64> {
        let mut left_index = *self.node_index.get(left)?;
        let mut right_index = *self.node_index.get(right)?;
        if self.component_by_index[left_index] != self.component_by_index[right_index] {
            return None;
        }

        let mut min_cut = f64::INFINITY;
        if self.depth_by_index[left_index] < self.depth_by_index[right_index] {
            std::mem::swap(&mut left_index, &mut right_index);
        }

        let depth_delta = self.depth_by_index[left_index] - self.depth_by_index[right_index];
        for level in 0..self.ancestor_by_power.len() {
            if (depth_delta & (1_usize << level)) == 0 {
                continue;
            }
            min_cut = min_cut.min(self.min_cut_by_power[level][left_index]);
            left_index = self.ancestor_by_power[level][left_index];
        }

        if left_index == right_index {
            return Some(finite_path_min_cut(min_cut));
        }

        for level in (0..self.ancestor_by_power.len()).rev() {
            if self.ancestor_by_power[level][left_index]
                == self.ancestor_by_power[level][right_index]
            {
                continue;
            }
            min_cut = min_cut.min(self.min_cut_by_power[level][left_index]);
            min_cut = min_cut.min(self.min_cut_by_power[level][right_index]);
            left_index = self.ancestor_by_power[level][left_index];
            right_index = self.ancestor_by_power[level][right_index];
        }

        min_cut = min_cut.min(self.min_cut_by_power[0][left_index]);
        min_cut = min_cut.min(self.min_cut_by_power[0][right_index]);
        Some(finite_path_min_cut(min_cut))
    }
}

fn binary_lifting_level_count(node_count: usize) -> usize {
    usize::BITS
        .saturating_sub(node_count.max(1).leading_zeros())
        .max(1) as usize
}

fn finite_path_min_cut(min_cut: f64) -> f64 {
    if min_cut.is_finite() { min_cut } else { 0.0 }
}

fn graph_components(graph: &Graph) -> HashMap<String, usize> {
    let mut components = HashMap::new();
    let mut component_index = 0;
    for node in graph.nodes_ordered() {
        if components.contains_key(node) {
            continue;
        }

        let mut queue = VecDeque::from([node.to_owned()]);
        while let Some(current) = queue.pop_front() {
            if components
                .insert(current.clone(), component_index)
                .is_some()
            {
                continue;
            }
            let Some(neighbors) = graph.neighbors_iter(&current) else {
                continue;
            };
            for neighbor in neighbors {
                if !components.contains_key(neighbor) {
                    queue.push_back(neighbor.to_owned());
                }
            }
        }
        component_index += 1;
    }
    components
}

fn same_original_component(tree: &GomoryHuTree, left: &str, right: &str) -> bool {
    tree.component_by_node
        .get(left)
        .is_some_and(|left_component| {
            tree.component_by_node
                .get(right)
                .is_some_and(|right_component| left_component == right_component)
        })
}

fn proximity_unreachable_degradation(left: &str, right: &str) -> ProximityDegradation {
    ProximityDegradation {
        code: GRAPH_PROXIMITY_UNREACHABLE_CODE.to_owned(),
        severity: "info".to_owned(),
        message: format!(
            "Proximity query endpoints {left} and {right} are unreachable across different components."
        ),
        repair: None,
    }
}

fn proximity_interpretation(min_cut: Option<f64>) -> &'static str {
    match min_cut {
        None => "unavailable",
        Some(0.0) => "self",
        Some(cut) if cut < 1.0 => "weak",
        Some(cut) if cut < 3.0 => "moderate",
        Some(_) => "strong",
    }
}

fn tree_path_nodes(tree: &Graph, left: &str, right: &str) -> Option<Vec<String>> {
    if !tree.has_node(left) || !tree.has_node(right) {
        return None;
    }
    if left == right {
        return Some(vec![left.to_owned()]);
    }

    let mut queue = VecDeque::from([left.to_owned()]);
    let mut predecessor: BTreeMap<String, Option<String>> =
        BTreeMap::from([(left.to_owned(), None)]);

    while let Some(node) = queue.pop_front() {
        let Some(neighbors) = tree.neighbors_iter(&node) else {
            continue;
        };
        for neighbor in neighbors {
            if predecessor.contains_key(neighbor) {
                continue;
            }
            predecessor.insert(neighbor.to_owned(), Some(node.clone()));
            if neighbor == right {
                return reconstruct_tree_path(&predecessor, right);
            }
            queue.push_back(neighbor.to_owned());
        }
    }

    None
}

fn reconstruct_tree_path(
    predecessor: &BTreeMap<String, Option<String>>,
    right: &str,
) -> Option<Vec<String>> {
    let mut path = Vec::new();
    let mut current = right.to_owned();
    loop {
        path.push(current.clone());
        match predecessor.get(&current)? {
            Some(parent) => current = parent.clone(),
            None => break,
        }
    }
    path.reverse();
    Some(path)
}

fn gomory_tree_edge_weight(tree: &Graph, left: &str, right: &str) -> f64 {
    tree.edge_attrs(left, right)
        .and_then(|attrs| attrs.get(GOMORY_HU_WEIGHT_ATTR))
        .and_then(fnx_runtime::CgseValue::as_f64)
        .filter(|weight| weight.is_finite() && *weight >= 0.0)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_classes::AttrMap;
    use fnx_runtime::CgseValue;

    type TestResult = Result<(), String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    fn weighted_edge(weight: f64) -> AttrMap {
        let mut attrs = AttrMap::new();
        attrs.insert(GOMORY_HU_WEIGHT_ATTR.to_owned(), CgseValue::Float(weight));
        attrs
    }

    fn add_weighted_edge(graph: &mut Graph, left: &str, right: &str, weight: f64) {
        let result = graph.add_edge_with_attrs(left, right, weighted_edge(weight));
        assert!(
            result.is_ok(),
            "test graph edge should be valid: {result:?}"
        );
    }

    #[test]
    fn gomory_hu_connected_path_uses_path_bottleneck() -> TestResult {
        let mut graph = Graph::strict();
        add_weighted_edge(&mut graph, "a", "b", 3.0);
        add_weighted_edge(&mut graph, "b", "c", 2.0);

        let tree = graph_result(build_gomory_hu_tree(&graph))?;

        assert_eq!(query_min_cut(&tree, "a", "c"), Some(2.0));
        assert_eq!(query_min_cut(&tree, "c", "a"), Some(2.0));
        Ok(())
    }

    #[test]
    fn gomory_hu_disconnected_graph_reports_zero_cut() -> TestResult {
        let mut graph = Graph::strict();
        graph.add_node("a");
        graph.add_node("b");

        let tree = graph_result(build_gomory_hu_tree(&graph))?;

        assert_eq!(query_min_cut(&tree, "a", "b"), Some(0.0));
        Ok(())
    }

    #[test]
    fn proximity_report_marks_different_components_unreachable() -> TestResult {
        let mut graph = Graph::strict();
        graph.add_node("a");
        graph.add_node("b");

        let tree = graph_result(build_gomory_hu_tree(&graph))?;
        let report = query_proximity(&tree, "a", "b", 7);

        assert_eq!(report.schema, PROXIMITY_SCHEMA_V1);
        assert_eq!(report.memory_a, "a");
        assert_eq!(report.memory_b, "b");
        assert_eq!(report.snapshot_version, 7);
        assert_eq!(report.min_cut, None);
        assert_eq!(report.interpretation, "unreachable");
        assert_eq!(report.tree_path, None);
        assert_eq!(report.degraded.len(), 1);
        let degraded = &report.degraded[0];
        assert_eq!(degraded.code, GRAPH_PROXIMITY_UNREACHABLE_CODE);
        assert_eq!(degraded.severity, "info");
        assert!(degraded.message.contains("unreachable"));
        assert!(degraded.message.contains("different components"));
        assert_eq!(degraded.repair, None);
        Ok(())
    }

    #[test]
    fn proximity_report_serializes_aggregated_degraded_entries() -> TestResult {
        let report = ProximityReport {
            schema: PROXIMITY_SCHEMA_V1,
            memory_a: "a".to_owned(),
            memory_b: "b".to_owned(),
            snapshot_version: 7,
            min_cut: None,
            interpretation: "unreachable".to_owned(),
            tree_path: None,
            degraded: vec![
                proximity_unreachable_degradation("a", "b"),
                proximity_unreachable_degradation("a", "b"),
            ],
        };

        let value = serde_json::to_value(report).map_err(|error| error.to_string())?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                "serialized proximity report should include degraded array".to_owned()
            })?;

        assert_eq!(degraded.len(), 1);
        assert_eq!(
            degraded[0].get("code"),
            Some(&serde_json::json!(GRAPH_PROXIMITY_UNREACHABLE_CODE))
        );
        assert_eq!(
            degraded[0].get("severity"),
            Some(&serde_json::json!("info"))
        );
        assert_eq!(
            degraded[0].get("repair"),
            Some(&serde_json::json!("Refresh graph proximity diagnostics."))
        );
        assert_eq!(
            degraded[0].get("sources"),
            Some(&serde_json::json!(["gomory_hu_proximity"]))
        );
        Ok(())
    }

    #[test]
    fn gomory_hu_single_node_has_zero_self_cut() -> TestResult {
        let mut graph = Graph::strict();
        graph.add_node("a");

        let tree = graph_result(build_gomory_hu_tree(&graph))?;

        assert_eq!(tree.tree.node_count(), 1);
        assert_eq!(query_min_cut(&tree, "a", "a"), Some(0.0));
        assert_eq!(query_min_cut(&tree, "a", "missing"), None);
        Ok(())
    }

    #[test]
    fn gomory_hu_complete_graph_has_three_way_cut() -> TestResult {
        let mut graph = Graph::strict();
        for left in ["a", "b", "c", "d"] {
            for right in ["a", "b", "c", "d"] {
                if left < right {
                    add_weighted_edge(&mut graph, left, right, 1.0);
                }
            }
        }

        let tree = graph_result(build_gomory_hu_tree(&graph))?;

        assert_eq!(query_min_cut(&tree, "a", "d"), Some(3.0));
        assert_eq!(tree.tree.edge_count(), 3);
        Ok(())
    }

    #[test]
    fn gomory_hu_weighted_graph_preserves_min_cut_capacity() -> TestResult {
        let mut graph = Graph::strict();
        add_weighted_edge(&mut graph, "a", "b", 5.0);
        add_weighted_edge(&mut graph, "b", "c", 1.5);
        add_weighted_edge(&mut graph, "a", "c", 2.0);

        let tree = graph_result(build_gomory_hu_tree(&graph))?;

        assert_eq!(query_min_cut(&tree, "a", "b"), Some(6.5));
        assert_eq!(query_min_cut(&tree, "b", "c"), Some(3.5));
        Ok(())
    }

    #[test]
    fn gomory_hu_build_is_deterministic_across_three_runs() -> TestResult {
        let mut graph = Graph::strict();
        add_weighted_edge(&mut graph, "b", "c", 2.0);
        add_weighted_edge(&mut graph, "a", "b", 3.0);
        add_weighted_edge(&mut graph, "a", "c", 4.0);
        add_weighted_edge(&mut graph, "c", "d", 1.0);

        let first = graph_result(build_gomory_hu_tree(&graph))?;
        let second = graph_result(build_gomory_hu_tree(&graph))?;
        let third = graph_result(build_gomory_hu_tree(&graph))?;

        assert_eq!(first.tree.edges_ordered(), second.tree.edges_ordered());
        assert_eq!(second.tree.edges_ordered(), third.tree.edges_ordered());
        for left in graph.nodes_ordered() {
            for right in graph.nodes_ordered() {
                assert_eq!(
                    query_min_cut(&first, left, right),
                    query_min_cut(&second, left, right)
                );
                assert_eq!(
                    query_min_cut(&second, left, right),
                    query_min_cut(&third, left, right)
                );
            }
        }
        Ok(())
    }
}
