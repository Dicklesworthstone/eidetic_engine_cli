//! Query-ranked admission for the bounded extractive ask engine.
//!
//! The bound belongs after relevance scoring, not before it: a caller's
//! database order must not decide whether an answer exists. Identity checking
//! happens before admission, so the limit cannot hide inconsistent copies of
//! the same source. This module never expands the caller's workspace scope.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use super::{
    AskCandidate, AskContradiction, AskRequest, SpanScorer, has_negation, same_conflict_topic,
    score_span, segment_spans, tokenize_for_ask, trust_tilt,
};

#[path = "ask_candidate_diversity.rs"]
mod diversity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SelectionError {
    AmbiguousSource,
    InvalidConfidence,
}

// Smaller is better; BinaryHeap therefore exposes the worst retained item.
// There is only one candidate per memory_id after identity validation.
#[derive(Clone, Copy, Debug)]
struct RankedCandidate<'a> {
    candidate: &'a AskCandidate,
    score: f32,
}

impl Ord for RankedCandidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.candidate.memory_id.cmp(&other.candidate.memory_id))
    }
}

impl PartialOrd for RankedCandidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RankedCandidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankedCandidate<'_> {}

fn same_source(left: &AskCandidate, right: &AskCandidate) -> bool {
    left.content == right.content
        && left.confidence.to_bits() == right.confidence.to_bits()
        && left.trust_class == right.trust_class
        && left.provenance_uri == right.provenance_uri
        && left.level == right.level
        && left.kind == right.kind
        && left.team_provenance == right.team_provenance
}

fn best_span_score(
    question_terms: &[String],
    candidate: &AskCandidate,
    scorer: SpanScorer<'_>,
) -> f32 {
    segment_spans(&candidate.content)
        .into_iter()
        .map(|(start, end)| {
            scorer(
                question_terms,
                &candidate.content[start..end],
                candidate.confidence,
                &candidate.trust_class,
            )
        })
        .fold(0.0, f32::max)
}

/// Admit relevant independent sources, protecting supported opposition.
///
/// Every provided row is checked, including rows outside the eventual budget.
/// Conflicting bodies or citation metadata cannot be resolved by input order.
/// The identity registry holds references, not cloned content. Ranking retains
/// at most `limit` entries per bounded selection set; expensive cross-span
/// clustering remains bounded by the admitted source set. The caller owns
/// database retrieval and its scope.
pub(super) fn select_candidates<'a>(
    request: &AskRequest,
    question_terms: &[String],
    candidates: &'a [AskCandidate],
    limit: usize,
) -> Result<Vec<&'a AskCandidate>, SelectionError> {
    select_candidates_with_scorer(request, question_terms, candidates, limit, &score_span)
}

/// Use the same complete scorer for admission and the eventual answer.
pub(super) fn select_candidates_with_scorer<'a>(
    request: &AskRequest,
    question_terms: &[String],
    candidates: &'a [AskCandidate],
    limit: usize,
    scorer: SpanScorer<'_>,
) -> Result<Vec<&'a AskCandidate>, SelectionError> {
    let mut unique: BTreeMap<&str, &AskCandidate> = BTreeMap::new();
    let mut invalid_confidence = false;
    let mut ambiguous_source = false;
    for candidate in candidates {
        invalid_confidence |=
            !candidate.confidence.is_finite() || !(0.0..=1.0).contains(&candidate.confidence);
        if let Some(previous) = unique.get(candidate.memory_id.as_str()) {
            ambiguous_source |= !same_source(previous, candidate);
        } else {
            unique.insert(candidate.memory_id.as_str(), candidate);
        }
    }
    // Complete the identity scan even on invalid input. This keeps both the
    // reported scan count and error precedence independent of input order.
    if invalid_confidence {
        return Err(SelectionError::InvalidConfidence);
    }
    if ambiguous_source {
        return Err(SelectionError::AmbiguousSource);
    }

    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut retained: BinaryHeap<RankedCandidate<'a>> = BinaryHeap::new();
    for candidate in unique.values().copied() {
        if candidate.content.trim().is_empty() {
            continue;
        }
        let ranked = RankedCandidate {
            candidate,
            score: best_span_score(question_terms, candidate, scorer),
        };
        if retained.len() < limit {
            retained.push(ranked);
        } else if let Some(mut worst) = retained.peek_mut()
            && ranked < *worst
        {
            *worst = ranked;
        }
    }

    let mut ranked = retained.into_sorted_vec();
    diversity::preserve_independent_support(request, question_terms, &unique, &mut ranked, scorer);
    if !preserve_linked_opposition(request, question_terms, &unique, &mut ranked, scorer) {
        preserve_inferred_opposition(request, question_terms, &unique, &mut ranked, scorer);
    }
    Ok(ranked.into_iter().map(|entry| entry.candidate).collect())
}

// These guards are shared with the final answer's explicit-conflict path.
// Admission must neither discard an eligible pair nor admit a weaker pair
// than composition is prepared to disclose.
pub(super) fn eligible_link(request: &AskRequest, link: &AskContradiction) -> bool {
    matches!(link.source.as_str(), "human" | "agent")
        && link.confidence.is_finite()
        && (request.min_confidence..=1.0).contains(&link.confidence)
        && link.src_memory_id != link.dst_memory_id
}

pub(super) fn compare_links(left: &AskContradiction, right: &AskContradiction) -> Ordering {
    left.id
        .cmp(&right.id)
        .then_with(|| left.src_memory_id.cmp(&right.src_memory_id))
        .then_with(|| left.dst_memory_id.cmp(&right.dst_memory_id))
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| right.confidence.total_cmp(&left.confidence))
}

pub(super) fn conflict_score(
    anchor_score: f32,
    anchor_trust: f32,
    other_trust: f32,
    link_confidence: f32,
    minimum: f32,
) -> Option<f32> {
    if ![
        anchor_score,
        anchor_trust,
        other_trust,
        link_confidence,
        minimum,
    ]
    .into_iter()
    .all(f32::is_finite)
    {
        return None;
    }
    let score = anchor_score
        .min(link_confidence)
        .min(anchor_trust)
        .min(other_trust);
    (score >= minimum).then_some(score)
}

/// Preserve the first eligible explicit opposing source for the raw-score
/// anchor, using exactly the ordering and confidence gates of composition.
///
/// A paraphrased opposition can have little lexical overlap with the query.
/// Spending the entire budget on query matches would hide the very evidence
/// that should prevent a one-sided answer. Reserve one existing slot, never
/// grow the budget, fetch another workspace, or walk a chain of links.
/// Return true when the explicit pair is retained, so inferred admission
/// cannot replace the endpoint the final explicit-conflict path will use.
fn preserve_linked_opposition<'a>(
    request: &AskRequest,
    question_terms: &[String],
    unique: &BTreeMap<&str, &'a AskCandidate>,
    ranked: &mut [RankedCandidate<'a>],
    scorer: SpanScorer<'_>,
) -> bool {
    let Some(anchor) = ranked.first().copied() else {
        return false;
    };
    if ranked.len() < 2 || anchor.score < request.min_confidence {
        return false;
    }
    let mut links: Vec<_> = request
        .contradictions
        .iter()
        .filter(|link| eligible_link(request, link))
        .collect();
    links.sort_by(|left, right| compare_links(left, right));

    for link in links {
        let other_id = if link.src_memory_id == anchor.candidate.memory_id {
            &link.dst_memory_id
        } else if link.dst_memory_id == anchor.candidate.memory_id {
            &link.src_memory_id
        } else {
            continue;
        };
        let Some(&other) = unique.get(other_id.as_str()) else {
            continue;
        };
        if other.content.trim().is_empty()
            || conflict_score(
                anchor.score,
                anchor.candidate.confidence * trust_tilt(&anchor.candidate.trust_class),
                other.confidence * trust_tilt(&other.trust_class),
                link.confidence,
                request.min_confidence,
            )
            .is_none()
        {
            continue;
        }
        if !ranked
            .iter()
            .any(|entry| entry.candidate.memory_id == other.memory_id)
        {
            // len >= 2 above; replacing the worst item cannot evict the anchor.
            let last = ranked.len() - 1;
            ranked[last] = RankedCandidate {
                candidate: other,
                score: best_span_score(question_terms, other, scorer),
            };
            ranked.sort();
        }
        // The final explicit-conflict path also exposes one pair, not a graph
        // closure. Reserving additional linked sources would displace useful
        // answer candidates without making those sources visible to the user.
        return true;
    }
    false
}

/// Protect a supported same-topic opposite passage from the relevance cap.
///
/// Session-aware corroboration happens after admission. It cannot recover an
/// opposing excerpt already displaced by hundreds of repeated affirmative
/// excerpts. Reserve one existing slot using the very same lexical topic,
/// polarity and evidence-floor tests as answer composition. This is not a
/// new contradiction classifier and never treats unrelated negation as proof.
fn preserve_inferred_opposition<'a>(
    request: &AskRequest,
    question_terms: &[String],
    unique: &BTreeMap<&str, &'a AskCandidate>,
    ranked: &mut [RankedCandidate<'a>],
    scorer: SpanScorer<'_>,
) {
    if ranked.len() < 2 || unique.len() <= ranked.len() {
        return;
    }
    let anchor = ranked[0];
    if anchor.score < request.min_confidence {
        return;
    }
    // Match all_spans' raw-score anchor, including its earliest-byte tie
    // break. The candidate's first sentence need not be its best answer.
    let Some((start, end)) =
        segment_spans(&anchor.candidate.content)
            .into_iter()
            .find(|&(start, end)| {
                scorer(
                    question_terms,
                    &anchor.candidate.content[start..end],
                    anchor.candidate.confidence,
                    &anchor.candidate.trust_class,
                )
                .total_cmp(&anchor.score)
                    == Ordering::Equal
            })
    else {
        return;
    };
    let anchor_text = &anchor.candidate.content[start..end];
    let anchor_terms = tokenize_for_ask(anchor_text);
    let anchor_negated = has_negation(anchor_text);
    let mut opposition: Option<RankedCandidate<'a>> = None;
    for candidate in unique.values().copied() {
        for (start, end) in segment_spans(&candidate.content) {
            let text = &candidate.content[start..end];
            let numeric_opposition = super::numeric_conflict(anchor_text, text);
            if (has_negation(text) == anchor_negated
                || !super::same_numeric_context(anchor_text, text))
                && !numeric_opposition
            {
                continue;
            }
            let score = scorer(
                question_terms,
                text,
                candidate.confidence,
                &candidate.trust_class,
            );
            if score < request.min_confidence
                || (!numeric_opposition
                    && !same_conflict_topic(&anchor_terms, &tokenize_for_ask(text)))
            {
                continue;
            }
            let opposing = RankedCandidate { candidate, score };
            if opposition.is_none_or(|current| opposing < current) {
                opposition = Some(opposing);
            }
        }
    }
    let Some(opposition) = opposition else {
        return;
    };
    if ranked
        .iter()
        .any(|entry| entry.candidate.memory_id == opposition.candidate.memory_id)
    {
        return;
    }
    // Only the candidate is reserved; its body, byte offsets, trust and
    // confidence remain unchanged. Rank it by its best span as usual.
    let last = ranked.len() - 1;
    ranked[last] = RankedCandidate {
        candidate: opposition.candidate,
        score: best_span_score(question_terms, opposition.candidate, scorer),
    };
    ranked.sort();
}

#[cfg(test)]
#[path = "ask_candidate_selection_tests.rs"]
mod tests;

#[cfg(test)]
mod inferred_opposition_tests {
    use super::*;
    use crate::core::ask::{ASK_CANDIDATE_SCAN_CAP, AskNativeSource, ask_data_json, evaluate_ask};
    use crate::models::EvidenceId;
    use crate::pack::PackEntityRef;

    const AFFIRMING: &str = "Run cargo fmt before release.";
    const OPPOSING: &str = "Never run cargo fmt before release.";

    fn candidate(id: &str, body: &str) -> AskCandidate {
        AskCandidate {
            memory_id: id.to_owned(),
            content: body.to_owned(),
            confidence: 1.0,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some(format!("manual://opposition/{id}")),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            team_provenance: None,
        }
    }

    fn request() -> AskRequest {
        AskRequest {
            question: "Run cargo fmt before release".to_owned(),
            ..AskRequest::default()
        }
    }

    fn crowded() -> Vec<AskCandidate> {
        let mut rows: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 4)
            .map(|index| candidate(&format!("a-support-{index:05}"), AFFIRMING))
            .collect();
        rows.push(candidate("z-opposition", OPPOSING));
        rows
    }

    fn ids(request: &AskRequest, rows: &[AskCandidate], limit: usize) -> Vec<String> {
        select_candidates(request, &tokenize_for_ask(&request.question), rows, limit)
            .expect("valid source fixtures")
            .into_iter()
            .map(|row| row.memory_id.clone())
            .collect()
    }

    #[test]
    fn repeated_transcript_excerpts_cannot_hide_opposition_beyond_the_cap() {
        let mut request = request();
        let mut rows = crowded();
        for (index, row) in rows.iter_mut().enumerate() {
            let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(index as u128 + 1));
            row.memory_id = id.to_string();
            row.confidence = 0.5;
            row.trust_class = "cass_evidence".to_owned();
            row.provenance_uri = Some(format!("cass-session://conversation#L{}", index + 1));
            request.native_sources.insert(
                row.memory_id.clone(),
                AskNativeSource {
                    entity: PackEntityRef::EvidenceSpan(id),
                    entity_revision: format!("blake3:{}", "0".repeat(64)),
                    source_memory_ids: Vec::new(),
                },
            );
        }
        let opposing_id = rows.last().expect("opposing source").memory_id.clone();
        let selected = ids(&request, &rows, ASK_CANDIDATE_SCAN_CAP);
        assert_eq!(selected.len(), ASK_CANDIDATE_SCAN_CAP);
        assert!(selected.contains(&opposing_id));
        let report = evaluate_ask(&request, &rows);
        assert!(report.conflict_detected);
        assert!(!report.abstained);
        assert!(report.answer_text.is_none());
        assert!(report.conflict_link.is_none());
        assert_eq!(report.confidence_components.corroboration, 1.0);
        let sides = report.sides.as_ref().expect("both supported sides");
        assert_eq!(sides.len(), 2);
        assert!(
            sides
                .iter()
                .flat_map(|side| &side.citations)
                .any(|citation| { citation.memory_id == opposing_id && citation.text == OPPOSING })
        );
        let expected = ask_data_json(&report);
        rows.reverse();
        assert_eq!(ask_data_json(&evaluate_ask(&request, &rows)), expected);
        rows.rotate_left(31);
        assert_eq!(ask_data_json(&evaluate_ask(&request, &rows)), expected);
    }

    #[test]
    fn opposing_passage_need_not_be_the_sources_best_scoring_sentence() {
        let mut rows = crowded();
        rows.last_mut().expect("opposing source").content = format!("{AFFIRMING} {OPPOSING}");
        let report = evaluate_ask(&request(), &rows);
        assert!(report.conflict_detected);
        let citations: Vec<_> = report
            .sides
            .as_ref()
            .expect("both sides")
            .iter()
            .flat_map(|side| &side.citations)
            .collect();
        let opposing = citations
            .iter()
            .find(|citation| citation.text == OPPOSING)
            .expect("opposing passage retained");
        assert_eq!(opposing.memory_id, "z-opposition");
        assert!(opposing.byte_start > 0);
        assert_eq!(
            rows.last()
                .expect("source")
                .content
                .get(opposing.byte_start..opposing.byte_end),
            Some(OPPOSING)
        );
    }

    #[test]
    fn opposition_below_the_evidence_floor_does_not_displace_an_answer() {
        let mut request = request();
        request.min_confidence = 1.0;
        assert!(
            score_span(
                &tokenize_for_ask(&request.question),
                OPPOSING,
                1.0,
                "human_explicit"
            ) < request.min_confidence
        );
        let rows = crowded();
        let selected = ids(&request, &rows, 2);
        assert_eq!(selected, ["a-support-00000", "a-support-00001"]);
    }

    #[test]
    fn unrelated_negation_does_not_consume_the_reserved_slot() {
        let mut request = request();
        request.min_confidence = 0.0;
        let rows = vec![
            candidate("a", AFFIRMING),
            candidate("b", AFFIRMING),
            candidate("z", "Never restart the database."),
        ];
        assert_eq!(ids(&request, &rows, 2), ["a", "b"]);
    }

    #[test]
    fn zero_and_single_slot_budgets_are_not_grown_for_opposition() {
        let rows = crowded();
        assert!(ids(&request(), &rows, 0).is_empty());
        assert_eq!(ids(&request(), &rows, 1), ["a-support-00000"]);
    }

    #[test]
    fn explicit_opposition_retains_priority_over_inferred_opposition() {
        let mut request = request();
        request.contradictions.push(AskContradiction {
            id: "explicit-edge".to_owned(),
            src_memory_id: "a-support-00000".to_owned(),
            dst_memory_id: "linked-opposition".to_owned(),
            confidence: 0.9,
            source: "human".to_owned(),
        });
        let mut rows = crowded();
        rows.push(candidate(
            "linked-opposition",
            "Formatting is prohibited by the deployment policy.",
        ));
        assert_eq!(
            ids(&request, &rows, 2),
            ["a-support-00000", "linked-opposition"]
        );
        let report = evaluate_ask(&request, &rows);
        assert!(report.conflict_detected);
        assert_eq!(
            report.conflict_link.as_ref().map(|link| link.id.as_str()),
            Some("explicit-edge")
        );
    }

    #[test]
    fn an_opposing_anchor_can_retain_a_lower_scoring_affirmative_source() {
        let mut request = request();
        request.question = "Never run cargo fmt before release".to_owned();
        let rows = vec![
            candidate("a", OPPOSING),
            candidate("b", OPPOSING),
            candidate("z", AFFIRMING),
        ];
        assert_eq!(ids(&request, &rows, 2), ["a", "z"]);
    }
}
