//! Exact sparse clustering for extractive answers.
//!
//! Keep the existing greedy seed order, Jaccard threshold, polarity gate and
//! distinct-memory corroboration. Numeric-bearing literals must also agree:
//! a different port or version is not corroboration merely because the other
//! words match. This guard preserves alternatives; it does not infer that every
//! pair of different numbers is a contradiction. Explicit categorical settings
//! also retain their subject and value: a different backend is not independent
//! support, and a staging setting cannot corroborate a production setting.
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
pub(super) fn numeric_literals(text: &str) -> Vec<String> {
    text.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`'
            )
    })
    .filter(|token| token.chars().any(char::is_numeric))
    // Units and numeric identifiers can be case-sensitive (MW versus mW).
    // Case folding here would manufacture agreement before conflict detection.
    .map(|token| token.trim_end_matches(['.', '!', '?']).to_owned())
    .collect()
}

/// A deliberately narrow, single-valued configuration claim. Natural-language
/// subjects stay ordered and complete; machine keys and quoted values retain
/// case. This is not a general entity/relation or synonym resolver.
#[derive(Debug)]
pub(super) struct CategoricalSetting {
    subject: Vec<String>,
    value: String,
    exact_value: bool,
}

/// Recognize `key=value` or a singular named setting ending in `is VALUE`.
/// A list, qualification, conditional, unknown value or plural capability is
/// not a singleton assignment. In particular, two supported backends or two
/// recommendations may coexist and must not be reported as a contradiction.
pub(super) fn categorical_setting(text: &str) -> Option<CategoricalSetting> {
    if has_negation(text) || text.chars().any(char::is_control) {
        return None;
    }
    let text = text.trim().trim_end_matches(['.', '!', '?']);
    let words: Vec<_> = text.split_whitespace().collect();
    if words.iter().any(|word| {
        matches!(
            word.to_ascii_lowercase().as_str(),
            "if" | "unless"
                | "when"
                | "except"
                | "either"
                | "or"
                | "and"
                | "may"
                | "might"
                | "could"
                | "can"
                | "possible"
                | "potential"
                | "recommended"
                | "preferred"
                | "supported"
                | "compatible"
                | "perhaps"
                | "probably"
                | "apparently"
                | "usually"
                | "sometimes"
                | "proposed"
                | "planned"
                | "hypothetical"
                | "alternative"
                | "optional"
                | "expected"
                | "assumed"
                | "example"
        )
    }) {
        return None;
    }

    let (subject, raw_value, machine_key) = if let Some((key, value)) = text.split_once('=') {
        let key = key.trim();
        if !key
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            return None;
        }
        // Keep exact machine key spelling, distinct from prose subjects.
        (vec!["=".to_owned(), key.to_owned()], value.trim(), true)
    } else {
        let value_index = words.len().checked_sub(1)?;
        let copula_index = value_index.checked_sub(1)?;
        if !words[copula_index].eq_ignore_ascii_case("is") || copula_index == 0 {
            return None;
        }
        if words[..copula_index].iter().any(|word| {
            word.eq_ignore_ascii_case("is")
                || word.eq_ignore_ascii_case("are")
                || word.contains([',', ';', ':', '!', '?', '"', '\'', '`', '(', ')'])
                || word.ends_with('.')
        }) {
            return None;
        }
        let field = words[copula_index - 1].to_ascii_lowercase();
        if !matches!(
            field.as_str(),
            "backend"
                | "engine"
                | "mode"
                | "format"
                | "profile"
                | "codec"
                | "driver"
                | "provider"
                | "algorithm"
                | "encoding"
                | "protocol"
                | "runtime"
        ) {
            return None;
        }
        let subject: Vec<_> = words[..copula_index]
            .iter()
            .map(|word| (*word).to_owned())
            .collect();
        // Exact subject spelling is intentional: even a case-only change can
        // name another identifier. A missed inference is safer than merging
        // environments or inventing an asserted contradiction between them.
        (subject, words[value_index], false)
    };

    let (value, quoted) = match raw_value.chars().next()? {
        quote @ ('`' | '\'' | '"') => (raw_value.strip_prefix(quote)?.strip_suffix(quote)?, true),
        _ => (raw_value, false),
    };
    if !value
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic())
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "unknown"
                | "unspecified"
                | "unavailable"
                | "undetermined"
                | "unset"
                | "unconfigured"
                | "pending"
                | "tbd"
                | "either"
        )
    {
        return None;
    }
    Some(CategoricalSetting {
        subject,
        value: value.to_owned(),
        exact_value: machine_key || quoted,
    })
}

/// Only two affirmative singleton assignments to the exact same subject can
/// conflict. Bare prose names compare case-insensitively; two explicit code
/// literals retain their case-sensitive configuration semantics.
pub(super) fn categorical_settings_conflict(
    left: Option<&CategoricalSetting>,
    right: Option<&CategoricalSetting>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    left.subject == right.subject
        && if left.exact_value && right.exact_value {
            left.value != right.value
        } else {
            !left.value.eq_ignore_ascii_case(&right.value)
        }
}

fn categorical_settings_compatible(
    left: Option<&CategoricalSetting>,
    right: Option<&CategoricalSetting>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.subject == right.subject && !categorical_settings_conflict(Some(left), Some(right))
        }
        // A qualified or unparsed statement cannot silently corroborate a
        // known singleton setting just because Jaccard drops its qualifier.
        _ => false,
    }
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
    let literals: Vec<_> = spans
        .iter()
        .map(|span| numeric_literals(&span.text))
        .collect();
    let settings: Vec<_> = spans
        .iter()
        .map(|span| categorical_setting(&span.text))
        .collect();
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
            if literals[seed] != literals[other]
                || !categorical_settings_compatible(
                    settings[seed].as_ref(),
                    settings[other].as_ref(),
                )
            {
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
            ("10MW", "10mW"),
            ("10MB", "10mb"),
        ] {
            let input = [
                span(
                    "a",
                    &format!(
                        "The production database service configuration uses the fixed value {left}."
                    ),
                ),
                span(
                    "b",
                    &format!(
                        "The production database service configuration uses the fixed value {right}."
                    ),
                ),
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
        assert_eq!(
            actual[0].score.to_bits(),
            (0.52_f32 * (1.0 + 0.1 * 2.0_f32.ln())).to_bits()
        );
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
            (
                "Use port 5432 and retry limit 3.",
                "Use port 3 and retry limit 5432.",
            ),
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
            span(
                "b",
                "The production database service must not use port 5432.",
            ),
        ];
        let actual = cluster_spans(&input);
        assert_eq!(actual.len(), 2);
        assert!(
            actual
                .iter()
                .all(|item| item.score.to_bits() == 0.52_f32.to_bits())
        );
    }

    #[test]
    fn numeric_alternatives_are_deterministic_under_input_permutation() {
        let mut input = vec![
            span("a", "The production database service uses port 5432."),
            span("b", "The production database service uses port 6432."),
            span("c", "The production database service uses port 5432."),
        ];
        let signature = |rows: &[AskSpan]| {
            cluster_spans(rows)
                .into_iter()
                .map(|item| {
                    (
                        item.memory_id,
                        item.text,
                        item.score.to_bits(),
                        item.provenance_uri,
                    )
                })
                .collect::<Vec<_>>()
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

#[cfg(test)]
mod categorical_tests {
    use super::*;

    const POSTGRES: &str =
        "The production application database service configuration backend is postgres.";
    const MYSQL: &str =
        "The production application database service configuration backend is mysql.";

    fn span(id: &str, body: &str) -> AskSpan {
        AskSpan {
            memory_id: id.to_owned(),
            byte_start: 0,
            byte_end: body.len(),
            text: body.to_owned(),
            score: 0.52,
            trust_class: "human_explicit".to_owned(),
            memory_confidence: 1.0,
            provenance_uri: Some(format!("manual://categorical/{id}")),
            team_provenance: None,
        }
    }

    fn conflict(left: &str, right: &str) -> bool {
        categorical_settings_conflict(
            categorical_setting(left).as_ref(),
            categorical_setting(right).as_ref(),
        )
    }

    #[test]
    fn different_backends_are_not_false_corroboration() {
        // This pair passes the old lexical threshold and has no numbers or
        // opposite polarity. Previously it hid one value and lifted 0.52 over
        // the default 0.55 answer floor through a false corroboration bonus.
        let left = tokenize_for_ask(POSTGRES);
        let right = tokenize_for_ask(MYSQL);
        assert!(super::super::jaccard_similarity(&left, &right) >= CLUSTER_SIMILARITY_THRESHOLD);
        let input = [span("a", POSTGRES), span("b", MYSQL)];
        let result = cluster_spans(&input);
        assert_eq!(result.len(), 2);
        for (actual, original) in result.iter().zip(&input) {
            assert_eq!(actual.memory_id, original.memory_id);
            assert_eq!(actual.text, original.text);
            assert_eq!(actual.byte_start, original.byte_start);
            assert_eq!(actual.byte_end, original.byte_end);
            assert_eq!(actual.provenance_uri, original.provenance_uri);
            assert_eq!(actual.score.to_bits(), original.score.to_bits());
        }
        assert!(conflict(POSTGRES, MYSQL));
        assert!(conflict(MYSQL, POSTGRES));
    }

    #[test]
    fn equal_settings_keep_independent_but_not_same_lineage_support() {
        let rows = [span("a", POSTGRES), span("b", POSTGRES)];
        let independent = cluster_spans(&rows);
        assert_eq!(independent.len(), 1);
        assert!(independent[0].score > 0.52);
        let groups = BTreeMap::from([
            ("a".to_owned(), "session".to_owned()),
            ("b".to_owned(), "session".to_owned()),
        ]);
        let correlated = cluster_spans_with_groups(&rows, &groups);
        assert_eq!(correlated.len(), 1);
        assert_eq!(correlated[0].score.to_bits(), 0.52_f32.to_bits());
    }

    #[test]
    fn environments_do_not_corroborate_or_contradict_each_other() {
        let staging = POSTGRES.replace("production", "staging");
        assert!(!conflict(POSTGRES, &staging));
        let rows = [span("a", POSTGRES), span("b", &staging)];
        let result = cluster_spans(&rows);
        assert_eq!(result.len(), 2);
        assert!(
            result
                .iter()
                .all(|row| row.score.to_bits() == 0.52_f32.to_bits())
        );
        assert!(!conflict(POSTGRES, &MYSQL.replace("production", "staging")));
    }

    #[test]
    fn explicit_machine_keys_and_code_values_preserve_case() {
        for (left, right) in [
            ("database.backend=postgres", "database.backend=mysql"),
            (
                "DATABASE_BACKEND = `postgres`",
                "DATABASE_BACKEND = `mysql`",
            ),
            ("BUILD_PROFILE=Release", "BUILD_PROFILE=release"),
            (
                "The build profile is `Release`.",
                "The build profile is `release`.",
            ),
        ] {
            assert!(conflict(left, right), "{left} / {right}");
        }
        assert!(!conflict("Backend=postgres", "backend=mysql"));
        assert!(!conflict(
            "The backend is Postgres.",
            "The backend is postgres."
        ));
        assert!(!conflict(
            "The backend is postgres.",
            "The backend is `postgres`."
        ));
    }

    #[test]
    fn qualified_and_multi_valued_statements_do_not_invent_conflicts() {
        for body in [
            "The database supports postgres.",
            "The supported backend is postgres.",
            "The preferred backend is postgres.",
            "The possible backend is postgres.",
            "If busy the backend is postgres.",
            "The backend is postgres if busy.",
            "The backend is postgres or mysql.",
            "The backend is not postgres.",
            "The backend is unknown.",
            "The backend is unspecified.",
            "The backend is `postgres.",
            "The backend is postgres`.",
            "The backend is postgres. The mode is async.",
            "The backend is postgres\nThe mode is async.",
            "The manager is Alice.",
            "The members are Alice.",
            "Run cargo build --profile=release.",
            "backend==postgres",
            "backend=postgres/mysql",
        ] {
            assert!(categorical_setting(body).is_none(), "{body}");
        }
        let qualified = POSTGRES.replace("backend is", "supported backend is");
        assert_eq!(
            cluster_spans(&[span("a", POSTGRES), span("b", &qualified)]).len(),
            2
        );
    }

    #[test]
    fn subject_order_and_numeric_identifiers_are_not_wildcards() {
        for (left, right) in [
            (
                "The worker1 backend is postgres.",
                "The worker2 backend is mysql.",
            ),
            (
                "The Worker1 backend is postgres.",
                "The worker1 backend is mysql.",
            ),
            (
                "The café backend is postgres.",
                "The café backend is mysql.",
            ),
        ] {
            let same_subject = left.starts_with("The café");
            assert_eq!(conflict(left, right), same_subject);
        }
        assert!(!conflict(
            "The proxy database backend is postgres.",
            "The database proxy backend is mysql.",
        ));
    }

    #[test]
    fn categorical_clustering_is_permutation_invariant() {
        let mut rows = vec![span("a", POSTGRES), span("b", MYSQL), span("c", POSTGRES)];
        let signature = |rows: &[AskSpan]| {
            cluster_spans(rows)
                .into_iter()
                .map(|row| {
                    (
                        row.memory_id,
                        row.text,
                        row.score.to_bits(),
                        row.provenance_uri,
                    )
                })
                .collect::<Vec<_>>()
        };
        let expected = signature(&rows);
        assert_eq!(expected.len(), 2);
        for _ in 0..rows.len() {
            rows.rotate_left(1);
            assert_eq!(signature(&rows), expected);
            rows.reverse();
            assert_eq!(signature(&rows), expected);
        }
    }
}
