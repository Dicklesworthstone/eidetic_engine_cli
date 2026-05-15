use std::collections::{BTreeMap, BTreeSet};

use fnx_algorithms::{k_truss, louvain_communities};
use fnx_classes::Graph;
use serde::Serialize;

pub const HEALTH_STRUCTURAL_SCHEMA_V1: &str = "ee.health.structural.v1";
pub const DEFAULT_CONTRADICTION_DENSITY_THRESHOLD: f64 = 0.20;
const DEFAULT_LOUVAIN_RESOLUTION: f64 = 1.0;
const DEFAULT_LOUVAIN_THRESHOLD: f64 = 1.0e-7;
const DEFAULT_LOUVAIN_SEED: u64 = 0;
const LOUVAIN_WEIGHT_ATTR: &str = "weight";
const EXEMPLAR_LIMIT: usize = 3;
const MIN_CLUSTER_SIZE: usize = 3;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KTrussReport {
    pub schema: &'static str,
    pub max_k: usize,
    pub member_counts: BTreeMap<usize, usize>,
    pub top_memories_at_k: Vec<KTrussMemory>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KTrussMemory {
    pub memory_id: String,
    pub max_k: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContradictionCluster {
    pub louvain_id: usize,
    pub size: usize,
    pub internal_contradictions: usize,
    pub density: f64,
    pub severity: ContradictionSeverity,
    pub exemplar_memory_ids: Vec<String>,
    pub suggested_action: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContradictionSeverity {
    Inconsistent,
    Incoherent,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContradictionClusterPolicy {
    pub density_threshold: f64,
}

impl ContradictionClusterPolicy {
    #[must_use]
    pub fn from_optional_config(contradiction_threshold: Option<f64>) -> Self {
        Self {
            density_threshold: contradiction_threshold
                .unwrap_or(DEFAULT_CONTRADICTION_DENSITY_THRESHOLD),
        }
    }
}

impl Default for ContradictionClusterPolicy {
    fn default() -> Self {
        Self {
            density_threshold: DEFAULT_CONTRADICTION_DENSITY_THRESHOLD,
        }
    }
}

impl ContradictionSeverity {
    #[must_use]
    pub const fn suggested_action(self) -> &'static str {
        match self {
            Self::Inconsistent => "review",
            Self::Incoherent => "curate_urgent",
        }
    }
}

#[must_use]
pub fn compute_k_truss(graph: &Graph) -> KTrussReport {
    let mut member_counts = BTreeMap::new();
    let mut max_by_memory = BTreeMap::<String, usize>::new();

    for k in 3..=graph.edge_count().saturating_add(2) {
        let result = k_truss(graph, k);
        if result.nodes.is_empty() {
            if k > 3 {
                break;
            }
            continue;
        }

        member_counts.insert(k, result.nodes.len());
        for node in result.nodes {
            max_by_memory.insert(node, k);
        }
    }

    let max_k = max_by_memory.values().copied().max().unwrap_or(3);
    let mut top_memories_at_k = max_by_memory
        .into_iter()
        .map(|(memory_id, max_k)| KTrussMemory { memory_id, max_k })
        .collect::<Vec<_>>();
    top_memories_at_k.sort_by(|left, right| {
        right
            .max_k
            .cmp(&left.max_k)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    KTrussReport {
        schema: HEALTH_STRUCTURAL_SCHEMA_V1,
        max_k,
        member_counts,
        top_memories_at_k,
    }
}

#[must_use]
pub fn detect_louvain_communities(graph: &Graph) -> Vec<Vec<String>> {
    louvain_communities(
        graph,
        DEFAULT_LOUVAIN_RESOLUTION,
        LOUVAIN_WEIGHT_ATTR,
        DEFAULT_LOUVAIN_THRESHOLD,
        None,
        Some(DEFAULT_LOUVAIN_SEED),
    )
}

#[must_use]
pub fn detect_contradiction_clusters(graph: &Graph) -> Vec<ContradictionCluster> {
    detect_contradiction_clusters_with_policy(graph, ContradictionClusterPolicy::default())
}

#[must_use]
pub fn detect_contradiction_clusters_with_policy(
    graph: &Graph,
    policy: ContradictionClusterPolicy,
) -> Vec<ContradictionCluster> {
    detect_contradiction_clusters_with_threshold(graph, policy.density_threshold)
}

#[must_use]
pub fn detect_contradiction_clusters_with_threshold(
    graph: &Graph,
    density_threshold: f64,
) -> Vec<ContradictionCluster> {
    let threshold = density_threshold.clamp(0.0, 1.0);
    let mut clusters = detect_louvain_communities(graph)
        .into_iter()
        .enumerate()
        .filter_map(|(louvain_id, mut community)| {
            community.sort();
            let size = community.len();
            if size < MIN_CLUSTER_SIZE {
                return None;
            }

            let internal_contradictions = internal_edge_count(graph, &community);
            let possible_edges = size.saturating_mul(size.saturating_sub(1)) / 2;
            let density = if possible_edges == 0 {
                0.0
            } else {
                internal_contradictions as f64 / possible_edges as f64
            };
            if density < threshold {
                return None;
            }

            let severity = if density >= 0.50 {
                ContradictionSeverity::Incoherent
            } else {
                ContradictionSeverity::Inconsistent
            };
            let exemplar_memory_ids = community.iter().take(EXEMPLAR_LIMIT).cloned().collect();

            Some(ContradictionCluster {
                louvain_id,
                size,
                internal_contradictions,
                density,
                severity,
                exemplar_memory_ids,
                suggested_action: severity.suggested_action(),
            })
        })
        .collect::<Vec<_>>();

    clusters.sort_by(|left, right| {
        right
            .density
            .total_cmp(&left.density)
            .then_with(|| {
                right
                    .internal_contradictions
                    .cmp(&left.internal_contradictions)
            })
            .then_with(|| left.exemplar_memory_ids.cmp(&right.exemplar_memory_ids))
    });
    clusters
}

fn internal_edge_count(graph: &Graph, community: &[String]) -> usize {
    let members = community
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut edges = BTreeSet::<(&str, &str)>::new();
    for node in community {
        let Some(neighbors) = graph.neighbors(node) else {
            continue;
        };
        for neighbor in neighbors {
            if !members.contains(neighbor) || node.as_str() == neighbor {
                continue;
            }
            let edge = if node.as_str() < neighbor {
                (node.as_str(), neighbor)
            } else {
                (neighbor, node.as_str())
            };
            edges.insert(edge);
        }
    }
    edges.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_runtime::CompatibilityMode;

    #[test]
    fn k_truss_report_finds_complete_graph_core() {
        let graph = Graph::complete_graph(CompatibilityMode::Strict, 4);

        let report = compute_k_truss(&graph);

        assert_eq!(report.max_k, 4);
        assert_eq!(report.member_counts.get(&4), Some(&4));
        assert_eq!(report.top_memories_at_k.len(), 4);
    }

    #[test]
    fn contradiction_clusters_filter_by_density_threshold() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("a", "c"), ("b", "c"), ("d", "e")]);

        let clusters = detect_contradiction_clusters_with_threshold(&graph, 0.50);

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].size, 3);
        assert_eq!(clusters[0].internal_contradictions, 3);
        assert_eq!(clusters[0].severity, ContradictionSeverity::Incoherent);
        assert_eq!(clusters[0].exemplar_memory_ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn contradiction_policy_uses_graph_config_override() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded([("a", "b"), ("b", "c")]);
        let permissive_policy = ContradictionClusterPolicy::from_optional_config(Some(0.50));
        let strict_policy = ContradictionClusterPolicy::from_optional_config(Some(0.75));

        assert_eq!(permissive_policy.density_threshold, 0.50);
        assert_eq!(
            detect_contradiction_clusters_with_policy(&graph, permissive_policy).len(),
            1
        );
        assert!(detect_contradiction_clusters_with_policy(&graph, strict_policy).is_empty());
    }
}
