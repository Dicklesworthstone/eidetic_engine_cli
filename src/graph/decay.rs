use std::collections::{BTreeMap, BTreeSet};

use fnx_algorithms::{articulation_points, number_connected_components, onion_layers};
use fnx_classes::Graph;
use serde::{Deserialize, Serialize};

use crate::graph::{GraphResult, algorithms};

pub const DEFAULT_ONION_DECAY_MAX: f64 = 3.0;
pub const DEFAULT_ARTICULATION_PROTECTION: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralDecayPolicy {
    pub onion_decay_max: f64,
    pub articulation_protection: f64,
}

impl StructuralDecayPolicy {
    #[must_use]
    pub fn from_optional_config(
        onion_decay_max: Option<f64>,
        articulation_protection: Option<f64>,
    ) -> Self {
        Self {
            onion_decay_max: onion_decay_max.unwrap_or(DEFAULT_ONION_DECAY_MAX),
            articulation_protection: articulation_protection
                .unwrap_or(DEFAULT_ARTICULATION_PROTECTION),
        }
    }
}

impl Default for StructuralDecayPolicy {
    fn default() -> Self {
        Self {
            onion_decay_max: DEFAULT_ONION_DECAY_MAX,
            articulation_protection: DEFAULT_ARTICULATION_PROTECTION,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArticulationPointReport {
    pub memory_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnionLayerReport {
    pub layers_by_memory: BTreeMap<String, usize>,
    pub max_layer: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralDecayMultiplier {
    pub memory_id: String,
    pub onion_layer: Option<usize>,
    pub max_layer: usize,
    pub is_articulation_point: bool,
    pub structural_multiplier: f64,
    pub rationale: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralDecayConnectivityReport {
    pub component_count: usize,
    pub is_connected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructuralDecayIndex {
    onion: OnionLayerReport,
    articulation_points: BTreeSet<String>,
    policy: StructuralDecayPolicy,
}

impl StructuralDecayIndex {
    #[must_use]
    pub fn adjustment(&self, memory_id: &str) -> StructuralDecayMultiplier {
        let onion_layer = self.onion.layers_by_memory.get(memory_id).copied();
        let is_articulation_point = self.articulation_points.contains(memory_id);

        let Some(layer) = onion_layer else {
            return StructuralDecayMultiplier {
                memory_id: memory_id.to_owned(),
                onion_layer,
                max_layer: self.onion.max_layer,
                is_articulation_point,
                structural_multiplier: 1.0,
                rationale: "structural_decay_baseline".to_owned(),
            };
        };

        if self.onion.max_layer < 2 {
            return StructuralDecayMultiplier {
                memory_id: memory_id.to_owned(),
                onion_layer: Some(layer),
                max_layer: self.onion.max_layer,
                is_articulation_point,
                structural_multiplier: 1.0,
                rationale: "structural_decay_baseline".to_owned(),
            };
        }

        let onion_normalized =
            (self.onion.max_layer.saturating_sub(layer)) as f64 / self.onion.max_layer as f64;
        let onion_multiplier =
            1.0 + (self.policy.onion_decay_max - 1.0).max(0.0) * onion_normalized;
        let articulation_multiplier = if is_articulation_point {
            if self.policy.articulation_protection.is_nan() {
                DEFAULT_ARTICULATION_PROTECTION
            } else {
                self.policy.articulation_protection.clamp(0.0, 1.0)
            }
        } else {
            1.0
        };
        let structural_multiplier = onion_multiplier * articulation_multiplier;

        StructuralDecayMultiplier {
            memory_id: memory_id.to_owned(),
            onion_layer: Some(layer),
            max_layer: self.onion.max_layer,
            is_articulation_point,
            structural_multiplier,
            rationale: structural_decay_rationale(layer, is_articulation_point),
        }
    }
}

#[must_use]
pub fn compute_structural_decay_index(graph: &Graph) -> StructuralDecayIndex {
    compute_structural_decay_index_with_policy(graph, StructuralDecayPolicy::default())
}

#[must_use]
pub fn compute_structural_decay_index_with_policy(
    graph: &Graph,
    policy: StructuralDecayPolicy,
) -> StructuralDecayIndex {
    StructuralDecayIndex {
        onion: compute_onion_layers(graph),
        articulation_points: compute_articulation_points(graph)
            .memory_ids
            .into_iter()
            .collect(),
        policy,
    }
}

#[must_use]
pub fn compute_articulation_points(graph: &Graph) -> ArticulationPointReport {
    match try_compute_articulation_points(graph) {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(
                target: "ee::graph",
                algorithm = "articulation_points",
                error = %error,
                "structural decay articulation-point wrapper failed; returning empty report"
            );
            ArticulationPointReport { memory_ids: vec![] }
        }
    }
}

pub fn try_compute_articulation_points(graph: &Graph) -> GraphResult<ArticulationPointReport> {
    let cx = algorithms::current_or_testing_cx();
    let graph = graph.clone();
    algorithms::run_with_budget(
        &cx,
        "articulation_points",
        algorithms::DEFAULT_BACKGROUND_BUDGET,
        move || compute_articulation_points_unbudgeted(&graph),
    )
}

fn compute_articulation_points_unbudgeted(graph: &Graph) -> ArticulationPointReport {
    let mut memory_ids = articulation_points(graph).nodes;
    memory_ids.sort();
    ArticulationPointReport { memory_ids }
}

#[must_use]
pub fn compute_onion_layers(graph: &Graph) -> OnionLayerReport {
    match try_compute_onion_layers(graph) {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(
                target: "ee::graph",
                algorithm = "onion_layers",
                error = %error,
                "structural decay onion-layer wrapper failed; returning empty report"
            );
            OnionLayerReport {
                layers_by_memory: BTreeMap::new(),
                max_layer: 0,
            }
        }
    }
}

pub fn try_compute_onion_layers(graph: &Graph) -> GraphResult<OnionLayerReport> {
    let cx = algorithms::current_or_testing_cx();
    let graph = graph.clone();
    algorithms::run_with_budget(
        &cx,
        "onion_layers",
        algorithms::DEFAULT_BACKGROUND_BUDGET,
        move || compute_onion_layers_unbudgeted(&graph),
    )
}

fn compute_onion_layers_unbudgeted(graph: &Graph) -> OnionLayerReport {
    let layers_by_memory = onion_layers(graph)
        .layers
        .into_iter()
        .map(|layer| (layer.node, layer.layer))
        .collect::<BTreeMap<_, _>>();
    let max_layer = layers_by_memory.values().copied().max().unwrap_or(0);

    OnionLayerReport {
        layers_by_memory,
        max_layer,
    }
}

#[must_use]
pub fn compute_structural_decay_connectivity(graph: &Graph) -> StructuralDecayConnectivityReport {
    match try_compute_structural_decay_connectivity(graph) {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(
                target: "ee::graph",
                algorithm = "number_connected_components",
                error = %error,
                "structural decay connectivity wrapper failed; returning connected baseline"
            );
            StructuralDecayConnectivityReport {
                component_count: 0,
                is_connected: true,
            }
        }
    }
}

pub fn try_compute_structural_decay_connectivity(
    graph: &Graph,
) -> GraphResult<StructuralDecayConnectivityReport> {
    let cx = algorithms::current_or_testing_cx();
    let graph = graph.clone();
    algorithms::run_with_budget(
        &cx,
        "number_connected_components",
        algorithms::DEFAULT_BACKGROUND_BUDGET,
        move || compute_structural_decay_connectivity_unbudgeted(&graph),
    )
}

fn compute_structural_decay_connectivity_unbudgeted(
    graph: &Graph,
) -> StructuralDecayConnectivityReport {
    let component_count = number_connected_components(graph).count;
    StructuralDecayConnectivityReport {
        component_count,
        is_connected: component_count <= 1,
    }
}

#[must_use]
pub fn compute_structural_decay_multiplier(graph: &Graph, memory_id: &str) -> f64 {
    compute_structural_decay_adjustment(graph, memory_id).structural_multiplier
}

#[must_use]
pub fn compute_structural_decay_adjustment(
    graph: &Graph,
    memory_id: &str,
) -> StructuralDecayMultiplier {
    compute_structural_decay_adjustment_with_policy(
        graph,
        memory_id,
        StructuralDecayPolicy::default(),
    )
}

#[must_use]
pub fn compute_structural_decay_adjustment_with_policy(
    graph: &Graph,
    memory_id: &str,
    policy: StructuralDecayPolicy,
) -> StructuralDecayMultiplier {
    compute_structural_decay_index_with_policy(graph, policy).adjustment(memory_id)
}

fn structural_decay_rationale(layer: usize, is_articulation_point: bool) -> String {
    if is_articulation_point {
        format!("articulation_point_in_layer_{layer}")
    } else {
        format!("onion_layer_{layer}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_runtime::CompatibilityMode;

    #[test]
    fn articulation_points_are_sorted() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("b", "c"), ("b", "d")]);

        let report = compute_articulation_points(&graph);

        assert_eq!(report.memory_ids, vec!["b"]);
    }

    #[test]
    fn articulation_points_cover_disconnected_components() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("b", "c"), ("x", "y"), ("y", "z")]);

        let report = compute_articulation_points(&graph);

        assert_eq!(report.memory_ids, vec!["b", "y"]);
    }

    #[test]
    fn structural_decay_connectivity_reports_disconnected_graphs() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("x", "y")]);

        let report = compute_structural_decay_connectivity(&graph);

        assert_eq!(report.component_count, 2);
        assert!(!report.is_connected);
    }

    #[test]
    fn onion_layers_keep_core_above_leaf_shells() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([
            ("core_a", "core_b"),
            ("core_b", "core_c"),
            ("core_a", "core_c"),
            ("core_a", "leaf_a"),
            ("core_b", "leaf_b"),
        ]);

        let report = compute_onion_layers(&graph);

        assert_eq!(report.layers_by_memory.len(), 5);
        let leaf_layers = ["leaf_a", "leaf_b"]
            .iter()
            .map(|memory_id| report.layers_by_memory[*memory_id])
            .collect::<Vec<_>>();
        let core_layers = ["core_a", "core_b", "core_c"]
            .iter()
            .map(|memory_id| report.layers_by_memory[*memory_id])
            .collect::<Vec<_>>();
        let leaf_max = leaf_layers.iter().copied().fold(usize::MIN, usize::max);
        let core_min = core_layers.iter().copied().fold(usize::MAX, usize::min);
        assert!(
            core_min >= leaf_max,
            "core layer {core_min} should not be outside leaf layer {leaf_max}"
        );
        let observed_max_layer = report
            .layers_by_memory
            .values()
            .copied()
            .fold(usize::MIN, usize::max);
        assert_eq!(report.max_layer, observed_max_layer);
    }

    #[test]
    fn structural_decay_uses_baseline_for_single_shell_graphs() {
        let graph = Graph::complete_graph(CompatibilityMode::Strict, 4);

        let adjustment = compute_structural_decay_adjustment(&graph, "0");

        assert_eq!(adjustment.structural_multiplier, 1.0);
        assert_eq!(adjustment.rationale, "structural_decay_baseline");
    }

    #[test]
    fn structural_decay_uses_baseline_for_missing_memory() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("b", "c")]);

        let adjustment = compute_structural_decay_adjustment(&graph, "missing");

        assert_eq!(adjustment.memory_id, "missing");
        assert_eq!(adjustment.onion_layer, None);
        assert_eq!(adjustment.structural_multiplier, 1.0);
        assert!(!adjustment.is_articulation_point);
        assert_eq!(adjustment.rationale, "structural_decay_baseline");
    }

    #[test]
    fn structural_decay_protects_articulation_points() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("b", "c"), ("c", "d"), ("c", "e")]);

        let adjustment = compute_structural_decay_adjustment(&graph, "c");

        assert!(adjustment.is_articulation_point);
        assert!(adjustment.structural_multiplier < 1.0);
        assert_eq!(adjustment.rationale, "articulation_point_in_layer_2");
    }

    #[test]
    fn structural_decay_policy_uses_graph_config_overrides() {
        let policy = StructuralDecayPolicy::from_optional_config(Some(2.0), Some(0.25));
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("b", "c"), ("c", "d"), ("c", "e")]);

        let adjustment = compute_structural_decay_adjustment_with_policy(&graph, "c", policy);

        assert_eq!(policy.onion_decay_max, 2.0);
        assert_eq!(policy.articulation_protection, 0.25);
        assert!(adjustment.is_articulation_point);
        assert!(adjustment.structural_multiplier <= 0.5);
    }

    #[test]
    fn structural_decay_index_matches_direct_adjustments() {
        let policy = StructuralDecayPolicy::from_optional_config(Some(2.5), Some(0.4));
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([
            ("a", "b"),
            ("b", "c"),
            ("c", "d"),
            ("c", "e"),
            ("e", "f"),
        ]);
        let index = compute_structural_decay_index_with_policy(&graph, policy);

        for memory_id in ["a", "b", "c", "e", "missing"] {
            assert_eq!(
                index.adjustment(memory_id),
                compute_structural_decay_adjustment_with_policy(&graph, memory_id, policy)
            );
        }
    }
}
