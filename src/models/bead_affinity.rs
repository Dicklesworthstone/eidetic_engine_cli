//! Pure-policy bead-affinity scoring for swarm-aware retrieval
//! (bd-2942u, swarmx.barp).
//!
//! When multiple agents are claiming different beads in a swarm, every
//! agent contends for the same hot memory subset because retrieval
//! scoring is bead-agnostic. Bead-affinity adds a small additive prior
//! to a memory's score whenever the memory's curated signals overlap
//! with the active bead's label / title / description tokens or its
//! family link graph.
//!
//! This module is a strict pure-policy module — callers supply already
//! redacted token sets and link-target ids, and receive a deterministic
//! [`BeadAffinityExplanation`] capped at [`BEAD_AFFINITY_BIAS_CAP`]. No file
//! I/O, database access, or environment reads happen here. Caller
//! wiring into `ee context`, `ee search`, `ee why`, and `ee pack build`
//! is left to follow-up child slices; this slice ships the scoring
//! contract plus the closed-set vocabulary the schema and `ee why`
//! renderer consume.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Schema identifier for the `scoreComponents.beadAffinity` block.
pub const BEAD_AFFINITY_SCHEMA_V1: &str = "ee.context.bead_affinity.v1";

/// Stable degraded code emitted when the active bead has no overlapping
/// signal across any candidate memory; the renderer surfaces this to
/// agents so they can distinguish "no relevant memory yet" from "memory
/// failed to load".
pub const BEAD_AFFINITY_COLD_START_CODE: &str = "bead_affinity_cold_start";

/// Stable degraded code emitted when bead context could not be loaded
/// (beads.jsonl missing, unreadable, or schema mismatch) so retrieval
/// falls back to the unbiased baseline. Pinned by
/// `docs/schemas/ee.context.bead_affinity.v1.json#/$defs/degradation`.
pub const BEAD_AFFINITY_UNAVAILABLE_CODE: &str = "bead_affinity_unavailable";

/// Stable degraded code emitted when at least one memory's computed
/// weight saturated the global cap, signalling that the cap is biting.
pub const BEAD_AFFINITY_CAPPED_CODE: &str = "bead_affinity_capped";

/// Stable degraded code emitted when the bead loader failed to resolve
/// the requested `--bead-id` (typo, deleted bead, network-only lookup
/// path) so bead-affinity was skipped entirely for the request.
pub const BEAD_AFFINITY_LOOKUP_FAILED_CODE: &str = "bead_affinity_lookup_failed";

/// All four degraded codes the schema enumerates, in stable order. The
/// renderer cycles through this slice when emitting the
/// `scoreComponents.beadAffinity.degraded` array.
pub const BEAD_AFFINITY_DEGRADED_CODES: &[&str] = &[
    BEAD_AFFINITY_COLD_START_CODE,
    BEAD_AFFINITY_UNAVAILABLE_CODE,
    BEAD_AFFINITY_CAPPED_CODE,
    BEAD_AFFINITY_LOOKUP_FAILED_CODE,
];

/// Maximum absolute weight a bead-affinity score may apply, matching
/// the swarmx.5 agent-profile cap so the two priors compose cleanly.
pub const BEAD_AFFINITY_BIAS_CAP: f64 = 0.05;

/// Maximum absolute contribution per individual component, so a memory
/// that matches every component still respects the global cap.
pub const BEAD_AFFINITY_COMPONENT_CAP: f64 = BEAD_AFFINITY_BIAS_CAP / 4.0;

/// Closed-set vocabulary for the per-component breakdown surfaced via
/// `ee why`. Adding a kind requires updating the schema and every
/// renderer; the closed set keeps the explanation deterministic.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BeadAffinityComponentKind {
    TagOverlap,
    TitleToken,
    DescriptionToken,
    LinkPeer,
}

impl BeadAffinityComponentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TagOverlap => "tag_overlap",
            Self::TitleToken => "title_token",
            Self::DescriptionToken => "description_token",
            Self::LinkPeer => "link_peer",
        }
    }
}

/// Caller-curated bead context. Tokens are expected to be already
/// normalised (lower-cased, punctuation stripped) by the upstream bead
/// loader so this module can stay byte-stable across hosts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BeadAffinityBead {
    /// Stable beads.jsonl id, surfaced through to `ee why` evidence.
    pub bead_id: String,
    /// Normalised label tokens drawn from the bead's `labels` array.
    pub label_tokens: BTreeSet<String>,
    /// Normalised title token set (single space split, lower-case).
    pub title_tokens: BTreeSet<String>,
    /// Normalised description token set.
    pub description_tokens: BTreeSet<String>,
    /// Memory ids that already cite this bead family (parent/child or
    /// label-sibling), used to seed the LinkPeer component.
    pub family_memory_ids: BTreeSet<String>,
}

/// Caller-curated memory signals against which the bead is scored.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BeadAffinityMemory {
    /// Stable memory id, surfaced in the per-memory breakdown.
    pub memory_id: String,
    /// Normalised tag tokens drawn from the memory's `tags` array.
    pub tag_tokens: BTreeSet<String>,
    /// Normalised content token set (title + first content snippet).
    pub content_tokens: BTreeSet<String>,
    /// Memory ids this memory links to via `links[]`.
    pub link_target_memory_ids: BTreeSet<String>,
}

/// Per-component breakdown surfaced through `scoreComponents.beadAffinity`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadAffinityComponent {
    pub kind: BeadAffinityComponentKind,
    pub weight: f64,
    pub overlap_count: u32,
}

/// Top-level score plus breakdown for one (bead, memory) pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeadAffinityExplanation {
    pub schema: &'static str,
    pub bead_id: String,
    pub memory_id: String,
    pub weight: f64,
    pub cold_start: bool,
    pub components: Vec<BeadAffinityComponent>,
}

/// Stop-word set for bead-token normalisation. Drawn from the
/// agent-profile and search-side token normalisers so the bead loader
/// produces token sets compatible with both BeadAffinityScore (peer)
/// and BeadAffinityExplanation (this module) without each call site
/// re-deriving its own list.
const BEAD_AFFINITY_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "if",
    "in", "into", "is", "it", "its", "of", "on", "or", "that", "the", "this", "to", "was", "were",
    "will", "with",
];

/// Minimum token length kept after normalisation. Anything shorter is
/// treated as noise.
const BEAD_AFFINITY_MIN_TOKEN_LEN: usize = 2;

/// Normalise a free-text bead surface (title or description) into the
/// token set expected by [`BeadAffinityBead::title_tokens`] /
/// `description_tokens`. The transformation is:
///
/// 1. Lower-case via the ASCII path (non-ASCII passes through
///    unchanged — beads are constrained to ASCII titles per
///    `.beads/issues.jsonl`).
/// 2. Replace any character that is not alphanumeric with a single
///    space so punctuation does not bind to adjacent tokens.
/// 3. Split on whitespace, drop tokens shorter than
///    [`BEAD_AFFINITY_MIN_TOKEN_LEN`], drop tokens in
///    [`BEAD_AFFINITY_STOP_WORDS`].
/// 4. Collect into a `BTreeSet<String>` so the score is
///    order-independent.
///
/// Pure: no allocations beyond the returned `BTreeSet` and a single
/// scratch `String` for normalisation.
#[must_use]
pub fn normalize_bead_text_tokens(text: &str) -> BTreeSet<String> {
    let mut scratch = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            scratch.push(ch.to_ascii_lowercase());
        } else {
            scratch.push(' ');
        }
    }

    let stop_words: BTreeSet<&'static str> = BEAD_AFFINITY_STOP_WORDS.iter().copied().collect();
    scratch
        .split_whitespace()
        .filter(|token| token.len() >= BEAD_AFFINITY_MIN_TOKEN_LEN)
        .filter(|token| !stop_words.contains(*token))
        .map(str::to_owned)
        .collect()
}

/// Normalise a label-style bead surface (`labels[]` entries are
/// already short, lower-case, punctuation-separated tokens in the
/// source jsonl, e.g. `degraded-codes`, `implements-surface:search`,
/// `swarm-scale`). This helper treats every non-ASCII-alphanumeric
/// character as a separator so namespaced labels keep their searchable
/// segments and stay aligned with the search-side bead-affinity
/// tokenizer.
#[must_use]
pub fn normalize_bead_label_tokens(label: &str) -> BTreeSet<String> {
    label
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= BEAD_AFFINITY_MIN_TOKEN_LEN)
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

/// Compute the bead-affinity weight for a single `(bead, memory)` pair.
///
/// The result is a sum of capped per-component contributions, then
/// clamped globally to `[-BEAD_AFFINITY_BIAS_CAP, BEAD_AFFINITY_BIAS_CAP]`.
/// When every component contributes zero, `cold_start` is set and the
/// caller is expected to emit [`BEAD_AFFINITY_COLD_START_CODE`] in the
/// `ee why` degraded block.
#[must_use]
pub fn explain_bead_affinity(
    bead: &BeadAffinityBead,
    memory: &BeadAffinityMemory,
) -> BeadAffinityExplanation {
    let tag_overlap_count = bead.label_tokens.intersection(&memory.tag_tokens).count();
    let title_overlap_count = bead
        .title_tokens
        .intersection(&memory.content_tokens)
        .count();
    let description_overlap_count = bead
        .description_tokens
        .intersection(&memory.content_tokens)
        .count();
    let link_peer_count = {
        let mut count = bead
            .family_memory_ids
            .intersection(&memory.link_target_memory_ids)
            .count();
        if bead.family_memory_ids.contains(&memory.memory_id) {
            count += 1;
        }
        count
    };

    let raw_components: BTreeMap<BeadAffinityComponentKind, (f64, u32)> = [
        (
            BeadAffinityComponentKind::TagOverlap,
            component_weight(tag_overlap_count),
            tag_overlap_count,
        ),
        (
            BeadAffinityComponentKind::TitleToken,
            component_weight(title_overlap_count),
            title_overlap_count,
        ),
        (
            BeadAffinityComponentKind::DescriptionToken,
            component_weight(description_overlap_count),
            description_overlap_count,
        ),
        (
            BeadAffinityComponentKind::LinkPeer,
            component_weight(link_peer_count),
            link_peer_count,
        ),
    ]
    .into_iter()
    .map(|(kind, weight, overlap)| (kind, (weight, u32::try_from(overlap).unwrap_or(u32::MAX))))
    .collect();

    let total_overlap: u32 = raw_components.values().map(|(_, count)| *count).sum();
    let cold_start = total_overlap == 0;

    let components: Vec<BeadAffinityComponent> = raw_components
        .into_iter()
        .filter(|(_, (weight, overlap))| *overlap > 0 || *weight != 0.0)
        .map(|(kind, (weight, overlap_count))| BeadAffinityComponent {
            kind,
            weight,
            overlap_count,
        })
        .collect();

    let raw_weight: f64 = components.iter().map(|component| component.weight).sum();
    let weight = if raw_weight.is_nan() {
        0.0
    } else {
        raw_weight.clamp(-BEAD_AFFINITY_BIAS_CAP, BEAD_AFFINITY_BIAS_CAP)
    };

    BeadAffinityExplanation {
        schema: BEAD_AFFINITY_SCHEMA_V1,
        bead_id: bead.bead_id.clone(),
        memory_id: memory.memory_id.clone(),
        weight,
        cold_start,
        components,
    }
}

fn component_weight(overlap_count: usize) -> f64 {
    if overlap_count == 0 {
        0.0
    } else {
        let raw = BEAD_AFFINITY_COMPONENT_CAP
            * f64::from(u32::try_from(overlap_count).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(overlap_count).unwrap_or(u32::MAX)).max(1.0);
        // `raw` is a flat per-active-component contribution at the cap;
        // saturating ensures we never exceed the cap even if a future
        // refactor changes the per-token gain shape.
        raw.clamp(-BEAD_AFFINITY_COMPONENT_CAP, BEAD_AFFINITY_COMPONENT_CAP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_set<I: IntoIterator<Item = &'static str>>(values: I) -> BTreeSet<String> {
        values.into_iter().map(str::to_owned).collect()
    }

    fn baseline_bead() -> BeadAffinityBead {
        BeadAffinityBead {
            bead_id: "bd-2942u".to_owned(),
            label_tokens: token_set(["swarmx", "retrieval", "context"]),
            title_tokens: token_set(["bead", "aware", "retrieval", "prioritization"]),
            description_tokens: token_set(["agent", "swarm", "context", "score"]),
            family_memory_ids: token_set(["mem_family_1", "mem_family_2"]),
        }
    }

    #[test]
    fn cold_start_when_no_component_overlaps() {
        let bead = baseline_bead();
        let memory = BeadAffinityMemory {
            memory_id: "mem_other".to_owned(),
            tag_tokens: token_set(["unrelated"]),
            content_tokens: token_set(["nothing", "matches"]),
            link_target_memory_ids: token_set(["mem_other2"]),
        };
        let score = explain_bead_affinity(&bead, &memory);
        assert!(score.cold_start, "expected cold_start when no overlap");
        assert_eq!(score.weight, 0.0);
        assert!(
            score.components.is_empty(),
            "no components for cold_start memory"
        );
    }

    #[test]
    fn weight_is_capped_at_global_bias() {
        let bead = baseline_bead();
        let memory = BeadAffinityMemory {
            memory_id: "mem_family_1".to_owned(),
            tag_tokens: token_set(["swarmx", "retrieval", "context"]),
            content_tokens: token_set([
                "bead",
                "aware",
                "retrieval",
                "prioritization",
                "agent",
                "swarm",
                "context",
                "score",
            ]),
            link_target_memory_ids: token_set(["mem_family_1", "mem_family_2"]),
        };
        let score = explain_bead_affinity(&bead, &memory);
        assert!(
            !score.cold_start,
            "must not be cold_start when every component overlaps"
        );
        assert!(
            score.weight <= BEAD_AFFINITY_BIAS_CAP + f64::EPSILON,
            "weight {} exceeded cap {}",
            score.weight,
            BEAD_AFFINITY_BIAS_CAP
        );
        assert!(
            score.weight >= -BEAD_AFFINITY_BIAS_CAP - f64::EPSILON,
            "weight {} fell below -cap {}",
            score.weight,
            -BEAD_AFFINITY_BIAS_CAP
        );
        for component in &score.components {
            assert!(
                component.weight.abs() <= BEAD_AFFINITY_COMPONENT_CAP + f64::EPSILON,
                "component {:?} weight {} exceeded per-component cap {}",
                component.kind,
                component.weight,
                BEAD_AFFINITY_COMPONENT_CAP
            );
        }
    }

    #[test]
    fn tag_overlap_alone_produces_positive_weight() {
        let bead = baseline_bead();
        let memory = BeadAffinityMemory {
            memory_id: "mem_tag".to_owned(),
            tag_tokens: token_set(["swarmx"]),
            content_tokens: token_set(["unrelated"]),
            link_target_memory_ids: BTreeSet::new(),
        };
        let score = explain_bead_affinity(&bead, &memory);
        assert!(!score.cold_start);
        assert!(
            score.weight > 0.0,
            "tag overlap must produce positive weight"
        );
        assert_eq!(score.components.len(), 1);
        assert_eq!(
            score.components[0].kind,
            BeadAffinityComponentKind::TagOverlap
        );
        assert_eq!(score.components[0].overlap_count, 1);
    }

    #[test]
    fn link_peer_includes_self_membership_in_family() {
        let bead = baseline_bead();
        let memory = BeadAffinityMemory {
            memory_id: "mem_family_1".to_owned(),
            tag_tokens: BTreeSet::new(),
            content_tokens: BTreeSet::new(),
            link_target_memory_ids: BTreeSet::new(),
        };
        let score = explain_bead_affinity(&bead, &memory);
        assert!(!score.cold_start);
        let link_component = score
            .components
            .iter()
            .find(|component| component.kind == BeadAffinityComponentKind::LinkPeer)
            .expect("link_peer component when memory is part of bead family");
        assert!(link_component.overlap_count >= 1);
        assert!(link_component.weight > 0.0);
    }

    #[test]
    fn scoring_is_deterministic_across_repeat_calls() {
        let bead = baseline_bead();
        let memory = BeadAffinityMemory {
            memory_id: "mem_det".to_owned(),
            tag_tokens: token_set(["context", "retrieval"]),
            content_tokens: token_set(["bead", "score", "swarm"]),
            link_target_memory_ids: token_set(["mem_family_2"]),
        };
        let first = serde_json::to_string(&explain_bead_affinity(&bead, &memory))
            .expect("first score must serialize");
        let second = serde_json::to_string(&explain_bead_affinity(&bead, &memory))
            .expect("second score must serialize");
        assert_eq!(
            first, second,
            "bead-affinity JSON must be byte-equal across repeated calls"
        );
    }

    #[test]
    fn schema_and_kind_codes_are_stable_lower_snake() {
        let bead = baseline_bead();
        let memory = BeadAffinityMemory {
            memory_id: "mem_codes".to_owned(),
            tag_tokens: token_set(["swarmx"]),
            content_tokens: token_set(["bead"]),
            link_target_memory_ids: token_set(["mem_family_1"]),
        };
        let score = explain_bead_affinity(&bead, &memory);
        assert_eq!(score.schema, "ee.context.bead_affinity.v1");

        for kind in [
            BeadAffinityComponentKind::TagOverlap,
            BeadAffinityComponentKind::TitleToken,
            BeadAffinityComponentKind::DescriptionToken,
            BeadAffinityComponentKind::LinkPeer,
        ] {
            let code = kind.as_str();
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "kind code {} must be lower_snake_case",
                code
            );
            assert!(
                !code.starts_with('_') && !code.ends_with('_'),
                "kind code {} must not start/end with underscore",
                code
            );
        }
    }

    #[test]
    fn cold_start_constant_matches_documented_string() {
        assert_eq!(BEAD_AFFINITY_COLD_START_CODE, "bead_affinity_cold_start");
        assert_eq!(BEAD_AFFINITY_BIAS_CAP, 0.05);
        assert_eq!(BEAD_AFFINITY_COMPONENT_CAP, 0.0125);
    }

    #[test]
    fn text_tokenizer_lowercases_strips_punctuation_and_drops_stopwords() {
        let tokens =
            normalize_bead_text_tokens("Wire aggregate_degraded helper into all renderers.");
        // "into" is a stopword; "aggregate_degraded" gets split on _;
        // single-char "a" is below minimum length.
        let expected: BTreeSet<String> = [
            "wire",
            "aggregate",
            "degraded",
            "helper",
            "all",
            "renderers",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(tokens, expected);
    }

    #[test]
    fn text_tokenizer_is_deterministic_and_order_independent() {
        let first = normalize_bead_text_tokens("Bead-aware retrieval prioritization in ee context");
        let second =
            normalize_bead_text_tokens("ee context in prioritization retrieval Bead-aware");
        assert_eq!(
            first, second,
            "tokeniser must be order-independent across two inputs that contain the same tokens"
        );
    }

    #[test]
    fn label_tokenizer_splits_dashes_and_underscores_then_lowercases() {
        let tokens = normalize_bead_label_tokens("swarm-scale_RETRIEVAL idea-wizard");
        let expected: BTreeSet<String> = ["swarm", "scale", "retrieval", "idea", "wizard"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(tokens, expected);
    }

    #[test]
    fn label_tokenizer_drops_short_segments() {
        let tokens = normalize_bead_label_tokens("a-bcd-e-fghi");
        let expected: BTreeSet<String> = ["bcd", "fghi"].into_iter().map(str::to_owned).collect();
        assert_eq!(tokens, expected);
    }

    #[test]
    fn label_tokenizer_splits_namespaced_beads_labels() {
        let tokens = normalize_bead_label_tokens("implements-surface:query-file-tags");
        let expected: BTreeSet<String> = ["implements", "surface", "query", "file", "tags"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(tokens, expected);
    }

    #[test]
    fn normalisers_feed_explain_bead_affinity_without_extra_work() {
        // Round-trip: build a bead from real-shaped strings via the
        // normalisers, then prove the explainer picks up at least one
        // component overlap with a memory whose tags share a label token.
        let bead = BeadAffinityBead {
            bead_id: "bd-2942u".to_owned(),
            label_tokens: normalize_bead_label_tokens("swarmx retrieval context"),
            title_tokens: normalize_bead_text_tokens(
                "swarmx.barp: bead-aware retrieval prioritization",
            ),
            description_tokens: normalize_bead_text_tokens(
                "Bias ee context retrieval scoring with active bead labels and title tokens.",
            ),
            family_memory_ids: BTreeSet::new(),
        };
        let memory = BeadAffinityMemory {
            memory_id: "mem_norm".to_owned(),
            tag_tokens: ["retrieval".to_owned()].into_iter().collect(),
            content_tokens: ["bead".to_owned(), "scoring".to_owned()]
                .into_iter()
                .collect(),
            link_target_memory_ids: BTreeSet::new(),
        };
        let score = explain_bead_affinity(&bead, &memory);
        assert!(
            !score.cold_start,
            "expected overlap via tag retrieval token"
        );
        assert!(
            score.weight > 0.0,
            "expected positive weight from normaliser output"
        );
    }

    #[test]
    fn degraded_codes_match_schema_pinned_vocabulary() {
        // Mirrors the closed set pinned by
        // docs/schemas/ee.context.bead_affinity.v1.json and verified by
        // tests/bead_affinity_schema_unit.rs::degradation_code_enum_lists_documented_codes.
        let expected: BTreeSet<&'static str> = [
            "bead_affinity_cold_start",
            "bead_affinity_unavailable",
            "bead_affinity_capped",
            "bead_affinity_lookup_failed",
        ]
        .into_iter()
        .collect();
        let actual: BTreeSet<&'static str> = BEAD_AFFINITY_DEGRADED_CODES.iter().copied().collect();
        assert_eq!(
            actual, expected,
            "bead-affinity degraded code closed set drifted from schema vocabulary"
        );

        assert_eq!(BEAD_AFFINITY_UNAVAILABLE_CODE, "bead_affinity_unavailable");
        assert_eq!(BEAD_AFFINITY_CAPPED_CODE, "bead_affinity_capped");
        assert_eq!(
            BEAD_AFFINITY_LOOKUP_FAILED_CODE,
            "bead_affinity_lookup_failed"
        );
    }
}
