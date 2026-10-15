//! Conservative ordering constraints for extractive procedural answers.
//!
//! Bag-of-words agreement is not agreement about execution order: `Run A
//! before B` and `Run B before A` have exactly the same terms. Recognize only
//! complete, unqualified Run/Execute instructions with one before/after edge
//! or a bounded `A then B then C` sequence. A sequence entails the order of
//! every pair of its steps, not just adjacent steps. Partial agreement cannot
//! corroborate the rest of a procedure. This is neither a shell parser nor a
//! general dependency inference engine: conditions, programs, loops and mixed
//! relation syntax still need an explicit contradiction edge. Parsing never
//! changes the evidence text or citation offsets.

#[path = "ask_command_literals.rs"]
mod literals;

const MAX_ORDERING_BYTES: usize = 8 * 1024;
const MAX_SEQUENCE_COMMANDS: usize = 8;

#[derive(Debug, Eq, PartialEq)]
struct OrderingClaim {
    // Canonical execution order, independent of before/after/then spelling.
    // Commands retain case, arguments and whitespace inside quoted bodies.
    operations: Vec<String>,
}

/// Any shared pair in opposite order is an incompatible requirement, including
/// non-adjacent steps. A pair absent from either claim supplies no constraint.
/// Negation inside a quoted command or an option is not English negation;
/// actual prohibitions and qualified instructions are rejected by the parser.
pub(super) fn conflicts(left: &str, right: &str) -> bool {
    let (Some(left), Some(right)) = (claim(left), claim(right)) else {
        return false;
    };
    left.operations.iter().enumerate().any(|(index, before)| {
        let Some(right_before) = right.operations.iter().position(|step| step == before) else {
            return false;
        };
        left.operations[index + 1..].iter().any(|after| {
            right
                .operations
                .iter()
                .position(|step| step == after)
                .is_some_and(|right_after| right_after < right_before)
        })
    })
}

/// Known ordering requirements corroborate only the same complete sequence.
/// Agreeing with one edge of a longer procedure does not support its other steps.
/// In particular, an unparsed or conditional near-duplicate must not lift a
/// recognized unconditional instruction across the answer confidence floor.
/// Unparsed ordering prose still has to agree on command and code literals.
pub(super) fn compatible(left: &str, right: &str) -> bool {
    match (claim(left), claim(right)) {
        (Some(left), Some(right)) => left == right,
        (None, None) => literals::compatible(left, right),
        _ => false,
    }
}

fn claim(text: &str) -> Option<OrderingClaim> {
    let text = text.trim();
    let text = text.strip_suffix('.').unwrap_or(text).trim_end();
    if text.len() > MAX_ORDERING_BYTES || text.chars().any(char::is_control) || text.contains("``")
    {
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
                "if" | "unless"
                    | "when"
                    | "whenever"
                    | "while"
                    | "until"
                    | "since"
                    | "during"
                    | "because"
                    | "except"
                    | "otherwise"
                    | "and"
                    | "or"
                    | "either"
                    | "not"
                    | "never"
                    | "no"
                    | "without"
                    | "only"
                    | "in"
                    | "on"
                    | "at"
                    | "for"
                    | "with"
                    | "may"
                    | "might"
                    | "can"
                    | "could"
                    | "should"
                    | "usually"
                    | "sometimes"
                    | "optionally"
                    | "perhaps"
                    | "probably"
                    | "recommended"
                    | "example"
            )
    }) {
        return None;
    }
    if words.iter().any(|word| word.eq_ignore_ascii_case("then")) {
        // Combining before/after with then is not an unambiguous linear list.
        // Quoted relation words are part of command identity, not delimiters.
        if words
            .iter()
            .any(|word| word.eq_ignore_ascii_case("before") || word.eq_ignore_ascii_case("after"))
        {
            return None;
        }
        return sequence(&words[1..]);
    }
    // Commas are admitted only immediately before an explicit sequence edge.
    // Do not broaden the original single-edge grammar to arbitrary lists.
    if words.contains(&",") {
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
            || (operation.eq_ignore_ascii_case("execute") && word.eq_ignore_ascii_case("executing"))
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
    Some(OrderingClaim {
        operations: vec![before, after],
    })
}

/// Parse an explicit sequence without interpreting shell syntax or inventing
/// omitted verbs. `run` in a later command remains an executable, just as it
/// does in a before/after operand. Repeated commands may describe a retry loop;
/// they are not a total order and therefore grant no ordering inference.
fn sequence(words: &[&str]) -> Option<OrderingClaim> {
    let mut segments = words
        .split(|word| word.eq_ignore_ascii_case("then"))
        .peekable();
    let mut operations = Vec::new();
    while let Some(segment) = segments.next() {
        if operations.len() == MAX_SEQUENCE_COMMANDS {
            return None;
        }
        let segment = if segments.peek().is_some() && segment.last() == Some(&",") {
            &segment[..segment.len() - 1]
        } else {
            segment
        };
        if segment.contains(&",") {
            return None;
        }
        let operation = command(segment)?;
        if operations.contains(&operation) {
            return None;
        }
        operations.push(operation);
    }
    (operations.len() >= 2).then_some(OrderingClaim { operations })
}

/// Split at whitespace and commas outside matching single, double or inline-code quotes.
/// Quoted `before`, `after`, `then` and commas remain arguments. Escape sequences, shell
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
        if character == ',' {
            if let Some(start) = start.take() {
                words.push(&text[start..index]);
            }
            words.push(&text[index..index + 1]);
            continue;
        }
        start.get_or_insert(index);
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            ';' | '|' | '&' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '!' | '?' => {
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
            quote @ ('\'' | '"' | '`') => {
                first.strip_prefix(quote)?.strip_suffix(quote)?.to_owned()
            }
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
        assert!(
            clusters
                .iter()
                .all(|span| span.score.to_bits() == 0.52_f32.to_bits())
        );
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
        assert!(compatible(
            FORWARD,
            "Execute `cargo fmt` before `cargo test`."
        ));
        assert!(compatible(
            FORWARD,
            "Run cargo fmt before running cargo test."
        ));
        assert!(conflicts(FORWARD, "Run `cargo fmt` after `cargo test`."));
        assert!(conflicts(
            "Run `printf 'before build'` before release.",
            "Run release before `printf 'before build'`.",
        ));
    }

    #[test]
    fn command_identity_keeps_case_arguments_and_quoted_whitespace() {
        assert!(!conflicts(
            "Run Build before Test.",
            "Run test before build."
        ));
        assert!(!conflicts(
            "Run deploy --prod before verify.",
            "Run verify before deploy --stage."
        ));
        assert!(!compatible(
            "Run `echo  x` before test.",
            "Run `echo x` before test."
        ));
        assert!(!conflicts(
            "Run cargo fmt before run tests.",
            "Run tests before cargo fmt."
        ));
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
            assert!(
                claim(text).is_none(),
                "must not infer an ordering from {text:?}"
            );
            assert!(!conflicts(text, REVERSE));
        }
    }

    #[test]
    fn qualified_near_duplicates_cannot_corroborate_a_known_constraint() {
        assert!(!compatible(
            FORWARD,
            "Run cargo fmt before cargo test if ready."
        ));
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
            side.citations
                .iter()
                .any(|citation| citation.memory_id == "z-opposing-order")
        }));
        let expected = ask_data_json(&report);
        candidates.reverse();
        assert_eq!(
            expected,
            ask_data_json(&evaluate_ask(&request(), &candidates))
        );
    }

    #[test]
    fn unrelated_and_under_floor_orderings_do_not_manufacture_conflict() {
        let mut low_trust = candidate("b", REVERSE);
        low_trust.confidence = 0.0;
        let strict = AskRequest {
            min_confidence: 0.95,
            ..request()
        };
        let report = evaluate_ask(&strict, &[candidate("a", FORWARD), low_trust]);
        assert!(!report.abstained && !report.conflict_detected);
        assert_eq!(report.citations.len(), 1);
        assert_eq!(report.citations[0].memory_id, "a");
        let report = evaluate_ask(
            &request(),
            &[
                candidate("a", FORWARD),
                candidate("b", "Run backup before archive."),
            ],
        );
        assert!(!report.conflict_detected);
    }

    #[test]
    fn explicit_sequences_share_the_existing_before_after_contract() {
        assert!(compatible(FORWARD, "Run cargo fmt then cargo test."));
        assert!(compatible(
            FORWARD,
            "Execute `cargo fmt`, then `cargo test`."
        ));
        assert!(conflicts(FORWARD, "Run cargo test then cargo fmt."));
        assert!(compatible(
            "Run cargo fmt then cargo test then cargo build.",
            "Execute `cargo fmt`, THEN `cargo test`, then `cargo build`.",
        ));
    }

    #[test]
    fn nonadjacent_steps_can_contradict_a_sequence() {
        let sequence = "Run cargo fmt then cargo test then cargo build.";
        for opposition in [
            "Run cargo build before cargo fmt.",
            "Run cargo fmt after cargo build.",
            "Run cargo build then cargo fmt.",
            "Run cargo fmt then cargo build then cargo test.",
        ] {
            assert!(conflicts(sequence, opposition), "{opposition}");
            assert!(conflicts(opposition, sequence), "{opposition}");
            assert!(!compatible(sequence, opposition), "{opposition}");
        }
    }

    #[test]
    fn partial_sequence_agreement_cannot_corroborate_unshared_steps() {
        let sequence = "Run cargo fmt then cargo test then cargo build.";
        for partial in [
            FORWARD,
            "Run cargo fmt before cargo build.",
            "Run cargo test then cargo build.",
            "Run cargo fmt then cargo test then cargo clippy.",
        ] {
            assert!(!conflicts(sequence, partial), "{partial}");
            assert!(!conflicts(partial, sequence), "{partial}");
            assert!(!compatible(sequence, partial), "{partial}");
            assert!(!compatible(partial, sequence), "{partial}");
        }
        // A path through an operation absent from the other claim is not a
        // contradiction, even when the English task descriptions overlap.
        assert!(!conflicts("Run alpha then beta.", "Run beta then gamma."));
    }

    #[test]
    fn sequence_delimiters_inside_quotes_are_not_edges() {
        assert!(conflicts(
            "Run `printf 'then, before, after'` then cargo test.",
            "Run cargo test then `printf 'then, before, after'`.",
        ));
        assert!(conflicts(
            "Run echo 'then, before' then verify --no-cache.",
            "Run verify --no-cache before echo 'then, before'.",
        ));
        assert!(claim("Run `alpha then beta`.").is_none());
        assert!(!conflicts(
            "Run `echo  x` then Test.",
            "Run Test then `echo x`.",
        ));
        assert!(!conflicts("Run Build then Test.", "Run test then build."));
        assert!(!conflicts(
            "Run deploy --prod then verify.",
            "Run verify then deploy --stage.",
        ));
        assert!(!conflicts(
            "Run alpha then run beta.",
            "Run beta then alpha."
        ));
    }

    #[test]
    fn ambiguous_or_qualified_sequences_do_not_grant_inference() {
        for text in [
            "Run alpha then beta if ready.",
            "Run alpha then beta unless blocked.",
            "Run alpha then beta in production.",
            "Run alpha then beta or gamma.",
            "Run alpha and then beta.",
            "Run alpha then never beta.",
            "Run alpha then beta before gamma.",
            "Run alpha after beta then gamma.",
            "Run alpha, beta then gamma.",
            "Run alpha, then, beta.",
            "Run alpha,, then beta.",
            "Run alpha then beta,.",
            "Run then beta.",
            "Run alpha then.",
            "Run alpha then then beta.",
            "Run alpha then alpha.",
            "Run alpha then beta then alpha.",
            "Run `alpha` then alpha.",
            "Run alpha then beta; gamma.",
            "Run alpha then beta | gamma.",
            "Run alpha then beta && gamma.",
            "Run alpha then beta\ngamma.",
            "Run alpha then `beta.",
            "Run alpha then ``beta``.",
        ] {
            assert!(claim(text).is_none(), "must not infer from {text:?}");
            assert!(!conflicts(text, "Run beta then alpha."));
            assert!(!compatible(text, "Run alpha then beta."));
        }
    }

    #[test]
    fn sequence_limits_refuse_the_whole_claim_not_a_truncated_prefix() {
        let operations = (0..MAX_SEQUENCE_COMMANDS)
            .map(|index| format!("step_{index}"))
            .collect::<Vec<_>>();
        let at_limit = format!("Run {}.", operations.join(" then "));
        assert_eq!(claim(&at_limit).unwrap().operations, operations);
        let over_limit = format!("Run {} then extra.", operations.join(" then "));
        assert!(claim(&over_limit).is_none());
        let padding = "x".repeat(MAX_ORDERING_BYTES - "Run  then beta".len());
        let at_bytes = format!("Run {padding} then beta.");
        assert!(claim(&at_bytes).is_some());
        assert!(claim(&format!("Run {padding}x then beta.")).is_none());
        let unicode = "資料".repeat(MAX_ORDERING_BYTES / 3);
        assert!(claim(&format!("Run {unicode} then beta.")).is_none());
    }

    #[test]
    fn sequence_pair_conflicts_match_an_independent_graph_cycle_oracle() {
        fn chains(prefix: &mut Vec<usize>, output: &mut Vec<Vec<usize>>) {
            if prefix.len() >= 2 {
                output.push(prefix.clone());
            }
            for next in 0..4 {
                if !prefix.contains(&next) {
                    prefix.push(next);
                    chains(prefix, output);
                    prefix.pop();
                }
            }
        }
        fn text(chain: &[usize]) -> String {
            format!(
                "Run {}.",
                chain
                    .iter()
                    .map(|index| format!("cmd_{index}"))
                    .collect::<Vec<_>>()
                    .join(" then ")
            )
        }
        let mut cases = Vec::new();
        chains(&mut Vec::new(), &mut cases);
        assert_eq!(cases.len(), 60);
        for left in &cases {
            for right in &cases {
                let mut reachable = [[false; 4]; 4];
                for chain in [left, right] {
                    for edge in chain.windows(2) {
                        reachable[edge[0]][edge[1]] = true;
                    }
                }
                for via in 0..4 {
                    for from in 0..4 {
                        for to in 0..4 {
                            let path = reachable[from][via] && reachable[via][to];
                            reachable[from][to] |= path;
                        }
                    }
                }
                let cycle = (0..4).any(|index| reachable[index][index]);
                let (left_text, right_text) = (text(left), text(right));
                assert_eq!(
                    conflicts(&left_text, &right_text),
                    cycle,
                    "{left:?} / {right:?}"
                );
                assert_eq!(compatible(&left_text, &right_text), left == right);
            }
        }
    }

    #[test]
    fn reversed_sequences_do_not_receive_corroboration_credit() {
        let forward = "Run cargo fmt then cargo test then cargo build.";
        let reverse = "Run cargo build then cargo test then cargo fmt.";
        assert_eq!(tokenize_for_ask(forward), tokenize_for_ask(reverse));
        let clusters = cluster_spans(&[span("a", forward), span("b", reverse)]);
        assert_eq!(clusters.len(), 2);
        assert!(
            clusters
                .iter()
                .all(|span| span.score.to_bits() == 0.52_f32.to_bits())
        );
    }

    #[test]
    fn public_ask_keeps_conflicting_sequence_citations_exact_and_deterministic() {
        let forward = "Run cargo fmt then cargo test then cargo build.";
        let reverse = "Run cargo build then cargo test then cargo fmt.";
        let request = AskRequest {
            question: forward.to_owned(),
            max_evidence: 1,
            ..AskRequest::default()
        };
        let mut candidates = [candidate("a", forward), candidate("b", reverse)];
        let report = evaluate_ask(&request, &candidates);
        assert!(!report.abstained && report.conflict_detected);
        assert!(!report.extractiveness_violated);
        assert!(report.answer_text.is_none() && report.citations.is_empty());
        assert_eq!(report.confidence_components.corroboration, 1.0);
        let sides = report.sides.as_ref().unwrap();
        assert_eq!(sides.len(), 2);
        assert_eq!(sides[1].label, "ordering_alternative");
        for side in sides {
            for citation in &side.citations {
                let original = candidates
                    .iter()
                    .find(|candidate| candidate.memory_id == citation.memory_id)
                    .unwrap();
                assert_eq!(citation.text, original.content);
                assert_eq!(citation.provenance_uri, original.provenance_uri);
                assert_eq!(
                    &original.content[citation.byte_start..citation.byte_end],
                    citation.text,
                );
            }
        }
        candidates.reverse();
        assert_eq!(
            ask_data_json(&report),
            ask_data_json(&evaluate_ask(&request, &candidates))
        );
    }
}
