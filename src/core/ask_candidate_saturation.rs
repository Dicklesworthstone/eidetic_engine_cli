//! Reclaim saturated exact-copy slots for distinct, already-supported answers.
//!
//! This is not semantic deduplication: bodies and every span-scoring input
//! must match exactly. Independent provenance remains independent, and enough
//! of it is retained to reach the existing corroboration cap. A new body must
//! clear the ordinary evidence floor without a manufactured support bonus.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AskCandidate, AskRequest, CORROBORATION_CAP, RankedCandidate, SpanScorer, best_span_score,
    support_key,
};

type SourceSignature<'a> = (&'a str, u32, &'a str);

/// Run after lineage diversity and before the existing opposition reservations.
/// Only slots beyond demonstrated full corroboration are exchangeable. Keep
/// the raw anchor, original source IDs, exact bodies, and all weaker evidence
/// unless a supported, previously unrepresented body can use a redundant slot.
/// All auxiliary selection sets are bounded by the caller's existing budget.
pub(super) fn preserve_distinct_answers<'a>(
    request: &AskRequest,
    question_terms: &[String],
    unique: &BTreeMap<&str, &'a AskCandidate>,
    ranked: &mut [RankedCandidate<'a>],
    groups: &BTreeMap<String, String>,
    scorer: SpanScorer<'_>,
) {
    // Match clustering's capped logarithmic multiplier. Do not hard-code a
    // count of copies, confuse source IDs with lineages, or round down the
    // number of independent observations needed to saturate the multiplier.
    let Some(support_limit) =
        (1..ranked.len()).find(|&count| 1.0 + 0.1 * (count as f32).ln() >= CORROBORATION_CAP)
    else {
        return;
    };
    let mut support: BTreeMap<SourceSignature<'_>, BTreeSet<&str>> = BTreeMap::new();
    let mut retained = Vec::with_capacity(ranked.len());
    let mut redundant = Vec::new();
    let mut admitted_bodies = BTreeSet::new();
    for entry in ranked.iter().copied() {
        let candidate = entry.candidate;
        admitted_bodies.insert(candidate.content.as_str());
        // Matching only the best sentence would lose other facts in the body.
        // Matching only the body could discard a distinct trust/confidence
        // input that changes scores for its other spans. Require all three.
        let key = (
            candidate.content.as_str(),
            candidate.confidence.to_bits(),
            candidate.trust_class.as_str(),
        );
        let origins = support.entry(key).or_default();
        if origins.len() >= support_limit {
            redundant.push(entry);
        } else {
            origins.insert(support_key(&candidate.memory_id, groups));
            retained.push(entry);
        }
    }
    if redundant.is_empty() {
        return;
    }

    // Pick the best distinct bodies, not thousands of replicas of the next
    // body. Both maps contain at most the number of reclaimable slots. Scope
    // and identity validation already happened before this selection pass.
    let limit = redundant.len();
    let mut by_body: BTreeMap<&str, RankedCandidate<'a>> = BTreeMap::new();
    let mut best: BTreeSet<RankedCandidate<'a>> = BTreeSet::new();
    for candidate in unique.values().copied() {
        if candidate.content.trim().is_empty()
            || admitted_bodies.contains(candidate.content.as_str())
        {
            continue;
        }
        let entry = RankedCandidate {
            candidate,
            score: best_span_score(question_terms, candidate, scorer),
        };
        if !entry.score.is_finite() || entry.score <= 0.0 || entry.score < request.min_confidence {
            continue;
        }
        if let Some(previous) = by_body.get(candidate.content.as_str()).copied() {
            if entry >= previous {
                continue;
            }
            best.remove(&previous);
        }
        by_body.insert(candidate.content.as_str(), entry);
        best.insert(entry);
        if best.len() > limit
            && let Some(worst) = best.pop_last()
        {
            by_body.remove(worst.candidate.content.as_str());
        }
    }
    if best.is_empty() {
        return;
    }
    retained.extend(best);
    let spare = ranked.len() - retained.len();
    retained.extend(redundant.into_iter().take(spare));
    retained.sort();
    ranked.copy_from_slice(&retained);
}
