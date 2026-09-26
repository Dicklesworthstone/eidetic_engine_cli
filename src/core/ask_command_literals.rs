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
        let Some(position) = text[offset..].match_indices(literal).find_map(|(start, _)| {
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
        }) else {
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

/// Only an explicit imperative introduces an opaque command payload. Retain
/// its conditions and arguments in full; we do not guess where a shell
/// command ends and prose resumes. The ordering parser handles its stronger
/// before/after equivalence before this fallback is called.
fn instruction_payload(text: &str) -> Option<&str> {
    let text = text.trim();
    let start = ask_list_content_start(text.as_bytes(), 0, text.len()).unwrap_or(0);
    let text = &text[start..];
    let (verb, rest) = text.split_once(char::is_whitespace)?;
    ["run", "execute", "invoke"]
        .iter()
        .any(|expected| verb.eq_ignore_ascii_case(expected))
        .then_some(rest.trim())
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
            ("The recommended invocation is `git reset --soft HEAD` for this task.",
             "The recommended invocation is `git reset --hard HEAD` for this task."),
            ("Copy `source` to `target` for the release.",
             "Copy `target` to `source` for the release."),
            ("Use `Build` for the release.", "Use `build` for the release."),
            ("Use `foo` for the release.", "Use foobar for the release."),
            ("Use `foo` for the release.", "Use foo-extra for the release."),
            ("Use ``echo `a`  b`` for the release.", "Use ``echo `a` b`` for the release."),
        ] {
            assert!(!compatible(left, right));
            assert!(!compatible(right, left));
        }
    }

    #[test]
    fn paths_namespaces_and_short_options_are_not_case_folded_or_reordered() {
        for (left, right) in [
            ("Use src/Main.rs for release.", "Use src/main.rs for release."),
            ("Copy src/first to src/second.", "Copy src/second to src/first."),
            ("Use tool -f for release.", "Use tool -F for release."),
            ("Use library::Build for release.", "Use library::build for release."),
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
            ("The command is `cargo test --all`.", "The command is cargo test --all."),
            ("Use `src/main.rs` for the release.", "Use src/main.rs for the release."),
            ("BACKEND: `sqlite`", "BACKEND: sqlite"),
            ("Use `café` for release.", "Use café for release."),
            ("Ordinary prose remains eligible.", "Ordinary prose is still eligible."),
        ] {
            assert!(compatible(left, right), "{left:?} / {right:?}");
            assert!(compatible(right, left));
        }
        let clusters = clustering::cluster_spans(&[span("a", SOFT), span("b", SOFT)]);
        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].score > ASK_MIN_CONFIDENCE_DEFAULT);
    }

    #[test]
    fn code_fences_never_infer_program_equivalence() {
        for (left, right) in [
            ("```sh\nprepare\ndeploy\n```", "```sh\ndeploy\nprepare\n```"),
            ("~~~sh\ncopy a b\n~~~", "~~~sh\ncopy b a\n~~~"),
            ("````sh\n```\nprepare\ndeploy\n````", "````sh\n```\ndeploy\nprepare\n````"),
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
        assert_eq!(literal_text("Use `A. B\\` safely.").unwrap().code, ["A. B\\"]);
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
            let report = evaluate_ask_scored(
                &request,
                &candidates,
                &|_, _, _, _| 0.52,
                semantic_degraded,
            );
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
            assert!(score_span(
                &tokenize_for_ask(&request.question),
                &row.content,
                row.confidence,
                &row.trust_class,
            ) > request.min_confidence);
        }
        let report = evaluate_ask(&request, &candidates);
        assert!(!report.abstained && !report.conflict_detected);
        assert_eq!(report.citations.len(), 2);
        for citation in &report.citations {
            let original = candidates.iter().find(|row| row.memory_id == citation.memory_id).unwrap();
            assert_eq!(original.content.get(citation.byte_start..citation.byte_end), Some(citation.text.as_str()));
            assert_eq!(citation.provenance_uri, original.provenance_uri);
        }
        let expected = ask_data_json(&report);
        candidates.reverse();
        assert_eq!(expected, ask_data_json(&evaluate_ask(&request, &candidates)));
    }
}
