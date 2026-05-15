use std::collections::{BTreeMap, BTreeSet};

use fnx_algorithms::{articulation_points, onion_layers};
use fnx_classes::Graph;
use serde::{Deserialize, Serialize};

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

#[must_use]
pub fn compute_articulation_points(graph: &Graph) -> ArticulationPointReport {
    let mut memory_ids = articulation_points(graph).nodes;
    memory_ids.sort();
    ArticulationPointReport { memory_ids }
}

#[must_use]
pub fn compute_onion_layers(graph: &Graph) -> OnionLayerReport {
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
    let onion = compute_onion_layers(graph);
    let articulation_points = compute_articulation_points(graph)
        .memory_ids
        .into_iter()
        .collect::<BTreeSet<_>>();
    let onion_layer = onion.layers_by_memory.get(memory_id).copied();
    let is_articulation_point = articulation_points.contains(memory_id);

    let Some(layer) = onion_layer else {
        return StructuralDecayMultiplier {
            memory_id: memory_id.to_owned(),
            onion_layer,
            max_layer: onion.max_layer,
            is_articulation_point,
            structural_multiplier: 1.0,
            rationale: "structural_decay_baseline".to_owned(),
        };
    };

    if onion.max_layer < 2 {
        return StructuralDecayMultiplier {
            memory_id: memory_id.to_owned(),
            onion_layer: Some(layer),
            max_layer: onion.max_layer,
            is_articulation_point,
            structural_multiplier: 1.0,
            rationale: "structural_decay_baseline".to_owned(),
        };
    }

    let onion_normalized = (onion.max_layer.saturating_sub(layer)) as f64 / onion.max_layer as f64;
    let onion_multiplier = 1.0 + (policy.onion_decay_max - 1.0).max(0.0) * onion_normalized;
    let articulation_multiplier = if is_articulation_point {
        policy.articulation_protection.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let structural_multiplier = onion_multiplier * articulation_multiplier;

    StructuralDecayMultiplier {
        memory_id: memory_id.to_owned(),
        onion_layer: Some(layer),
        max_layer: onion.max_layer,
        is_articulation_point,
        structural_multiplier,
        rationale: structural_decay_rationale(layer, is_articulation_point),
    }
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
    fn structural_decay_uses_baseline_for_single_shell_graphs() {
        let graph = Graph::complete_graph(CompatibilityMode::Strict, 4);

        let adjustment = compute_structural_decay_adjustment(&graph, "0");

        assert_eq!(adjustment.structural_multiplier, 1.0);
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
}
