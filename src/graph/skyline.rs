use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use fnx_classes::Graph;
use serde::Serialize;

use crate::graph::decay::compute_onion_layers;
use crate::graph::health::{compute_k_truss, detect_louvain_communities};
use crate::util::radix_ulid_sort::sort_by_ulid_payload_or_lexical;

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
    /// The actual Pareto frontier. bd-pmgg0.
    ///
    /// `rows` below is a layer x trust-class grid of CELL MEANS, which is a
    /// useful posture summary and is kept, but it is not a skyline: a mean
    /// cannot express dominance, and averaging is exactly what hides the
    /// memories a skyline exists to surface. A reader who knows what a skyline
    /// is will read this surface as a Pareto frontier, so it now contains one.
    pub frontier: Vec<KnowledgeSkylineFrontierPoint>,
    pub frontier_size: usize,
    pub rows: Vec<KnowledgeSkylineLayerRow>,
    pub communities: Vec<KnowledgeSkylineCommunitySummary>,
}

/// One non-dominated memory, with the dimension values that put it there.
///
/// The values are reported alongside the id deliberately: a frontier entry
/// without its coordinates is an assertion the caller cannot check, and the
/// whole complaint behind bd-pmgg0 was a surface whose numbers did not support
/// the reading its name invited.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSkylineFrontierPoint {
    pub memory_id: String,
    pub trust_class: String,
    pub trust_rank: u8,
    pub onion_layer: usize,
    pub k_truss_rank: usize,
    pub ppr_percentile: f64,
    pub age_days: f64,
    /// Dimensions on which no other memory beats this one. A point can be on the
    /// frontier without being maximal anywhere -- it merely has to be dominated
    /// by nobody -- so an empty list here is meaningful, not a bug.
    pub maximal_dimensions: Vec<&'static str>,
}

/// The dominance dimensions, all oriented so that HIGHER IS BETTER.
///
/// `age_days` is the one that has to be flipped: fresher knowledge is better, so
/// dominance uses negated age. Getting that backwards would invert the whole
/// frontier while still producing a plausible-looking answer, which is why the
/// orientation is stated here once and applied in exactly one function.
const SKYLINE_DIMENSIONS: [&str; 5] = [
    "onionLayer",
    "kTrussRank",
    "pprPercentile",
    "recency",
    "trustRank",
];

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
    sort_by_ulid_payload_or_lexical(&mut memories, |memory| memory.memory_id.as_str());

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
        let k_truss_rank = k_truss_ranks.get(&memory.memory_id).copied().unwrap_or(0);
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

    let frontier = pareto_frontier(&metrics_by_memory);

    KnowledgeSkyline {
        schema: KNOWLEDGE_SKYLINE_SCHEMA_V1,
        node_count: memories.len(),
        row_count: rows.len(),
        trust_class_count: trust_classes.len(),
        max_onion_layer: onion_layers.max_layer,
        frontier_size: frontier.len(),
        frontier,
        rows,
        communities: community_summaries(&input.graph, &metrics_by_memory),
    }
}

/// The five dominance coordinates for one memory, all higher-is-better.
///
/// Age is negated here and nowhere else, so the "fresher is better" orientation
/// lives in one line rather than being restated at each comparison.
fn dimension_vector(metrics: &MemoryMetrics) -> [f64; 5] {
    [
        metrics.onion_layer as f64,
        metrics.k_truss_rank as f64,
        finite_or_zero(metrics.ppr_percentile),
        -finite_or_zero(metrics.age_days),
        f64::from(crate::core::contradiction_detect::trust_class_rank(
            metrics.trust_class.as_str(),
        )),
    ]
}

/// Pareto dominance: `left` dominates `right` when it is at least as good on
/// EVERY dimension and strictly better on AT LEAST ONE.
///
/// Both halves matter. Without the second, two identical points would dominate
/// each other and the frontier would be empty; without the first, the relation
/// would just be "better somewhere", which is not dominance and would admit
/// nearly everything.
fn dominates(left: &[f64; 5], right: &[f64; 5]) -> bool {
    let mut strictly_better_somewhere = false;
    for (l, r) in left.iter().zip(right.iter()) {
        if l < r {
            return false;
        }
        if l > r {
            strictly_better_somewhere = true;
        }
    }
    strictly_better_somewhere
}

/// The skyline proper: every memory dominated by no other memory.
///
/// O(n^2) by construction. That is deliberate and adequate here -- this runs
/// over the memories already materialised for the posture grid, not over the
/// store -- and a divide-and-conquer skyline would trade readability for a
/// constant factor on an input this size.
fn pareto_frontier(
    metrics_by_memory: &BTreeMap<String, MemoryMetrics>,
) -> Vec<KnowledgeSkylineFrontierPoint> {
    let points: Vec<(&String, &MemoryMetrics, [f64; 5])> = metrics_by_memory
        .iter()
        .map(|(memory_id, metrics)| (memory_id, metrics, dimension_vector(metrics)))
        .collect();

    // A dimension's maxima, so `maximal_dimensions` reports a fact about the
    // whole population rather than about the comparison that happened to run last.
    let mut dimension_max = [f64::NEG_INFINITY; 5];
    for (_, _, vector) in &points {
        for (index, value) in vector.iter().enumerate() {
            if *value > dimension_max[index] {
                dimension_max[index] = *value;
            }
        }
    }

    let mut frontier = points
        .iter()
        .filter(|(_, _, candidate)| {
            !points
                .iter()
                .any(|(_, _, other)| dominates(other, candidate))
        })
        .map(|(memory_id, metrics, vector)| {
            let maximal_dimensions = SKYLINE_DIMENSIONS
                .iter()
                .enumerate()
                .filter(|(index, _)| vector[*index] >= dimension_max[*index])
                .map(|(_, name)| *name)
                .collect();
            KnowledgeSkylineFrontierPoint {
                memory_id: (*memory_id).clone(),
                trust_class: metrics.trust_class.clone(),
                trust_rank: crate::core::contradiction_detect::trust_class_rank(
                    metrics.trust_class.as_str(),
                ),
                onion_layer: metrics.onion_layer,
                k_truss_rank: metrics.k_truss_rank,
                ppr_percentile: finite_or_zero(metrics.ppr_percentile),
                age_days: finite_or_zero(metrics.age_days),
                maximal_dimensions,
            }
        })
        .collect::<Vec<_>>();

    sort_by_ulid_payload_or_lexical(&mut frontier, |point| point.memory_id.as_str());
    frontier
}

fn skyline_cell(matching: &[&MemoryMetrics], trust_class: &str) -> KnowledgeSkylineCell {
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
            sort_by_ulid_payload_or_lexical(&mut members, String::as_str);
            let layers = members
                .iter()
                .filter_map(|memory_id| metrics_by_memory.get(memory_id))
                .map(|metrics| metrics.onion_layer)
                .collect::<Vec<_>>();
            let onion_layer_min = layers.iter().copied().min().unwrap_or(0);
            let onion_layer_max = layers.iter().copied().max().unwrap_or(0);
            let midpoint = onion_layer_min + (onion_layer_max.saturating_sub(onion_layer_min) / 2);
            // Onion layers count outward from the periphery, so a higher layer is
            // more core. A community whose members all share a single layer (e.g.
            // a clique) is uniformly core, not periphery: without this guard the
            // `<= midpoint` predicate sweeps every member into `periphery_count`
            // (midpoint == min == max) and mislabels the densest possible core as
            // PeripheryHeavy. Multi-layer communities keep the normal split.
            let periphery_count = if onion_layer_min == onion_layer_max {
                0
            } else {
                layers.iter().filter(|layer| **layer <= midpoint).count()
            };
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
    sort_by_ulid_payload_or_lexical(&mut ranked, |(memory_id, _)| memory_id.as_str());
    ranked.sort_by(|left, right| left.1.total_cmp(&right.1));
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
    sort_by_ulid_payload_or_lexical(&mut ranked, |(memory_id, _)| memory_id.as_str());
    ranked.sort_by(|left, right| left.1.total_cmp(&right.1));
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

    #[test]
    fn skyline_same_metric_ties_accept_radix_memory_ids() {
        let first = "mem_01J0000000000000000000000A";
        let second = "mem_01J0000000000000000000000B";
        let third = "mem_01J0000000000000000000000C";

        let percentiles = ppr_percentiles(&ppr(&[(third, 0.5), (first, 0.5), (second, 0.5)]));
        assert_eq!(percentiles.get(first), Some(&0.0));
        assert_eq!(percentiles.get(second), Some(&0.5));
        assert_eq!(percentiles.get(third), Some(&1.0));

        let deciles = age_deciles(
            &[
                memory(third, "agent_assertion", 1),
                memory(first, "agent_assertion", 1),
                memory(second, "agent_assertion", 1),
            ],
            ts(16),
        );
        assert_eq!(deciles.get(first), Some(&0));
        assert_eq!(deciles.get(second), Some(&5));
        assert_eq!(deciles.get(third), Some(&9));
    }

    /// bd-pmgg0. THE test this change exists to pass: the Pareto frontier and the
    /// cell-mean grid must DISAGREE on a fixture, in both directions. A test that
    /// passes under the old grid-only implementation would prove nothing, because
    /// the complaint was never that the grid is wrong -- it is that a grid of
    /// means is not a skyline and cannot answer what a skyline answers.
    ///
    /// The fixture is a complete graph, so onion layer and k-truss are uniform
    /// and dominance turns on the three dimensions the test controls exactly:
    /// ppr percentile, recency, and trust rank. The trade-offs are deliberate --
    /// each memory is best at something, or dominated outright:
    ///   "0" human_explicit,  ppr 0.1 (worst),  oldest -> top trust only
    ///   "1" agent_assertion, ppr 0.9 (best),   middle -> top ppr only
    ///   "2" agent_validated, ppr 0.15,         newest -> top recency only
    ///   "3" agent_assertion, ppr 0.2,          old    -> beaten by "1" on every
    ///       dimension at equal trust, so it is DOMINATED and must not appear
    #[test]
    fn frontier_and_cell_mean_grid_disagree_in_both_directions() {
        let input = KnowledgeSkylineInput {
            graph: Graph::complete_graph(CompatibilityMode::Strict, 4),
            memories: vec![
                memory("0", "human_explicit", 1),
                memory("1", "agent_assertion", 8),
                memory("2", "agent_validated", 15),
                memory("3", "agent_assertion", 2),
            ],
            // Avoid a tied mean between the two-member assertion cell and
            // the single validated point: the former must rank higher.
            ppr_scores: ppr(&[("0", 0.1), ("1", 0.9), ("2", 0.15), ("3", 0.2)]),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);
        let on_frontier: BTreeSet<&str> = skyline
            .frontier
            .iter()
            .map(|point| point.memory_id.as_str())
            .collect();

        assert!(
            !on_frontier.is_empty(),
            "a populated store must have a non-empty frontier"
        );
        assert_eq!(skyline.frontier_size, skyline.frontier.len());

        // "3" is dominated by "1": same trust class, lower ppr, older. The grid
        // still counts it in a cell; the skyline must not list it.
        assert!(
            !on_frontier.contains("3"),
            "a memory beaten on every dimension must not be on the frontier, got {on_frontier:?}"
        );

        // The cell-mean view, read off the SAME output, so the two readings are
        // compared rather than asserted separately.
        let cells: Vec<&KnowledgeSkylineCell> = skyline
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .filter(|cell| cell.count > 0)
            .collect();
        assert!(!cells.is_empty(), "fixture produced no populated cells");
        let best_cell = cells
            .iter()
            .max_by(|left, right| left.ppr_percentile.total_cmp(&right.ppr_percentile))
            .expect("populated cells exist");
        let worst_cell = cells
            .iter()
            .min_by(|left, right| left.ppr_percentile.total_cmp(&right.ppr_percentile))
            .expect("populated cells exist");
        assert_ne!(
            best_cell.trust_class, worst_cell.trust_class,
            "fixture is inert: the grid's best and worst cells must differ"
        );

        // DIRECTION A: the grid's best-mean cell holds more than its frontier
        // member, so ranking cells by mean points a reader at a memory the
        // skyline excludes.
        assert!(
            best_cell.count > 1,
            "fixture is inert: the best-mean cell must contain a dominated memory too"
        );

        // DIRECTION B: a frontier member sits in the cell the grid ranks WORST.
        // "0" is the unique top of trust rank, so nothing dominates it, yet its
        // cell's mean ppr is the lowest. Averaging buries exactly the memory a
        // skyline exists to surface.
        let buried = skyline
            .frontier
            .iter()
            .find(|point| point.trust_class == worst_cell.trust_class)
            .unwrap_or_else(|| {
                panic!(
                    "fixture is inert: no frontier member in the worst-mean cell ({})",
                    worst_cell.trust_class
                )
            });
        assert!(
            buried.maximal_dimensions.contains(&"trustRank"),
            "the buried frontier member should be there on trust rank, got {:?}",
            buried.maximal_dimensions
        );

        println!(
            "frontier={on_frontier:?} best_mean_cell={} worst_mean_cell={}",
            best_cell.trust_class, worst_cell.trust_class
        );
    }

    /// bd-pmgg0. The defining property, checkable from the output alone: no
    /// frontier member may dominate another. If one did, the "frontier" would be
    /// something else wearing the name, which is the defect this bead is about.
    #[test]
    fn frontier_members_never_dominate_each_other() {
        let input = KnowledgeSkylineInput {
            graph: graph([("a", "b"), ("b", "c"), ("a", "c"), ("c", "d"), ("d", "e")]),
            memories: vec![
                memory("a", "human_explicit", 1),
                memory("b", "agent_validated", 4),
                memory("c", "agent_assertion", 7),
                memory("d", "peer_human_attested", 10),
                memory("e", "legacy_import", 14),
            ],
            ppr_scores: ppr(&[("a", 0.8), ("b", 0.4), ("c", 0.6), ("d", 0.2), ("e", 0.1)]),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);
        assert!(!skyline.frontier.is_empty(), "frontier must not be empty");

        let coordinates = |point: &KnowledgeSkylineFrontierPoint| {
            [
                point.onion_layer as f64,
                point.k_truss_rank as f64,
                point.ppr_percentile,
                -point.age_days,
                f64::from(point.trust_rank),
            ]
        };
        for left in &skyline.frontier {
            for right in &skyline.frontier {
                if left.memory_id == right.memory_id {
                    continue;
                }
                assert!(
                    !dominates(&coordinates(left), &coordinates(right)),
                    "{} dominates {} yet both are on the frontier",
                    left.memory_id,
                    right.memory_id
                );
            }
        }
    }

    /// bd-pmgg0. Controls for `dominates` itself, because a relation that
    /// answered `true` or `false` unconditionally would satisfy one direction of
    /// each test above and still look fine.
    #[test]
    fn dominance_requires_at_least_as_good_everywhere_and_strictly_better_once() {
        let base = [1.0, 1.0, 0.5, -3.0, 4.0];
        let better_once = [1.0, 1.0, 0.6, -3.0, 4.0];
        let worse_once = [1.0, 1.0, 0.4, -3.0, 4.0];
        let mixed = [2.0, 1.0, 0.4, -3.0, 4.0];

        assert!(dominates(&better_once, &base), "strictly better on one");
        assert!(!dominates(&base, &better_once), "dominance is asymmetric");
        assert!(!dominates(&base, &base), "a point cannot dominate itself");
        assert!(!dominates(&mixed, &base), "better on one, worse on another");
        assert!(!dominates(&base, &mixed), "and neither way round");
        assert!(dominates(&base, &worse_once), "worse on exactly one");

        // Recency orientation: fresher must beat older, all else equal. Getting
        // this backwards inverts the whole frontier while still looking plausible.
        let fresher = [1.0, 1.0, 0.5, -1.0, 4.0];
        assert!(
            dominates(&fresher, &base),
            "a fresher memory must dominate an older identical one"
        );
    }

    #[test]
    fn skyline_empty_graph_matches_golden_contract() {
        let input = KnowledgeSkylineInput {
            graph: Graph::new(CompatibilityMode::Strict),
            memories: Vec::new(),
            ppr_scores: BTreeMap::new(),
            as_of: ts(16),
        };

        let skyline = compute_knowledge_skyline(&input);
        let actual = serde_json::to_string_pretty(&skyline).expect("serialize skyline");
        let expected = include_str!("../../tests/golden/skyline.snap").trim_end();

        assert_eq!(actual, expected);
    }
}
