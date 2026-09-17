//! Query-ranked admission for the bounded extractive ask engine.
//!
//! The bound belongs after relevance scoring, not before it: a caller's
//! database order must not decide whether an answer exists. Identity checking
//! happens before admission, so the limit cannot hide inconsistent copies of
//! the same source. This module never expands the caller's workspace scope.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use super::{AskCandidate, AskContradiction, AskRequest, score_span, segment_spans, trust_tilt};

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

fn best_span_score(question_terms: &[String], candidate: &AskCandidate) -> f32 {
    segment_spans(&candidate.content)
        .into_iter()
        .map(|(start, end)| {
            score_span(
                question_terms,
                &candidate.content[start..end],
                candidate.confidence,
                &candidate.trust_class,
            )
        })
        .fold(0.0, f32::max)
}

/// Admit the best `limit` distinct memories, ranked by their best answer span.
///
/// Every provided row is checked, including rows outside the eventual budget.
/// Conflicting bodies or citation metadata cannot be resolved by input order.
/// The identity registry holds references, not cloned content. Ranking retains
/// at most `limit` entries; expensive cross-span clustering remains bounded by
/// the admitted memory set. The caller owns database retrieval and its scope.
pub(super) fn select_candidates<'a>(
    request: &AskRequest,
    question_terms: &[String],
    candidates: &'a [AskCandidate],
    limit: usize,
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
            score: best_span_score(question_terms, candidate),
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
    preserve_linked_opposition(request, question_terms, &unique, &mut ranked);
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
    if ![anchor_score, anchor_trust, other_trust, link_confidence, minimum]
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
fn preserve_linked_opposition<'a>(
    request: &AskRequest,
    question_terms: &[String],
    unique: &BTreeMap<&str, &'a AskCandidate>,
    ranked: &mut [RankedCandidate<'a>],
) {
    let Some(anchor) = ranked.first().copied() else {
        return;
    };
    if ranked.len() < 2 || anchor.score < request.min_confidence {
        return;
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
        if !ranked.iter().any(|entry| entry.candidate.memory_id == other.memory_id) {
            // len >= 2 above; replacing the worst item cannot evict the anchor.
            let last = ranked.len() - 1;
            ranked[last] = RankedCandidate {
                candidate: other,
                score: best_span_score(question_terms, other),
            };
            ranked.sort();
        }
        // The final explicit-conflict path also exposes one pair, not a graph
        // closure. Reserving additional linked sources would displace useful
        // answer candidates without making those sources visible to the user.
        break;
    }
}

#[cfg(test)]
#[path = "ask_candidate_selection_tests.rs"]
mod tests;
