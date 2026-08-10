//! Link-prediction core for `ee graph suggest-links` (ADR 0066 §1 /
//! bd-3a1op.3).
//!
//! Pure over injected inputs (links, tags, PPR affinities, retrieval-affinity
//! edges, content snippets) so every predictor and the candidate bound are
//! unit-testable without a database. Graph signals go through `fnx`
//! primitives — no hand-rolled algorithms.
//!
//! Candidate generation is bounded: only unlinked pairs sharing a graph
//! neighbor, a tag, or a retrieval-affinity edge are scored, with per-node
//! neighbor lists and per-tag fan-out capped at [`NEIGHBOR_FANOUT_CAP`].
//! Worst-case cost is O(Σ deg²) over CAPPED lists — never O(n²) over the
//! corpus (the planted-trap unit test enforces this).

use std::collections::{BTreeMap, BTreeSet};

use fnx_classes::Graph;
use fnx_runtime::CompatibilityMode;

/// Blend weights (`[graph.suggest]`, ADR 0066 defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SuggestBlendWeights {
    pub adamic_adar: f64,
    pub jaccard_tags: f64,
    pub ppr: f64,
    pub affinity: f64,
    pub preferential_attachment: f64,
}

impl Default for SuggestBlendWeights {
    fn default() -> Self {
        Self {
            adamic_adar: 0.35,
            jaccard_tags: 0.20,
            ppr: 0.25,
            affinity: 0.15,
            preferential_attachment: 0.05,
        }
    }
}

/// Contradiction-typing thresholds (`[graph.suggest]`; precision over
/// recall applies doubly — a false contradiction costs reviewer trust).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContradictionThresholds {
    /// Token-Jaccard content similarity at or above which a pair is
    /// "about the same thing".
    pub similarity_threshold: f64,
}

impl Default for ContradictionThresholds {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.5,
        }
    }
}

/// Per-node neighbor and per-tag member cap for candidate generation.
pub const NEIGHBOR_FANOUT_CAP: usize = 64;

/// Typed suggestion relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedRelation {
    Related,
    Supports,
    Contradicts,
}

impl SuggestedRelation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Related => "related",
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
        }
    }
}

/// Raw per-signal values carried on every suggestion.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestSignals {
    pub adamic_adar: f64,
    pub jaccard_tags: f64,
    pub ppr: f64,
    pub affinity: f64,
    pub preferential_attachment: f64,
}

/// One suggested link.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedLink {
    pub memory_a: String,
    pub memory_b: String,
    /// Blended, batch-normalized score in [0, 1].
    pub score: f64,
    pub suggested_relation: SuggestedRelation,
    pub signals: SuggestSignals,
    /// One-line human-readable reason (top contributing signals).
    pub reason: String,
}

/// Pure inputs for one suggestion run.
#[derive(Clone, Debug, Default)]
pub struct SuggestLinksInput {
    /// Existing undirected memory links (order-insensitive pairs).
    pub links: Vec<(String, String)>,
    /// Memory tags.
    pub tags: BTreeMap<String, BTreeSet<String>>,
    /// Symmetrized PPR affinity per canonical pair (a < b); absent = 0.
    pub ppr: BTreeMap<(String, String), f64>,
    /// Decayed retrieval-affinity weights per canonical pair; `None` when
    /// the projection snapshot does not exist (cold).
    pub affinity: Option<BTreeMap<(String, String), f64>>,
    /// Content snippets for polarity typing.
    pub content: BTreeMap<String, String>,
    pub weights: SuggestBlendWeights,
    pub contradiction: ContradictionThresholds,
    pub limit: usize,
    pub min_score: f64,
}

/// Run result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SuggestLinksReport {
    pub suggestions: Vec<SuggestedLink>,
    pub candidate_count: usize,
    /// True when the affinity signal was honestly omitted (cold).
    pub affinity_cold: bool,
}

fn canonical_pair(left: &str, right: &str) -> (String, String) {
    if left < right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

fn token_set(text: &str) -> BTreeSet<String> {
    text.to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() > 2)
        .map(str::to_owned)
        .collect()
}

fn jaccard<T: Ord>(left: &BTreeSet<T>, right: &BTreeSet<T>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count() as f64;
    let union = left.union(right).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn min_max_normalize(values: &mut [f64]) {
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    for value in values.iter() {
        min = min.min(*value);
        max = max.max(*value);
    }
    let span = max - min;
    if !span.is_finite() || span < 1e-12 {
        for value in values.iter_mut() {
            *value = 0.0;
        }
        return;
    }
    for value in values.iter_mut() {
        *value = (*value - min) / span;
    }
}

/// Decide the typed relation for a candidate pair from its content.
fn typed_relation(
    content: &BTreeMap<String, String>,
    thresholds: ContradictionThresholds,
    memory_a: &str,
    memory_b: &str,
) -> SuggestedRelation {
    let (Some(text_a), Some(text_b)) = (content.get(memory_a), content.get(memory_b)) else {
        return SuggestedRelation::Related;
    };
    let similarity = jaccard(&token_set(text_a), &token_set(text_b));
    if similarity < thresholds.similarity_threshold {
        return SuggestedRelation::Related;
    }
    let negation_a = crate::core::ask::has_negation(text_a);
    let negation_b = crate::core::ask::has_negation(text_b);
    if negation_a != negation_b {
        SuggestedRelation::Contradicts
    } else {
        SuggestedRelation::Supports
    }
}

/// Bounded candidate generation (public so the CLI can pre-compute PPR
/// seeds for exactly the pairs that will be scored). Deterministic order.
#[must_use]
pub fn generate_candidate_pairs(input: &SuggestLinksInput) -> Vec<(String, String)> {
    let (_, _, candidates) = bounded_candidates(input);
    candidates
}

fn bounded_candidates(
    input: &SuggestLinksInput,
) -> (
    BTreeSet<(String, String)>,
    BTreeMap<String, Vec<String>>,
    Vec<(String, String)>,
) {
    let mut linked: BTreeSet<(String, String)> = BTreeSet::new();
    let mut neighbors: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (left, right) in &input.links {
        if left == right {
            continue;
        }
        linked.insert(canonical_pair(left, right));
        neighbors.entry(left.as_str()).or_default().insert(right);
        neighbors.entry(right.as_str()).or_default().insert(left);
    }
    let capped_neighbors: BTreeMap<String, Vec<String>> = neighbors
        .iter()
        .map(|(node, set)| {
            (
                (*node).to_owned(),
                set.iter()
                    .copied()
                    .take(NEIGHBOR_FANOUT_CAP)
                    .map(str::to_owned)
                    .collect(),
            )
        })
        .collect();

    let mut candidates: BTreeSet<(String, String)> = BTreeSet::new();
    for adjacent in capped_neighbors.values() {
        for (left_index, left) in adjacent.iter().enumerate() {
            for right in adjacent.iter().skip(left_index + 1) {
                let pair = canonical_pair(left, right);
                if !linked.contains(&pair) {
                    candidates.insert(pair);
                }
            }
        }
    }
    let mut tag_members: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (memory_id, tags) in &input.tags {
        for tag in tags {
            tag_members.entry(tag.as_str()).or_default().push(memory_id);
        }
    }
    for members in tag_members.values() {
        // A tag shared by more members than the fan-out cap is a broad
        // category, not pair evidence: skipping it entirely is what keeps
        // the planted O(n²) trap fixture cheap.
        if members.len() > NEIGHBOR_FANOUT_CAP {
            continue;
        }
        for (left_index, left) in members.iter().enumerate() {
            for right in members.iter().skip(left_index + 1) {
                let pair = canonical_pair(left, right);
                if pair.0 != pair.1 && !linked.contains(&pair) {
                    candidates.insert(pair);
                }
            }
        }
    }
    if let Some(affinity) = &input.affinity {
        for pair in affinity.keys() {
            if !linked.contains(pair) {
                candidates.insert(pair.clone());
            }
        }
    }
    let ordered: Vec<(String, String)> = candidates.iter().cloned().collect();
    (linked, capped_neighbors, ordered)
}

/// Generate bounded candidates and score them with the blended predictor
/// suite. Deterministic: ordering is score desc, then pair id.
#[must_use]
pub fn suggest_links(input: &SuggestLinksInput) -> SuggestLinksReport {
    let (_linked, capped_neighbors, candidate_pairs) = bounded_candidates(input);
    let candidate_count = candidate_pairs.len();
    if candidate_pairs.is_empty() {
        return SuggestLinksReport {
            affinity_cold: input.affinity.is_none(),
            ..SuggestLinksReport::default()
        };
    }

    // ── graph signals via fnx over the undirected link graph ─────────────
    let mut graph = Graph::new(CompatibilityMode::Strict);
    for (node, adjacent) in &capped_neighbors {
        graph.add_node(node.as_str());
        for neighbor in adjacent {
            let _ = graph.add_edge(node.as_str(), neighbor.as_str());
        }
    }
    for (left, right) in &candidate_pairs {
        graph.add_node(left.as_str());
        graph.add_node(right.as_str());
    }
    let ebunch: Vec<(String, String)> = candidate_pairs.clone();
    let aa_scores = fnx_algorithms::adamic_adar_index(&graph, &ebunch);
    let pa_scores = fnx_algorithms::preferential_attachment(&graph, &ebunch);

    // ── raw signal vectors in candidate order ────────────────────────────
    let empty_tags = BTreeSet::new();
    let mut aa: Vec<f64> = aa_scores.iter().map(|(_, _, score)| *score).collect();
    let mut pa: Vec<f64> = pa_scores.iter().map(|(_, _, score)| *score).collect();
    let mut jaccard_tags: Vec<f64> = candidate_pairs
        .iter()
        .map(|(left, right)| {
            jaccard(
                input.tags.get(left).unwrap_or(&empty_tags),
                input.tags.get(right).unwrap_or(&empty_tags),
            )
        })
        .collect();
    let mut ppr: Vec<f64> = candidate_pairs
        .iter()
        .map(|pair| input.ppr.get(pair).copied().unwrap_or(0.0))
        .collect();
    let mut affinity: Vec<f64> = candidate_pairs
        .iter()
        .map(|pair| {
            input
                .affinity
                .as_ref()
                .and_then(|edges| edges.get(pair))
                .copied()
                .unwrap_or(0.0)
        })
        .collect();

    let raw = (
        aa.clone(),
        jaccard_tags.clone(),
        ppr.clone(),
        affinity.clone(),
        pa.clone(),
    );
    min_max_normalize(&mut aa);
    min_max_normalize(&mut jaccard_tags);
    min_max_normalize(&mut ppr);
    min_max_normalize(&mut affinity);
    min_max_normalize(&mut pa);

    let affinity_cold = input.affinity.is_none();
    let affinity_weight = if affinity_cold {
        0.0
    } else {
        input.weights.affinity
    };

    let mut suggestions: Vec<SuggestedLink> = candidate_pairs
        .iter()
        .enumerate()
        .map(|(index, (memory_a, memory_b))| {
            let score = input.weights.adamic_adar * aa[index]
                + input.weights.jaccard_tags * jaccard_tags[index]
                + input.weights.ppr * ppr[index]
                + affinity_weight * affinity[index]
                + input.weights.preferential_attachment * pa[index];
            let signals = SuggestSignals {
                adamic_adar: raw.0[index],
                jaccard_tags: raw.1[index],
                ppr: raw.2[index],
                affinity: raw.3[index],
                preferential_attachment: raw.4[index],
            };
            let relation = typed_relation(&input.content, input.contradiction, memory_a, memory_b);
            let mut contributions = [
                (
                    "shared-neighbor structure",
                    aa[index] * input.weights.adamic_adar,
                ),
                (
                    "tag overlap",
                    jaccard_tags[index] * input.weights.jaccard_tags,
                ),
                ("walk affinity", ppr[index] * input.weights.ppr),
                ("retrieval co-occurrence", affinity[index] * affinity_weight),
                (
                    "popularity prior",
                    pa[index] * input.weights.preferential_attachment,
                ),
            ];
            contributions.sort_by(|left, right| {
                right
                    .1
                    .partial_cmp(&left.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let reason = format!(
                "{} between {memory_a} and {memory_b} (driven by {}, {})",
                relation.as_str(),
                contributions[0].0,
                contributions[1].0,
            );
            SuggestedLink {
                memory_a: memory_a.clone(),
                memory_b: memory_b.clone(),
                score,
                suggested_relation: relation,
                signals,
                reason,
            }
        })
        .filter(|suggestion| suggestion.score >= input.min_score)
        .collect();

    suggestions.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| (&left.memory_a, &left.memory_b).cmp(&(&right.memory_a, &right.memory_b)))
    });
    if input.limit > 0 {
        suggestions.truncate(input.limit);
    }

    SuggestLinksReport {
        suggestions,
        candidate_count,
        affinity_cold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags_for(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        pairs
            .iter()
            .map(|(memory_id, tags)| {
                (
                    (*memory_id).to_owned(),
                    tags.iter().map(|tag| (*tag).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn adamic_adar_matches_hand_computation_on_a_micro_graph() {
        // a-c, b-c: candidate (a,b) shares exactly one neighbor c with
        // degree 2 -> AA = 1/ln(2).
        let input = SuggestLinksInput {
            links: vec![
                ("mem_a".to_owned(), "mem_c".to_owned()),
                ("mem_b".to_owned(), "mem_c".to_owned()),
            ],
            limit: 10,
            ..SuggestLinksInput::default()
        };
        let report = suggest_links(&input);
        assert_eq!(report.candidate_count, 1);
        let suggestion = &report.suggestions[0];
        assert_eq!(
            (suggestion.memory_a.as_str(), suggestion.memory_b.as_str()),
            ("mem_a", "mem_b")
        );
        let expected = 1.0 / 2f64.ln();
        assert!(
            (suggestion.signals.adamic_adar - expected).abs() < 1e-9,
            "AA raw signal must be 1/ln 2, got {}",
            suggestion.signals.adamic_adar
        );
    }

    #[test]
    fn broad_tags_are_excluded_by_the_fanout_cap() {
        // Planted O(n²) trap: one tag shared by 200 memories would produce
        // ~20k pairs; the cap must skip it entirely.
        let mut tag_pairs: Vec<(String, Vec<&str>)> = Vec::new();
        let ids: Vec<String> = (0..200)
            .map(|index| format!("mem_broad{index:03}"))
            .collect();
        for id in &ids {
            tag_pairs.push((id.clone(), vec!["everything"]));
        }
        let tags = tag_pairs
            .into_iter()
            .map(|(id, tags)| (id, tags.into_iter().map(str::to_owned).collect()))
            .collect();
        let input = SuggestLinksInput {
            tags,
            limit: 1000,
            ..SuggestLinksInput::default()
        };
        let report = suggest_links(&input);
        assert_eq!(
            report.candidate_count, 0,
            "a broad tag past the fan-out cap must contribute no candidates"
        );
    }

    #[test]
    fn contradiction_typing_requires_similarity_and_opposed_polarity() {
        let mut content = BTreeMap::new();
        content.insert(
            "mem_x".to_owned(),
            "always run the schema drift gate before altering public json".to_owned(),
        );
        content.insert(
            "mem_y".to_owned(),
            "never run the schema drift gate before altering public json".to_owned(),
        );
        content.insert(
            "mem_z".to_owned(),
            "always run the schema drift gate before altering public json today".to_owned(),
        );
        let input = SuggestLinksInput {
            tags: tags_for(&[
                ("mem_x", &["gate"]),
                ("mem_y", &["gate"]),
                ("mem_z", &["gate"]),
            ]),
            content,
            limit: 10,
            ..SuggestLinksInput::default()
        };
        let report = suggest_links(&input);
        let relation_for = |a: &str, b: &str| {
            report
                .suggestions
                .iter()
                .find(|s| s.memory_a == a && s.memory_b == b)
                .map(|s| s.suggested_relation)
        };
        assert_eq!(
            relation_for("mem_x", "mem_y"),
            Some(SuggestedRelation::Contradicts),
            "same subject, opposed polarity -> contradicts"
        );
        assert_eq!(
            relation_for("mem_x", "mem_z"),
            Some(SuggestedRelation::Supports),
            "same subject, same polarity -> supports"
        );
    }

    #[test]
    fn affinity_cold_is_honest_and_zero_weighted() {
        let input = SuggestLinksInput {
            links: vec![
                ("mem_a".to_owned(), "mem_c".to_owned()),
                ("mem_b".to_owned(), "mem_c".to_owned()),
            ],
            affinity: None,
            limit: 10,
            ..SuggestLinksInput::default()
        };
        let report = suggest_links(&input);
        assert!(report.affinity_cold);
        assert!(
            report.suggestions.iter().all(|s| s.signals.affinity == 0.0),
            "cold affinity contributes nothing"
        );
    }

    #[test]
    fn ordering_is_deterministic_score_then_pair_id() {
        let input = SuggestLinksInput {
            tags: tags_for(&[("mem_a", &["t1"]), ("mem_b", &["t1"]), ("mem_c", &["t1"])]),
            limit: 10,
            ..SuggestLinksInput::default()
        };
        let first = suggest_links(&input);
        let second = suggest_links(&input);
        assert_eq!(first, second, "identical inputs -> identical report");
        let pairs: Vec<(&str, &str)> = first
            .suggestions
            .iter()
            .map(|s| (s.memory_a.as_str(), s.memory_b.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("mem_a", "mem_b"), ("mem_a", "mem_c"), ("mem_b", "mem_c")],
            "equal scores fall back to pair-id order"
        );
    }
}
