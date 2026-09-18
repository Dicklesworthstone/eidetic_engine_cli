//! Exact sparse clustering for extractive answers.
//!
//! Keep the existing greedy seed order, Jaccard threshold, polarity gate and
//! distinct-memory corroboration. An inverted term index replaces the old
//! all-pairs scan: disjoint spans cannot meet a positive Jaccard threshold.
//! Dense shared vocabulary can still require quadratic work; this is not a
//! claim of a universal latency bound and does not truncate any evidence.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AskSpan, CLUSTER_SIMILARITY_THRESHOLD, CORROBORATION_CAP, has_negation, tokenize_for_ask,
};

pub(super) fn cluster_spans(spans: &[AskSpan]) -> Vec<AskSpan> {
    cluster_with_observer(spans, || {})
}

fn cluster_with_observer(spans: &[AskSpan], mut on_similarity_check: impl FnMut()) -> Vec<AskSpan> {
    cluster_with_groups_and_observer(spans, &BTreeMap::new(), &mut on_similarity_check)
}

pub(super) fn cluster_spans_with_groups(
    spans: &[AskSpan],
    groups: &BTreeMap<String, String>,
) -> Vec<AskSpan> {
    cluster_with_groups_and_observer(spans, groups, || {})
}

fn cluster_with_groups_and_observer(
    spans: &[AskSpan],
    groups: &BTreeMap<String, String>,
    mut on_similarity_check: impl FnMut(),
) -> Vec<AskSpan> {
    let support_key = |index: usize| {
        groups
            .get(&spans[index].memory_id)
            .map(String::as_str)
            .unwrap_or(&spans[index].memory_id)
    };
    let terms: Vec<Vec<String>> = spans
        .iter()
        .map(|span| tokenize_for_ask(&span.text))
        .collect();
    let negated: Vec<bool> = spans.iter().map(|span| has_negation(&span.text)).collect();
    let mut postings: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, span_terms) in terms.iter().enumerate() {
        // tokenize_for_ask sorts and deduplicates, so each posting contributes
        // exactly one to a pair's set intersection, not its word frequency.
        for term in span_terms {
            postings.entry(term.as_str()).or_default().push(index);
        }
    }

    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&left, &right| {
        spans[right]
            .score
            .total_cmp(&spans[left].score)
            .then_with(|| spans[left].memory_id.cmp(&spans[right].memory_id))
            .then_with(|| spans[left].byte_start.cmp(&spans[right].byte_start))
    });
    let mut assigned = vec![false; spans.len()];
    let mut representatives = Vec::new();
    for seed in order {
        if assigned[seed] {
            continue;
        }
        assigned[seed] = true;
        let mut intersections: BTreeMap<usize, usize> = BTreeMap::new();
        for term in &terms[seed] {
            for &other in &postings[term.as_str()] {
                if !assigned[other] && negated[seed] == negated[other] {
                    *intersections.entry(other).or_default() += 1;
                }
            }
        }

        let mut supporting_memories = BTreeSet::from([support_key(seed)]);
        // All members are compared to the seed, never to one another. Thus
        // processing this neighborhood in index order instead of score order
        // cannot change membership or accidentally introduce transitive links.
        for (other, intersection) in intersections {
            on_similarity_check();
            let union = terms[seed].len() + terms[other].len() - intersection;
            // Same integer set counts and f32 division as jaccard_similarity.
            // A posting hit guarantees a nonempty union.
            let similarity = intersection as f32 / union as f32;
            if similarity >= CLUSTER_SIMILARITY_THRESHOLD {
                assigned[other] = true;
                supporting_memories.insert(support_key(other));
            }
        }
        let corroboration =
            (1.0 + 0.1 * (supporting_memories.len() as f32).ln()).min(CORROBORATION_CAP);
        let mut representative = spans[seed].clone();
        representative.score = (representative.score * corroboration).clamp(0.0, 1.0);
        representatives.push(representative);
    }
    representatives.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    representatives
}

#[cfg(test)]
#[path = "ask_clustering_tests.rs"]
mod tests;
