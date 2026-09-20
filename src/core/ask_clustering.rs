//! Exact sparse clustering for extractive answers.
//!
//! Keep the existing greedy seed order, Jaccard threshold, polarity gate and
//! distinct-memory corroboration. Numeric-bearing literals must also agree:
//! a different port or version is not corroboration merely because the other
//! words match. This guard preserves alternatives; it does not infer that every
//! pair of different numbers is a contradiction.
//!
//! An inverted term index replaces the old all-pairs scan: disjoint spans cannot
//! meet a positive Jaccard threshold. Dense shared vocabulary can still require
//! quadratic work; this is not a universal latency bound or evidence truncation.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AskSpan, CLUSTER_SIMILARITY_THRESHOLD, CORROBORATION_CAP, has_negation, tokenize_for_ask,
};

/// Keep ordered literal spellings, not numeric values or a bag of digits.
/// Parsing floats would collapse versions, overflow large identifiers and lose
/// signs or precision. Keeping the entire numeric-bearing token also protects
/// identifiers, units, IP addresses and versions. Sentence punctuation and
/// quote delimiters do not change a literal; internal punctuation still does.
/// Formatting differences may withhold a corroboration bonus, never invent one.
fn numeric_literals(text: &str) -> Vec<String> {
    text.split(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`')
    })
    .filter(|token| token.chars().any(char::is_numeric))
    .map(|token| token.trim_end_matches(['.', '!', '?']).to_lowercase())
    .collect()
}

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
    let literals: Vec<_> = spans.iter().map(|span| numeric_literals(&span.text)).collect();
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
            // Jaccard drops order and heavily discounts a single changed word.
            // It must not turn a different factual value into independent
            // support, hide its citation, or lift a weak answer over the floor.
            if literals[seed] != literals[other] {
                continue;
            }
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

#[cfg(test)]
mod numeric_tests {
    use super::*;

    fn span(id: &str, content: &str) -> AskSpan {
        AskSpan {
            memory_id: id.to_owned(),
            byte_start: 0,
            byte_end: content.len(),
            text: content.to_owned(),
            score: 0.52,
            trust_class: "human_explicit".to_owned(),
            memory_confidence: 1.0,
            provenance_uri: Some(format!("manual://numeric-claims/{id}")),
            team_provenance: None,
        }
    }

    #[test]
    fn incompatible_numeric_facts_stay_visible_without_a_confidence_bonus() {
        for (left, right) in [
            ("5432", "6432"),
            ("-30", "30"),
            ("1.25", "1.75"),
            ("1.2.3", "1.2.4"),
            ("2026-09-19", "2026-09-20"),
            ("9007199254740992", "9007199254740993"),
            ("worker1", "worker2"),
            ("10ms", "10s"),
        ] {
            let input = [
                span("a", &format!("The production database service configuration uses the fixed value {left}.")),
                span("b", &format!("The production database service configuration uses the fixed value {right}.")),
            ];
            let actual = cluster_spans(&input);
            assert_eq!(actual.len(), 2, "different values {left} / {right}");
            for (result, original) in actual.iter().zip(&input) {
                assert_eq!(result.memory_id, original.memory_id);
                assert_eq!(result.text, original.text);
                assert_eq!(result.byte_start, original.byte_start);
                assert_eq!(result.byte_end, original.byte_end);
                assert_eq!(result.provenance_uri, original.provenance_uri);
                assert_eq!(result.score.to_bits(), original.score.to_bits());
                assert!(result.score < super::super::ASK_MIN_CONFIDENCE_DEFAULT);
            }
        }
    }

    #[test]
    fn equal_numeric_facts_still_corroborate_independent_sources() {
        let content = "The production database service configuration uses the fixed value 5432.";
        let input = [span("a", content), span("b", content)];
        let actual = cluster_spans(&input);
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].text, content);
        assert_eq!(actual[0].score.to_bits(), (0.52_f32 * (1.0 + 0.1 * 2.0_f32.ln())).to_bits());
        assert!(actual[0].score >= super::super::ASK_MIN_CONFIDENCE_DEFAULT);
    }

    #[test]
    fn equal_numbers_from_one_lineage_do_not_add_independent_support() {
        let content = "The database port is 5432.";
        let input = [span("a", content), span("b", content)];
        let groups = BTreeMap::from([
            ("a".to_owned(), "session".to_owned()),
            ("b".to_owned(), "session".to_owned()),
        ]);
        let actual = cluster_spans_with_groups(&input, &groups);
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].score.to_bits(), 0.52_f32.to_bits());
    }

    #[test]
    fn value_order_and_missing_values_are_not_discarded() {
        for (left, right) in [
            ("Use port 5432 and retry limit 3.", "Use port 3 and retry limit 5432."),
            ("Use port 5432.", "Use the port."),
        ] {
            assert_ne!(numeric_literals(left), numeric_literals(right));
            assert_eq!(cluster_spans(&[span("a", left), span("b", right)]).len(), 2);
        }
    }

    #[test]
    fn quotes_and_sentence_punctuation_preserve_literal_spelling() {
        assert_eq!(numeric_literals("Use `1.2.3`."), ["1.2.3"]);
        assert_eq!(numeric_literals("Use (5432)!"), ["5432"]);
        assert_eq!(numeric_literals("-1.25 and +1.25"), ["-1.25", "+1.25"]);
        assert_eq!(numeric_literals("Timeout: ３０ms."), ["３０ms"]);
    }

    #[test]
    fn opposite_polarities_with_equal_values_never_corroborate() {
        let input = [
            span("a", "The production database service must use port 5432."),
            span("b", "The production database service must not use port 5432."),
        ];
        let actual = cluster_spans(&input);
        assert_eq!(actual.len(), 2);
        assert!(actual.iter().all(|item| item.score.to_bits() == 0.52_f32.to_bits()));
    }

    #[test]
    fn numeric_alternatives_are_deterministic_under_input_permutation() {
        let mut input = vec![
            span("a", "The production database service uses port 5432."),
            span("b", "The production database service uses port 6432."),
            span("c", "The production database service uses port 5432."),
        ];
        let signature = |rows: &[AskSpan]| {
            cluster_spans(rows).into_iter().map(|item| {
                (item.memory_id, item.text, item.score.to_bits(), item.provenance_uri)
            }).collect::<Vec<_>>()
        };
        let expected = signature(&input);
        assert_eq!(expected.len(), 2);
        for _ in 0..input.len() {
            input.rotate_left(1);
            assert_eq!(signature(&input), expected);
            input.reverse();
            assert_eq!(signature(&input), expected);
        }
    }
}
