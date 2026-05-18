use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use fnx_classes::Graph;
use serde::Serialize;

use crate::graph::decay::compute_onion_layers;
use crate::graph::health::{compute_k_truss, detect_louvain_communities};

pub const KNOWLEDGE_SKYLINE_SCHEMA_V1: &str = "ee.knowledge_skyline.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeSkylineMemory {
    pub memory_id: String,
    pub trust_class: String,
    pub created_at: DateTime<Utc>,
}

pub struct KnowledgeSkylineInput {
    pub graph: Graph,
    pub memories: Vec<KnowledgeSkylineMemory>,
    pub ppr_scores: BTreeMap<String, f64>,
    pub as_of: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSkyline {
    pub schema: &'static str,
    pub node_count: usize,
    pub row_count: usize,
    pub trust_class_count: usize,
    pub max_onion_layer: usize,
    pub rows: Vec<KnowledgeSkylineLayerRow>,
    pub communities: Vec<KnowledgeSkylineCommunitySummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSkylineLayerRow {
    pub onion_layer: usize,
    pub cells: Vec<KnowledgeSkylineCell>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSkylineCell {
    pub trust_class: String,
    pub count: usize,
    pub mean_age_days: f64,
    pub mean_age_decile: f64,
    pub k_truss_rank: usize,
    pub ppr_percentile: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSkylineCommunitySummary {
    pub community_id: usize,
    pub size: usize,
    pub onion_layer_min: usize,
    pub onion_layer_max: usize,
    pub core_count: usize,
    pub periphery_count: usize,
    pub k_truss_core_count: usize,
    pub diagnostic_label: KnowledgeSkylineDiagnosticLabel,
    pub exemplar_memory_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSkylineDiagnosticLabel {
    PeripheryHeavy,
    Balanced,
    CoreSparse,
}

#[derive(Clone, Debug)]
struct MemoryMetrics {
    trust_class: String,
    age_days: f64,
    age_decile: usize,
    onion_layer: usize,
    k_truss_rank: usize,
    ppr_percentile: f64,
}

#[must_use]
pub fn compute_knowledge_skyline(input: &KnowledgeSkylineInput) -> KnowledgeSkyline {
    let mut memories = input.memories.clone();
    memories.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));

    let onion_layers = compute_onion_layers(&input.graph);
    let k_truss = compute_k_truss(&input.graph);
    let ppr_percentiles = ppr_percentiles(&input.ppr_scores);
    let age_deciles = age_deciles(&memories, input.as_of);

    let k_truss_ranks: BTreeMap<String, usize> = k_truss
        .top_memories_at_k
        .into_iter()
        .map(|entry| (entry.memory_id, entry.max_k))
        .collect();

    let mut trust_classes = BTreeSet::new();
    let mut metrics_by_memory = BTreeMap::<String, MemoryMetrics>::new();
    for memory in &memories {
        trust_classes.insert(memory.trust_class.clone());
        let onion_layer = onion_layers
            .layers_by_memory
            .get(&memory.memory_id)
            .copied()
            .unwrap_or(0);
        let k_truss_rank = k_truss_ranks
            .get(&memory.memory_id)
            .copied()
            .unwrap_or(0);
        let age_days = age_days(memory.created_at, input.as_of);
        metrics_by_memory.insert(
            memory.memory_id.clone(),
            MemoryMetrics {
                trust_class: memory.trust_class.clone(),
                age_days,
                age_decile: *age_deciles.get(&memory.memory_id).unwrap_or(&0),
                onion_layer,
                k_truss_rank,
                ppr_percentile: *ppr_percentiles.get(&memory.memory_id).unwrap_or(&0.0),
            },
        );
    }

    let mut row_layers = metrics_by_memory
        .values()
        .map(|metrics| metrics.onion_layer)
        .collect::<BTreeSet<_>>();
    if row_layers.is_empty() {
        row_layers.insert(0);
    }
    let trust_classes = trust_classes.into_iter().collect::<Vec<_>>();

    let mut metrics_by_cell: BTreeMap<(usize, String), Vec<&MemoryMetrics>> = BTreeMap::new();
    for metrics in metrics_by_memory.values() {
        metrics_by_cell
            .entry((metrics.onion_layer, metrics.trust_class.clone()))
            .or_default()
            .push(metrics);
    }

    let rows = row_layers
        .into_iter()
        .map(|onion_layer| KnowledgeSkylineLayerRow {
            onion_layer,
            cells: trust_classes
                .iter()
                .map(|trust_class| {
                    let matching = metrics_by_cell
                        .get(&(onion_layer, trust_class.clone()))
                        .map(|v| v.as_slice())
                        .unwrap_or(&[]);
                    skyline_cell(matching, trust_class.as_str())
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    KnowledgeSkyline {
        schema: KNOWLEDGE_SKYLINE_SCHEMA_V1,
        node_count: memories.len(),
        row_count: rows.len(),
        trust_class_count: trust_classes.len(),
        max_onion_layer: onion_layers.max_layer,
        rows,
        communities: community_summaries(&input.graph, &metrics_by_memory),
    }
}

fn skyline_cell(
    matching: &[&MemoryMetrics],
    trust_class: &str,
) -> KnowledgeSkylineCell {
    let count = matching.len();
    if count == 0 {
        return KnowledgeSkylineCell {
            trust_class: trust_class.to_owned(),
            count: 0,
            mean_age_days: 0.0,
            mean_age_decile: 0.0,
            k_truss_rank: 0,
            ppr_percentile: 0.0,
        };
    }

    let mean_age_days = matching.iter().map(|metrics| metrics.age_days).sum::<f64>() / count as f64;
    let mean_age_decile = matching
        .iter()
        .map(|metrics| metrics.age_decile as f64)
        .sum::<f64>()
        / count as f64;
    let k_truss_rank = matching
        .iter()
        .map(|metrics| metrics.k_truss_rank)
        .max()
        .unwrap_or(0);
    let ppr_percentile = matching
        .iter()
        .map(|metrics| metrics.ppr_percentile)
        .sum::<f64>()
        / count as f64;

    KnowledgeSkylineCell {
        trust_class: trust_class.to_owned(),
        count,
        mean_age_days,
        mean_age_decile,
        k_truss_rank,
        ppr_percentile,
    }
}

fn community_summaries(
    graph: &Graph,
    metrics_by_memory: &BTreeMap<String, MemoryMetrics>,
) -> Vec<KnowledgeSkylineCommunitySummary> {
    let mut communities = detect_louvain_communities(graph)
        .into_iter()
        .enumerate()
        .map(|(community_id, mut members)| {
            members.sort();
            let layers = members
                .iter()
                .filter_map(|memory_id| metrics_by_memory.get(memory_id))
                .map(|metrics| metrics.onion_layer)
                .collect::<Vec<_>>();
            let onion_layer_min = layers.iter().copied().min().unwrap_or(0);
            let onion_layer_max = layers.iter().copied().max().unwrap_or(0);
            let midpoint = onion_layer_min + (onion_layer_max.saturating_sub(onion_layer_min) / 2);
            let periphery_count = layers.iter().filter(|layer| **layer <= midpoint).count();
            let core_count = layers.len().saturating_sub(periphery_count);
            let k_truss_core_count = members
                .iter()
                .filter_map(|memory_id| metrics_by_memory.get(memory_id))
                .filter(|metrics| metrics.k_truss_rank >= 3)
                .count();
            let diagnostic_label = community_label(
                layers.len(),
                core_count,
                periphery_count,
                k_truss_core_count,
            );
            let exemplar_memory_ids = members.iter().take(3).cloned().collect();

            KnowledgeSkylineCommunitySummary {
                community_id,
                size: members.len(),
                onion_layer_min,
                onion_layer_max,
                core_count,
                periphery_count,
                k_truss_core_count,
                diagnostic_label,
                exemplar_memory_ids,
            }
        })
        .collect::<Vec<_>>();

    communities.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.community_id.cmp(&right.community_id))
    });
    communities
}

fn community_label(
    size: usize,
    core_count: usize,
    periphery_count: usize,
    k_truss_core_count: usize,
) -> KnowledgeSkylineDiagnosticLabel {
    if size >= 3 && k_truss_core_count == 0 {
        KnowledgeSkylineDiagnosticLabel::CoreSparse
    } else if periphery_count > core_count {
        KnowledgeSkylineDiagnosticLabel::PeripheryHeavy
    } else {
        KnowledgeSkylineDiagnosticLabel::Balanced
    }
}

fn ppr_percentiles(scores: &BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    let mut ranked = scores
        .iter()
        .map(|(memory_id, score)| (memory_id.clone(), finite_or_zero(*score)))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    if ranked.is_empty() {
        return BTreeMap::new();
    }
    let denominator = ranked.len().saturating_sub(1).max(1) as f64;
    ranked
        .into_iter()
        .enumerate()
        .map(|(index, (memory_id, _))| (memory_id, index as f64 / denominator))
        .collect()
}

fn age_deciles(
    memories: &[KnowledgeSkylineMemory],
    as_of: DateTime<Utc>,
) -> BTreeMap<String, usize> {
    let mut ranked = memories
        .iter()
        .map(|memory| (memory.memory_id.clone(), age_days(memory.created_at, as_of)))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    if ranked.is_empty() {
        return BTreeMap::new();
    }
    let denominator = ranked.len().saturating_sub(1).max(1) as f64;
    ranked
        .into_iter()
        .enumerate()
        .map(|(index, (memory_id, _))| {
            let decile = ((index as f64 / denominator) * 9.0).round() as usize;
            (memory_id, decile.min(9))
        })
        .collect()
}

fn age_days(created_at: DateTime<Utc>, as_of: DateTime<Utc>) -> f64 {
    let duration = as_of.signed_duration_since(created_at);
    duration.num_seconds().max(0) as f64 / 86_400.0
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use fnx_runtime::CompatibilityMode;

    fn ts(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, day, 0, 0, 0)
            .single()
            .expect("valid timestamp")
    }

    fn memory(memory_id: &str, trust_class: &str, day: u32) -> KnowledgeSkylineMemory {
        KnowledgeSkylineMemory {
            memory_id: memory_id.to_owned(),
            trust_class: trust_class.to_owned(),
            created_at: ts(day),
        }
    }

    fn graph(edges: impl IntoIterator<Item = (&'static str, &'static str)>) -> Graph {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        let _ = graph.extend_edges_unrecorded(edges);
        graph
    }

    fn ppr(scores: &[(&str, f64)]) -> BTreeMap<String, f64> {
        scores
            .iter()
            .map(|(memory_id, score)| ((*memory_id).to_owned(), *score))
            .collect()
    }

    #[test]
    fn skyline_single_community_builds_layer_trust_cells() {
        let graph = graph([("a", "b"), ("b", "c"), ("a", "c")]);
        let input = KnowledgeSkylineInput {
            graph,
            memories: vec![
                memory("a", "human_explicit", 1),
                memory("b", "agent_validated", 5),
                memory("c", "agent_validated", 10),
            ],
            ppr_scores: ppr(&[("a", 0.7), ("b", 0.2), ("c", 0.1)]),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);

        assert_eq!(skyline.schema, KNOWLEDGE_SKYLINE_SCHEMA_V1);
        assert_eq!(skyline.node_count, 3);
        assert_eq!(skyline.communities.len(), 1);
        assert!(skyline.trust_class_count >= 2);
        assert!(
            skyline
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.trust_class == "agent_validated" && cell.count == 2)
        );
    }

    #[test]
    fn skyline_multi_community_summaries_are_deterministic() {
        let graph = graph([("a", "b"), ("b", "c"), ("x", "y"), ("y", "z")]);
        let input = KnowledgeSkylineInput {
            graph,
            memories: vec![
                memory("z", "agent_assertion", 1),
                memory("a", "human_explicit", 2),
                memory("x", "agent_assertion", 3),
                memory("b", "human_explicit", 4),
                memory("y", "agent_assertion", 5),
                memory("c", "human_explicit", 6),
            ],
            ppr_scores: ppr(&[("a", 0.9), ("b", 0.8), ("c", 0.7), ("x", 0.3), ("y", 0.2)]),
            as_of: ts(16),
        };

        let first = compute_knowledge_skyline(&input);
        let second = compute_knowledge_skyline(&input);

        assert_eq!(first, second);
        assert!(first.communities.len() >= 2);
        assert!(
            first
                .communities
                .windows(2)
                .all(|pair| pair[0].size >= pair[1].size)
        );
    }

    #[test]
    fn skyline_labels_periphery_heavy_communities() {
        let graph = graph([
            ("core_a", "core_b"),
            ("core_b", "core_c"),
            ("core_a", "core_c"),
            ("core_a", "leaf_a"),
            ("core_a", "leaf_b"),
            ("core_b", "leaf_c"),
            ("core_c", "leaf_d"),
        ]);
        let input = KnowledgeSkylineInput {
            graph,
            memories: vec![
                memory("core_a", "human_explicit", 1),
                memory("core_b", "human_explicit", 2),
                memory("core_c", "human_explicit", 3),
                memory("leaf_a", "agent_assertion", 4),
                memory("leaf_b", "agent_assertion", 5),
                memory("leaf_c", "agent_assertion", 6),
                memory("leaf_d", "agent_assertion", 7),
            ],
            ppr_scores: ppr(&[
                ("core_a", 0.8),
                ("core_b", 0.7),
                ("core_c", 0.6),
                ("leaf_a", 0.1),
                ("leaf_b", 0.1),
                ("leaf_c", 0.1),
                ("leaf_d", 0.1),
            ]),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);

        assert!(
            skyline
                .communities
                .iter()
                .any(|community| community.diagnostic_label
                    == KnowledgeSkylineDiagnosticLabel::PeripheryHeavy)
        );
    }

    #[test]
    fn skyline_labels_balanced_complete_core() {
        let graph = Graph::complete_graph(CompatibilityMode::Strict, 4);
        let input = KnowledgeSkylineInput {
            graph,
            memories: vec![
                memory("0", "human_explicit", 1),
                memory("1", "human_explicit", 2),
                memory("2", "agent_validated", 3),
                memory("3", "agent_validated", 4),
            ],
            ppr_scores: ppr(&[("0", 0.4), ("1", 0.3), ("2", 0.2), ("3", 0.1)]),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);

        assert!(skyline.communities.iter().any(
            |community| community.diagnostic_label == KnowledgeSkylineDiagnosticLabel::Balanced
        ));
        assert!(
            skyline
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.k_truss_rank >= 4)
        );
    }

    #[test]
    fn skyline_labels_core_sparse_communities() {
        let graph = graph([("a", "b"), ("b", "c")]);
        let input = KnowledgeSkylineInput {
            graph,
            memories: vec![
                memory("a", "agent_assertion", 1),
                memory("b", "agent_assertion", 2),
                memory("c", "agent_assertion", 3),
            ],
            ppr_scores: ppr(&[("a", 0.1), ("b", 0.2), ("c", 0.1)]),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);

        assert!(
            skyline
                .communities
                .iter()
                .any(|community| community.diagnostic_label
                    == KnowledgeSkylineDiagnosticLabel::CoreSparse)
        );
    }

    #[test]
    fn skyline_computes_age_deciles_and_ppr_percentiles() {
        let graph = graph([("a", "b"), ("b", "c"), ("c", "d")]);
        let input = KnowledgeSkylineInput {
            graph,
            memories: vec![
                memory("a", "agent_assertion", 1),
                memory("b", "agent_assertion", 6),
                memory("c", "agent_assertion", 11),
                memory("d", "agent_assertion", 15),
            ],
            ppr_scores: ppr(&[("a", 0.1), ("b", 0.2), ("c", 0.3), ("d", 0.4)]),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);
        let populated = skyline
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .filter(|cell| cell.count > 0)
            .collect::<Vec<_>>();

        assert!(populated.iter().any(|cell| cell.mean_age_decile > 0.0));
        assert!(populated.iter().any(|cell| cell.ppr_percentile > 0.0));
    }
}
