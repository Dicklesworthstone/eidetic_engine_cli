//! Conservative ordering constraints for extractive procedural answers.
//!
//! Bag-of-words agreement is not agreement about execution order: `Run A
//! before B` and `Run B before A` have exactly the same terms. Recognize only
//! complete, unqualified Run/Execute instructions with one before/after edge.
//! This is neither a shell parser nor a general dependency inference engine.
//! Unknown prose, conditions, programs and multiple constraints need an
//! explicit contradiction edge. Parsing never changes quoted evidence.

#[derive(Debug, Eq, PartialEq)]
struct OrderingClaim {
    before: String,
    after: String,
}

/// The same two operations in opposite order are incompatible requirements.
/// Negation inside a quoted command or an option is not English negation;
/// actual prohibitions and qualified instructions are rejected by the parser.
pub(super) fn conflicts(left: &str, right: &str) -> bool {
    let (Some(left), Some(right)) = (claim(left), claim(right)) else {
        return false;
    };
    left.before == right.after && left.after == right.before
}

/// Known ordering requirements corroborate only the same ordered pair.
/// In particular, an unparsed or conditional near-duplicate must not lift a
/// recognized unconditional instruction across the answer confidence floor.
pub(super) fn compatible(left: &str, right: &str) -> bool {
    match (claim(left), claim(right)) {
        (Some(left), Some(right)) => left == right,
        (None, None) => true,
        _ => false,
    }
}

fn claim(text: &str) -> Option<OrderingClaim> {
    let text = text.trim();
    let text = text.strip_suffix('.').unwrap_or(text).trim_end();
    if text.chars().any(char::is_control) || text.contains("``") {
        return None;
    }
    let words = words(text)?;
    let operation = *words.first()?;
    if !operation.eq_ignore_ascii_case("run") && !operation.eq_ignore_ascii_case("execute") {
        return None;
    }
    // Examine complete unquoted words, never substrings of identifiers or
    // quoted commands. Keep scopes and non-asserted advice out of inference.
    if words.iter().any(|word| {
        word.ends_with('.')
            || matches!(
                word.to_ascii_lowercase().as_str(),
                "if" | "unless" | "when" | "whenever" | "while" | "until" | "since"
                    | "during" | "because" | "except" | "otherwise" | "then" | "and"
                    | "or" | "either" | "not" | "never" | "no" | "without" | "only"
                    | "in" | "on" | "at" | "for" | "with" | "may" | "might" | "can"
                    | "could" | "should" | "usually" | "sometimes" | "optionally"
                    | "perhaps" | "probably" | "recommended" | "example"
            )
    }) {
        return None;
    }
    let mut relations = words.iter().enumerate().filter(|(_, word)| {
        word.eq_ignore_ascii_case("before") || word.eq_ignore_ascii_case("after")
    });
    let (position, relation) = relations.next()?;
    if relations.next().is_some() || position <= 1 {
        return None;
    }
    let left = command(&words[1..position])?;
    let mut right = &words[position + 1..];
    // "before running X" is the gerund form of "Run ... before X".
    // Do not strip a bare `run` executable from the command itself.
    if right.first().is_some_and(|word| {
        (operation.eq_ignore_ascii_case("run") && word.eq_ignore_ascii_case("running"))
            || (operation.eq_ignore_ascii_case("execute")
                && word.eq_ignore_ascii_case("executing"))
    }) {
        right = &right[1..];
    }
    let right = command(right)?;
    if left == right {
        return None;
    }
    let (before, after) = if relation.eq_ignore_ascii_case("before") {
        (left, right)
    } else {
        (right, left)
    };
    Some(OrderingClaim { before, after })
}

/// Split at whitespace outside matching single, double or inline-code quotes.
/// A quoted `before` is an argument, not an edge. Escape sequences, shell
/// operators and multiline/code-fence syntax deliberately grant no inference.
fn words(text: &str) -> Option<Vec<&str>> {
    let mut words = Vec::new();
    let mut start = None;
    let mut quote = None;
    for (index, character) in text.char_indices() {
        if character == '\\' {
            return None;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }
            continue;
        }
        if character.is_whitespace() {
            if let Some(start) = start.take() {
                words.push(&text[start..index]);
            }
            continue;
        }
        start.get_or_insert(index);
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            ';' | '|' | '&' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '!' | '?' => {
                return None;
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return None;
    }
    if let Some(start) = start {
        words.push(&text[start..]);
    }
    Some(words)
}

fn command(words: &[&str]) -> Option<String> {
    let first = *words.first()?;
    // Delimiters around a whole command are prose quotation, not part of
    // its identity. Quotes inside arguments and their whitespace stay exact.
    let command = if words.len() == 1 {
        match first.chars().next()? {
            quote @ ('\'' | '"' | '`') => first.strip_prefix(quote)?.strip_suffix(quote)?.to_owned(),
            _ => first.to_owned(),
        }
    } else {
        words.join(" ")
    };
    (!command.trim().is_empty()).then_some(command)
}

#[cfg(test)]
mod tests {
    use super::super::{
        ASK_CANDIDATE_SCAN_CAP, AskCandidate, AskRequest, AskSpan, ask_data_json, cluster_spans,
        evaluate_ask, score_span, tokenize_for_ask,
    };
    use super::*;

    const FORWARD: &str = "Run cargo fmt before cargo test.";
    const REVERSE: &str = "Run cargo test before cargo fmt.";

    fn candidate(id: &str, content: &str) -> AskCandidate {
        AskCandidate {
            memory_id: id.to_owned(),
            content: content.to_owned(),
            confidence: 1.0,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some(format!("manual://ordering/{id}")),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            team_provenance: None,
        }
    }

    fn span(id: &str, text: &str) -> AskSpan {
        AskSpan {
            memory_id: id.to_owned(),
            byte_start: 0,
            byte_end: text.len(),
            text: text.to_owned(),
            score: 0.52,
            trust_class: "human_explicit".to_owned(),
            memory_confidence: 1.0,
            provenance_uri: Some(format!("manual://ordering/{id}")),
            team_provenance: None,
        }
    }

    fn request() -> AskRequest {
        AskRequest {
            question: "Run cargo fmt before cargo test".to_owned(),
            max_evidence: 1,
            ..AskRequest::default()
        }
    }

    #[test]
    fn reversal_is_not_corroboration_despite_identical_lexical_scores() {
        let question = tokenize_for_ask(&request().question);
        assert_eq!(tokenize_for_ask(FORWARD), tokenize_for_ask(REVERSE));
        assert_eq!(
            score_span(&question, FORWARD, 1.0, "human_explicit"),
            score_span(&question, REVERSE, 1.0, "human_explicit"),
        );
        assert!(conflicts(FORWARD, REVERSE));
        assert!(conflicts(REVERSE, FORWARD));
        assert!(!compatible(FORWARD, REVERSE));
        let clusters = cluster_spans(&[span("a", FORWARD), span("b", REVERSE)]);
        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|span| span.score.to_bits() == 0.52_f32.to_bits()));
    }

    #[test]
    fn equivalent_before_after_constraints_corroborate() {
        let equivalent = "Run cargo test after cargo fmt.";
        assert!(compatible(FORWARD, equivalent));
        assert!(!conflicts(FORWARD, equivalent));
        // Compatibility does not bypass the existing Jaccard threshold.
        // These longer equivalent commands also clear that lexical gate.
        let clusters = cluster_spans(&[
            span("a", "Run cargo fmt --all before cargo test --workspace."),
            span("b", "Run cargo test --workspace after cargo fmt --all."),
        ]);
        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].score > 0.52);
    }

    #[test]
    fn same_anchor_with_different_dependencies_is_not_a_conflict_or_support() {
        let alternative = "Run cargo fmt before cargo clippy.";
        assert!(!conflicts(FORWARD, alternative));
        assert!(!compatible(FORWARD, alternative));
    }

    #[test]
    fn quoted_commands_and_gerunds_retain_order() {
        assert!(compatible(FORWARD, "Execute `cargo fmt` before `cargo test`."));
        assert!(compatible(FORWARD, "Run cargo fmt before running cargo test."));
        assert!(conflicts(FORWARD, "Run `cargo fmt` after `cargo test`."));
        assert!(conflicts(
            "Run `printf 'before build'` before release.",
            "Run release before `printf 'before build'`.",
        ));
    }

    #[test]
    fn command_identity_keeps_case_arguments_and_quoted_whitespace() {
        assert!(!conflicts("Run Build before Test.", "Run test before build."));
        assert!(!conflicts("Run deploy --prod before verify.", "Run verify before deploy --stage."));
        assert!(!compatible("Run `echo  x` before test.", "Run `echo x` before test."));
        assert!(!conflicts("Run cargo fmt before run tests.", "Run tests before cargo fmt."));
    }

    #[test]
    fn modifiers_programs_and_ambiguous_syntax_do_not_grant_inference() {
        for text in [
            "Never run cargo fmt before cargo test.",
            "Run not cargo fmt before cargo test.",
            "Run cargo fmt before cargo test if ready.",
            "Run cargo fmt before cargo test in production.",
            "Run cargo fmt before cargo test or cargo clippy.",
            "Run cargo fmt before cargo test and release.",
            "Run cargo fmt before cargo test before release.",
            "Run cargo fmt before cargo test; release.",
            "Run cargo fmt before cargo test. Release.",
            "Run cargo fmt before cargo test\nRelease.",
            "Run `cargo fmt before cargo test`.",
            "Run `cargo fmt before cargo test.",
            "Run cargo fmt before.",
            "Run before cargo test.",
            "Run cargo fmt before cargo fmt.",
            "Run ``cargo fmt`` before cargo test.",
            "Run cargo fmt | cargo test before release.",
            "Run \"cargo \\\"fmt\\\"\" before release.",
        ] {
            assert!(claim(text).is_none(), "must not infer an ordering from {text:?}");
            assert!(!conflicts(text, REVERSE));
        }
    }

    #[test]
    fn qualified_near_duplicates_cannot_corroborate_a_known_constraint() {
        assert!(!compatible(FORWARD, "Run cargo fmt before cargo test if ready."));
        assert!(compatible("Unparsed prose.", "Other unparsed prose."));
    }

    #[test]
    fn unquoted_command_options_are_not_english_prohibitions() {
        assert!(conflicts(
            "Run deploy --no-cache before verify.",
            "Run verify before deploy --no-cache.",
        ));
        assert!(!compatible(
            "Run deploy --no-cache before verify.",
            "Run verify before deploy --no-cache.",
        ));
    }

    #[test]
    fn direction_comparison_is_symmetric_and_does_not_invent_transitive_conflicts() {
        let commands = ["cargo fmt", "cargo test", "db migrate", "deploy --no-cache"];
        for (a, first) in commands.iter().enumerate() {
            for (b, second) in commands.iter().enumerate() {
                if a == b {
                    continue;
                }
                let left = format!("Run {first} before {second}.");
                let equivalent = format!("Run {second} after {first}.");
                assert!(compatible(&left, &equivalent));
                for (c, third) in commands.iter().enumerate() {
                    for (d, fourth) in commands.iter().enumerate() {
                        if c == d {
                            continue;
                        }
                        let right = format!("Run {third} before {fourth}.");
                        assert_eq!(conflicts(&left, &right), a == d && b == c);
                        assert_eq!(conflicts(&left, &right), conflicts(&right, &left));
                        assert_eq!(compatible(&left, &right), a == c && b == d);
                    }
                }
            }
        }
    }

    #[test]
    fn public_ask_exposes_two_byte_exact_sides_not_a_chosen_order() {
        let candidates = [candidate("a", FORWARD), candidate("b", REVERSE)];
        let report = evaluate_ask(&request(), &candidates);
        assert!(!report.abstained && report.conflict_detected);
        assert!(!report.extractiveness_violated);
        assert!(report.answer_text.is_none() && report.citations.is_empty());
        assert!(report.conflict_link.is_none());
        assert_eq!(report.confidence_components.corroboration, 1.0);
        let sides = report.sides.as_ref().unwrap();
        assert_eq!(sides.len(), 2);
        assert_eq!(sides[1].label, "ordering_alternative");
        for (side, original) in sides.iter().zip(&candidates) {
            assert_eq!(side.citations.len(), 1);
            let citation = &side.citations[0];
            assert_eq!(citation.memory_id, original.memory_id);
            assert_eq!(citation.text, original.content);
            assert_eq!(citation.provenance_uri, original.provenance_uri);
            assert_eq!(citation.trust_class, original.trust_class);
            assert_eq!(citation.confidence, original.confidence);
            assert_eq!(
                &original.content[citation.byte_start..citation.byte_end],
                citation.text,
            );
        }
    }

    #[test]
    fn reversed_advice_cannot_supply_the_bonus_that_turns_abstention_into_an_answer() {
        let request = AskRequest {
            question: "cargo fmt production release".to_owned(),
            ..request()
        };
        let candidates = [
            candidate("a", FORWARD),
            candidate("b", FORWARD),
            candidate("c", REVERSE),
        ];
        // Two agreeing sources remain just below the default evidence floor.
        // Counting the reversed instruction as a third vote would cross it.
        let report = evaluate_ask(&request, &candidates);
        assert!(report.abstained);
        assert!(report.confidence < request.min_confidence);
        assert!(report.answer_text.is_none() && report.citations.is_empty());
        assert!(!report.extractiveness_violated);
    }

    #[test]
    fn public_conflict_report_is_input_order_independent() {
        let mut candidates = [candidate("a", FORWARD), candidate("b", REVERSE)];
        let first = ask_data_json(&evaluate_ask(&request(), &candidates));
        candidates.reverse();
        assert_eq!(first, ask_data_json(&evaluate_ask(&request(), &candidates)));
    }

    #[test]
    fn relevance_budget_does_not_hide_the_reverse_order() {
        let mut candidates: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 8)
            .map(|index| candidate(&format!("a-{index:04}"), FORWARD))
            .collect();
        candidates.push(candidate("z-opposing-order", REVERSE));
        let report = evaluate_ask(&request(), &candidates);
        assert!(!report.abstained && report.conflict_detected);
        assert_eq!(report.candidates_scanned, candidates.len());
        assert!(report.sides.as_ref().unwrap().iter().any(|side| {
            side.citations.iter().any(|citation| citation.memory_id == "z-opposing-order")
        }));
        let expected = ask_data_json(&report);
        candidates.reverse();
        assert_eq!(expected, ask_data_json(&evaluate_ask(&request(), &candidates)));
    }

    #[test]
    fn unrelated_and_under_floor_orderings_do_not_manufacture_conflict() {
        let mut low_trust = candidate("b", REVERSE);
        low_trust.confidence = 0.0;
        let strict = AskRequest { min_confidence: 0.95, ..request() };
        let report = evaluate_ask(&strict, &[candidate("a", FORWARD), low_trust]);
        assert!(!report.abstained && !report.conflict_detected);
        assert_eq!(report.citations.len(), 1);
        assert_eq!(report.citations[0].memory_id, "a");
        let report = evaluate_ask(&request(), &[
            candidate("a", FORWARD), candidate("b", "Run backup before archive."),
        ]);
        assert!(!report.conflict_detected);
    }
}
