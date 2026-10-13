//! Literal agreement for procedural evidence, not command execution or inference.
//!
//! Lexical similarity discards case, punctuation, argument order and quoting.
//! Those are meaningful in code. Withhold corroboration when commands, quoted
//! code, paths or switches disagree; never invent a contradiction from this
//! guard. The existing relevance, polarity and confidence gates still apply.

use super::super::{
    ask_byte_is_escaped, ask_fence_line, ask_inline_code_ends, ask_list_content_start,
};

struct LiteralText<'a> {
    /// Markdown code delimiters are presentation, not executable bytes. The
    /// payload itself (including quoting and whitespace) remains untouched.
    plain: String,
    code: Vec<&'a str>,
}

pub(super) fn compatible(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    // A fenced program is an indivisible source span. Do not infer program
    // equivalence from its words, including reordered lines or changed pipes.
    if has_fence(left) || has_fence(right) {
        return false;
    }
    let (Some(left), Some(right)) = (literal_text(left), literal_text(right)) else {
        // Unbalanced markup supplies no safe literal interpretation.
        return false;
    };
    if !contains_ordered_literals(&right.plain, &left.code)
        || !contains_ordered_literals(&left.plain, &right.code)
        || machine_tokens(&left.plain) != machine_tokens(&right.plain)
    {
        return false;
    }
    let (Some(left_assignments), Some(right_assignments)) = (
        assignment_literals(&left.plain),
        assignment_literals(&right.plain),
    ) else {
        return false;
    };
    if left_assignments != right_assignments {
        return false;
    }
    if option_literals(&left.plain)
        .zip(option_literals(&right.plain))
        .is_none_or(|(left, right)| left != right)
    {
        return false;
    }
    match (
        instruction_payload(&left.plain),
        instruction_payload(&right.plain),
    ) {
        (Some(left), Some(right)) => left == right,
        (None, None) => true,
        _ => false,
    }
}

fn has_fence(text: &str) -> bool {
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        if ask_fence_line(text.as_bytes(), start, end).is_some() {
            return true;
        }
        start = end;
    }
    false
}

fn literal_text(text: &str) -> Option<LiteralText<'_>> {
    let bytes = text.as_bytes();
    // Share the evidence segmenter's delimiter and escape rules, including
    // arbitrary run lengths and literal backslashes inside a code span.
    let ends = ask_inline_code_ends(bytes, 0, bytes.len());
    let mut plain = String::with_capacity(text.len());
    let mut code = Vec::new();
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let width = bytes[index..]
            .iter()
            .take_while(|&&byte| byte == b'`')
            .count();
        if ask_byte_is_escaped(bytes, 0, index) {
            index += width;
            continue;
        }
        let end = *ends.get(&index)?;
        let payload = &text[index + width..end - width];
        // The delimiter helper always points at a later complete run.
        plain.push_str(&text[copied..index]);
        plain.push_str(payload);
        code.push(payload);
        index = end;
        copied = end;
    }
    plain.push_str(&text[copied..]);
    Some(LiteralText { plain, code })
}

fn identifier_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '$')
}

/// Marked literals may match byte-identical unmarked text, but not substrings
/// of other identifiers. Keeping their order prevents `copy A to B` from
/// corroborating `copy B to A` merely because both names occur in each source.
fn contains_ordered_literals(text: &str, literals: &[&str]) -> bool {
    let mut offset = 0;
    for &literal in literals {
        if literal.is_empty() {
            continue;
        }
        let Some(position) = text[offset..]
            .match_indices(literal)
            .find_map(|(start, _)| {
                let start = offset + start;
                let end = start + literal.len();
                let left = literal.chars().next().is_none_or(|first| {
                    !identifier_char(first)
                        || text[..start]
                            .chars()
                            .next_back()
                            .is_none_or(|previous| !identifier_char(previous))
                });
                let right = literal.chars().next_back().is_none_or(|last| {
                    !identifier_char(last)
                        || text[end..]
                            .chars()
                            .next()
                            .is_none_or(|next| !identifier_char(next))
                });
                (left && right).then_some(end)
            })
        else {
            return false;
        };
        offset = position;
    }
    true
}

/// Bare paths and flags have the same identity constraints as marked code.
/// Preserve signs, separators, spelling and order; do not resolve paths,
/// expand variables, case-fold names or consult the filesystem.
fn machine_tokens(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .map(|word| word.trim_matches(['"', '\'', '(', ')', '[', ']', '{', '}', ',', ';']))
        .filter(|word| {
            (word.starts_with('-') && word.chars().any(char::is_alphabetic))
                || word.contains('/')
                || word.contains('\\')
                || word.contains("::")
        })
        .collect()
}

/// Assignment values are executable/configuration data even without Markdown
/// or an imperative. Compare the complete words, not their case-folded tokens.
/// Quoted and escaped whitespace is part of the value. This is a lexical guard,
/// not a shell parser: unclosed quoting and unquoted command/parameter expansion
/// cannot supply corroboration between different source spans.
fn assignment_literals(text: &str) -> Option<Vec<&str>> {
    let bytes = text.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'=' || bytes.get(index + 1) == Some(&b'=') {
            index += 1;
            continue;
        }
        let mut start = index;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        if start == index
            || !(bytes[start].is_ascii_alphabetic() || bytes[start] == b'_')
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(|previous| identifier_char(previous) || matches!(previous, '.' | ':'))
        {
            index += 1;
            continue;
        }
        let end = literal_word_end(text, index + 1)?;
        literals.push(&text[start..end]);
        index = end;
    }
    Some(literals)
}

fn literal_word_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = start;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' && quote != Some(b'\'') {
            // An escape belongs to this word, including an escaped space.
            if index + 1 == bytes.len() {
                return None;
            }
            index += 2;
            continue;
        }
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte.is_ascii_whitespace() {
            break;
        } else if byte == b'$' && matches!(bytes.get(index + 1), Some(b'(' | b'{')) {
            return None;
        }
        index += 1;
    }
    quote.is_none().then_some(index)
}

/// Bind each option to its following lexical word as well as its own bytes.
/// An option's arity is not inferred: the neighboring word is evidence that
/// must agree, not a parsed argument to execute. This also preserves values
/// for boolean-looking options and options unknown to this version of ee.
fn option_literals(text: &str) -> Option<Vec<(&str, Option<&str>)>> {
    let bytes = text.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'-'
            || (index > 0
                && !bytes[index - 1].is_ascii_whitespace()
                && !matches!(bytes[index - 1], b'(' | b'[' | b'{'))
        {
            index += 1;
            continue;
        }
        let end = literal_word_end(text, index)?;
        let option = &text[index..end];
        if option != "--" && !option.chars().any(char::is_alphabetic) {
            index = end;
            continue;
        }
        index = end;
        let argument = if option.contains('=') {
            // The whole quoted/escaped value is already in this word.
            None
        } else {
            let mut start = end;
            while start < bytes.len() && bytes[start].is_ascii_whitespace() {
                start += 1;
            }
            if start == bytes.len() {
                None
            } else {
                let next_end = literal_word_end(text, start)?;
                // Do not reinterpret an option-looking substring inside a
                // quoted argument. A following real option still gets its
                // own neighborhood so its value cannot disappear.
                if bytes[start] != b'-' {
                    index = next_end;
                }
                Some(&text[start..next_end])
            }
        };
        literals.push((option, argument));
    }
    Some(literals)
}

/// An explicit imperative or command label introduces an opaque payload.
/// Preserve any preceding condition as well as the full tail; finding `run`
/// inside a qualified statement must not erase its workspace or environment.
/// The ordering parser handles its stronger before/after equivalence before
/// this fallback is called. No command is parsed, normalized or executed here.
fn instruction_payload(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let start = ask_list_content_start(text.as_bytes(), 0, text.len()).unwrap_or(0);
    let text = &text[start..];
    let mut offset = 0;
    for word in text.split_whitespace() {
        let start = offset + text[offset..].find(word)?;
        let end = start + word.len();
        offset = end;
        let verb = word.strip_suffix(':').unwrap_or(word);
        let prefix = text[..start].trim_end();
        let command_boundary = prefix.is_empty()
            || prefix.ends_with([',', ';', ':'])
            || prefix.split_whitespace().next_back().is_some_and(|word| {
                ["please", "should", "must"]
                    .iter()
                    .any(|expected| word.eq_ignore_ascii_case(expected))
            });
        let imperative = command_boundary
            && ["run", "execute", "invoke"]
                .iter()
                .any(|expected| verb.eq_ignore_ascii_case(expected));
        let label = word.ends_with(':')
            && ["command", "invocation"]
                .iter()
                .any(|expected| verb.eq_ignore_ascii_case(expected));
        if imperative || label {
            let tail = text[end..].trim_start();
            if !tail.is_empty() {
                return Some((&text[..start], tail));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ask::{
        ASK_MIN_CONFIDENCE_DEFAULT, AskCandidate, AskRequest, AskSpan, ask_data_json, clustering,
        evaluate_ask, evaluate_ask_scored, score_span, tokenize_for_ask,
    };

    const SOFT: &str = "Run git reset --soft HEAD in the workspace before starting the release.";
    const HARD: &str = "Run git reset --hard HEAD in the workspace before starting the release.";

    fn candidate(id: &str, content: &str) -> AskCandidate {
        AskCandidate {
            memory_id: id.to_owned(),
            content: content.to_owned(),
            confidence: 1.0,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some(format!("manual://literal/{id}")),
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
            provenance_uri: Some(format!("manual://literal/{id}")),
            team_provenance: None,
        }
    }

    #[test]
    fn different_flags_cannot_manufacture_confidence_or_hide_a_command() {
        let left = tokenize_for_ask(SOFT);
        let right = tokenize_for_ask(HARD);
        let intersection = left.iter().filter(|term| right.contains(term)).count();
        let similarity = intersection as f32 / (left.len() + right.len() - intersection) as f32;
        assert!(similarity >= super::super::super::CLUSTER_SIMILARITY_THRESHOLD);
        let clusters = clustering::cluster_spans(&[span("a", SOFT), span("b", HARD)]);
        assert_eq!(clusters.len(), 2);
        for item in clusters {
            assert_eq!(item.score.to_bits(), 0.52_f32.to_bits());
            assert!(item.score < ASK_MIN_CONFIDENCE_DEFAULT);
            assert_eq!(item.byte_end, item.text.len());
        }
    }

    #[test]
    fn command_identity_keeps_order_case_operators_and_quoted_whitespace() {
        for (left, right) in [
            ("Run copy source target.", "Run copy target source."),
            ("Run Build release.", "Run build release."),
            ("Run test && deploy.", "Run test ; deploy."),
            ("Run printf 'A  B'.", "Run printf 'A B'."),
            ("Run deploy $TARGET.", "Run deploy TARGET."),
            ("Run deploy --mode=prod.", "Run deploy --mode=stage."),
            ("Run cargo test --all.", "Run cargo test --all if ready."),
        ] {
            assert!(!compatible(left, right), "{left:?} / {right:?}");
            assert!(!compatible(right, left), "reverse {left:?} / {right:?}");
            assert!(!super::super::conflicts(left, right));
        }
    }

    #[test]
    fn inline_code_is_literal_even_inside_nearly_identical_prose() {
        for (left, right) in [
            (
                "The recommended invocation is `git reset --soft HEAD` for this task.",
                "The recommended invocation is `git reset --hard HEAD` for this task.",
            ),
            (
                "Copy `source` to `target` for the release.",
                "Copy `target` to `source` for the release.",
            ),
            (
                "Use `Build` for the release.",
                "Use `build` for the release.",
            ),
            ("Use `foo` for the release.", "Use foobar for the release."),
            (
                "Use `foo` for the release.",
                "Use foo-extra for the release.",
            ),
            (
                "Use ``echo `a`  b`` for the release.",
                "Use ``echo `a` b`` for the release.",
            ),
        ] {
            assert!(!compatible(left, right));
            assert!(!compatible(right, left));
        }
    }

    #[test]
    fn paths_namespaces_and_short_options_are_not_case_folded_or_reordered() {
        for (left, right) in [
            (
                "Use src/Main.rs for release.",
                "Use src/main.rs for release.",
            ),
            (
                "Copy src/first to src/second.",
                "Copy src/second to src/first.",
            ),
            ("Use tool -f for release.", "Use tool -F for release."),
            (
                "Use library::Build for release.",
                "Use library::build for release.",
            ),
            (r"Use C:\Build for release.", r"Use C:\build for release."),
        ] {
            assert!(!compatible(left, right));
            assert!(!compatible(right, left));
        }
    }

    #[test]
    fn formatting_does_not_remove_real_agreement() {
        for (left, right) in [
            ("Run `cargo test --all`.", "Execute cargo test --all."),
            (
                "The command is `cargo test --all`.",
                "The command is cargo test --all.",
            ),
            (
                "Use `src/main.rs` for the release.",
                "Use src/main.rs for the release.",
            ),
            ("BACKEND: `sqlite`", "BACKEND: sqlite"),
            ("Use `café` for release.", "Use café for release."),
            (
                "Ordinary prose remains eligible.",
                "Ordinary prose is still eligible.",
            ),
        ] {
            assert!(compatible(left, right), "{left:?} / {right:?}");
            assert!(compatible(right, left));
        }
        let clusters = clustering::cluster_spans(&[span("a", SOFT), span("b", SOFT)]);
        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].score > ASK_MIN_CONFIDENCE_DEFAULT);
    }

    #[test]
    fn bare_assignments_preserve_values_case_order_and_quoted_whitespace() {
        for (left, right) in [
            (
                "Set BACKEND=sqlite for this workspace.",
                "Set BACKEND=postgres for this workspace.",
            ),
            (
                "Use MODE=Release during deployment.",
                "Use MODE=release during deployment.",
            ),
            (
                "Use MODE=release during deployment.",
                "Use mode=release during deployment.",
            ),
            (
                "Use A=one B=two for deployment.",
                "Use B=two A=one for deployment.",
            ),
            (
                "Use LABEL='A  B' for deployment.",
                "Use LABEL='A B' for deployment.",
            ),
            (
                "Use LABEL='A' for deployment.",
                "Use LABEL=\"A\" for deployment.",
            ),
            (
                "Use TARGET=$DEST for deployment.",
                "Use TARGET=DEST for deployment.",
            ),
            (
                "Use FILTER= for deployment.",
                "Use FILTER=all for deployment.",
            ),
            (
                r"Use LABEL=A\ B for deployment.",
                r"Use LABEL=A\ C for deployment.",
            ),
            (
                "Use LABEL='資料 完了' for deployment.",
                "Use LABEL='資料 未完了' for deployment.",
            ),
        ] {
            assert!(!compatible(left, right), "{left:?} / {right:?}");
            assert!(!compatible(right, left), "reverse {left:?} / {right:?}");
        }
        assert_eq!(
            assignment_literals("Set A='one two' B=\"three four\" C=five\\ six D="),
            Some(vec![
                "A='one two'",
                "B=\"three four\"",
                "C=five\\ six",
                "D="
            ])
        );
        assert_eq!(assignment_literals("x == y and x != y"), Some(vec![]));
    }

    #[test]
    fn assignment_markup_agrees_without_loosening_incomplete_values() {
        for (left, right) in [
            (
                "Set `BACKEND=sqlite` for deployment.",
                "Set BACKEND=sqlite for deployment.",
            ),
            ("Use `LABEL='A  B'` here.", "Use LABEL='A  B' here."),
            ("Use MODE=release here.", "Select MODE=release here."),
        ] {
            assert!(compatible(left, right), "{left:?} / {right:?}");
            assert!(compatible(right, left));
        }
        for text in [
            "Set A='unfinished",
            "Set A=\"unfinished",
            "Set A=trailing\\",
            "Set A=$(query x)",
            "Set A=${NAME:-x}",
        ] {
            assert!(assignment_literals(text).is_none(), "{text:?}");
            assert!(compatible(text, text));
            assert!(!compatible(text, &format!("Please {text}")));
        }
    }

    #[test]
    fn evaluator_does_not_promote_different_backend_settings_as_corroboration() {
        let left = "Set BACKEND=sqlite for the application workspace deployment configuration.";
        let right = "Set BACKEND=postgres for the application workspace deployment configuration.";
        let candidates = [candidate("a", left), candidate("b", right)];
        let request = AskRequest {
            question: "application workspace deployment backend configuration".to_owned(),
            ..AskRequest::default()
        };
        let clusters = clustering::cluster_spans(&[span("a", left), span("b", right)]);
        assert_eq!(clusters.len(), 2);
        assert!(
            clusters
                .iter()
                .all(|item| item.score.to_bits() == 0.52_f32.to_bits())
        );
        for semantic_degraded in [false, true] {
            let report =
                evaluate_ask_scored(&request, &candidates, &|_, _, _, _| 0.52, semantic_degraded);
            assert!(report.abstained);
            assert!(report.answer_text.is_none() && report.citations.is_empty());
            assert!(!report.extractiveness_violated);
            assert_eq!(report.nearest_evidence.as_ref().unwrap().len(), 2);
        }
    }

    #[test]
    fn option_values_remain_literal_without_an_imperative_or_markdown() {
        for (left, right) in [
            (
                "The recommended tool uses --profile release for this workspace.",
                "The recommended tool uses --profile debug for this workspace.",
            ),
            ("Use tool -C Source here.", "Use tool -C source here."),
            (
                "Use tool --name 'A  B' here.",
                "Use tool --name 'A B' here.",
            ),
            (
                "Use tool --name='A  B' here.",
                "Use tool --name='A B' here.",
            ),
            (
                "Use tool --all --profile release.",
                "Use tool --all --profile debug.",
            ),
            ("Use tool -- --profile here.", "Use tool -- --other here."),
        ] {
            assert!(!compatible(left, right), "{left:?} / {right:?}");
            assert!(!compatible(right, left));
        }
        assert_eq!(
            option_literals("Use --name 'a --embedded b' --all --profile release"),
            Some(vec![
                ("--name", Some("'a --embedded b'")),
                ("--all", Some("--profile")),
                ("--profile", Some("release")),
            ])
        );
        assert!(option_literals("Use --name 'unfinished").is_none());
        assert!(compatible(
            "Use `tool --name 'A  B'` here.",
            "Use tool --name 'A  B' here.",
        ));
    }

    #[test]
    fn conditional_commands_keep_their_scope_operators_and_argument_order() {
        for (left, right) in [
            (
                "For the release, run copy source target.",
                "For the release, run copy target source.",
            ),
            (
                "When ready, execute test && deploy.",
                "When ready, execute test ; deploy.",
            ),
            (
                "In staging, invoke deploy App.",
                "In production, invoke deploy App.",
            ),
            (
                "The command: copy source target.",
                "The command: copy target source.",
            ),
        ] {
            assert!(!compatible(left, right), "{left:?} / {right:?}");
            assert!(!compatible(right, left));
        }
        for (left, right) in [
            (
                "When ready, run cargo test.",
                "When ready, execute cargo test.",
            ),
            ("Run: cargo test.", "Invoke: cargo test."),
            ("Command: cargo test.", "Invocation: cargo test."),
        ] {
            assert!(compatible(left, right), "{left:?} / {right:?}");
            assert!(compatible(right, left));
        }
        assert!(instruction_payload("The rerun completes.").is_none());
        assert!(instruction_payload("The test run completed.").is_none());
        assert!(compatible(
            "You should run cargo test.",
            "You should invoke cargo test.",
        ));
    }

    #[test]
    fn evaluator_preserves_option_alternatives_and_exact_citation_bytes() {
        let left = "The recommended tool uses --profile release for this workspace deployment.";
        let right = "The recommended tool uses --profile debug for this workspace deployment.";
        let mut candidates = [candidate("a", left), candidate("b", right)];
        let request = AskRequest {
            question: "recommended tool profile workspace deployment".to_owned(),
            ..AskRequest::default()
        };
        for semantic_degraded in [false, true] {
            let weak =
                evaluate_ask_scored(&request, &candidates, &|_, _, _, _| 0.52, semantic_degraded);
            assert!(weak.abstained && weak.citations.is_empty());
            assert_eq!(weak.nearest_evidence.as_ref().unwrap().len(), 2);
            let strong =
                evaluate_ask_scored(&request, &candidates, &|_, _, _, _| 0.8, semantic_degraded);
            assert!(!strong.abstained);
            assert_eq!(strong.citations.len(), 2);
            for citation in &strong.citations {
                let original = candidates
                    .iter()
                    .find(|row| row.memory_id == citation.memory_id)
                    .unwrap();
                assert_eq!(
                    original.content.get(citation.byte_start..citation.byte_end),
                    Some(citation.text.as_str()),
                );
            }
            let expected = ask_data_json(&strong);
            candidates.reverse();
            assert_eq!(
                expected,
                ask_data_json(&evaluate_ask_scored(
                    &request,
                    &candidates,
                    &|_, _, _, _| 0.8,
                    semantic_degraded,
                )),
            );
        }
    }

    #[test]
    fn code_fences_never_infer_program_equivalence() {
        for (left, right) in [
            ("```sh\nprepare\ndeploy\n```", "```sh\ndeploy\nprepare\n```"),
            ("~~~sh\ncopy a b\n~~~", "~~~sh\ncopy b a\n~~~"),
            (
                "````sh\n```\nprepare\ndeploy\n````",
                "````sh\n```\ndeploy\nprepare\n````",
            ),
        ] {
            assert!(!compatible(left, right));
            assert!(compatible(left, left));
        }
    }

    #[test]
    fn incomplete_and_escaped_markup_cannot_change_literal_boundaries() {
        assert!(!compatible("Use `cargo test", "Use `cargo build"));
        assert!(!compatible("Use `cargo test", "Use cargo test"));
        assert!(compatible("Use `cargo test", "Use `cargo test"));
        assert_eq!(
            literal_text("Use `A. B\\` safely.").unwrap().code,
            ["A. B\\"]
        );
        assert!(literal_text(r"Use \`literal.").unwrap().code.is_empty());
        assert!(literal_text(r"Use \``literal.").unwrap().code.is_empty());
        let text = (1..=128)
            .map(|width| format!("{}x ", "`".repeat(width)))
            .collect::<String>();
        assert!(literal_text(&text).is_none());
    }

    #[test]
    fn public_evaluator_withholds_false_bonus_in_both_scoring_regimes() {
        let request = AskRequest {
            question: "reset workspace release".to_owned(),
            ..AskRequest::default()
        };
        let candidates = [candidate("a", SOFT), candidate("b", HARD)];
        for semantic_degraded in [false, true] {
            // Exercise the real shared evaluator with a controlled score at
            // the existing floor boundary; this is not a model-quality test.
            let report =
                evaluate_ask_scored(&request, &candidates, &|_, _, _, _| 0.52, semantic_degraded);
            assert!(report.abstained);
            assert!(report.answer_text.is_none() && report.citations.is_empty());
            assert!(!report.extractiveness_violated && !report.conflict_detected);
            let nearest = report.nearest_evidence.as_ref().unwrap();
            assert_eq!(nearest.len(), 2);
            assert_eq!(nearest[0].memory_id, "a");
            assert_eq!(nearest[1].memory_id, "b");
        }
    }

    #[test]
    fn public_lexical_answer_retains_each_exact_command_and_input_order() {
        let request = AskRequest {
            question: SOFT.to_owned(),
            ..AskRequest::default()
        };
        let mut candidates = [candidate("a", SOFT), candidate("b", HARD)];
        for row in &candidates {
            assert!(
                score_span(
                    &tokenize_for_ask(&request.question),
                    &row.content,
                    row.confidence,
                    &row.trust_class,
                ) > request.min_confidence
            );
        }
        let report = evaluate_ask(&request, &candidates);
        assert!(!report.abstained && !report.conflict_detected);
        assert_eq!(report.citations.len(), 2);
        for citation in &report.citations {
            let original = candidates
                .iter()
                .find(|row| row.memory_id == citation.memory_id)
                .unwrap();
            assert_eq!(
                original.content.get(citation.byte_start..citation.byte_end),
                Some(citation.text.as_str())
            );
            assert_eq!(citation.provenance_uri, original.provenance_uri);
        }
        let expected = ask_data_json(&report);
        candidates.reverse();
        assert_eq!(
            expected,
            ask_data_json(&evaluate_ask(&request, &candidates))
        );
    }
}
