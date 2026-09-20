//! Conservative single-valued setting alternatives, not general language inference.
//!
//! Only an affirmative assignment with one scalar slot is eligible. The entire
//! ordered nonnumeric statement (including the unit and subject) must agree.
//! Different environments, identifiers, ranges, lists and negative restrictions
//! are not contradictory just because they contain different numbers. More
//! general or paraphrased disputes still need an explicitly stored relation.
//! Categorical settings reuse the clustering parser so an alternative that
//! cannot corroborate the anchor can also be disclosed as opposing evidence.

#[derive(Debug, Eq, PartialEq)]
enum Token {
    Text(String),
    Scalar { unit: String },
}

#[derive(Debug)]
struct Claim {
    template: Vec<Token>,
    value: String,
}

/// Detect a different numeric or categorical setting in the same affirmative claim.
/// Prose uses the caller's shared polarity detector, not another vocabulary
/// of negation words. Exact setting syntax instead binds identifiers as keys:
/// `NO_RETRY=1` is an assignment, not an English prohibition.
/// Decimal canonicalization uses strings, never floating point or integer
/// parsing, so sign, fractional precision and arbitrarily large values survive.
pub(crate) fn conflicts(left: &str, left_negated: bool, right: &str, right_negated: bool) -> bool {
    match (setting_claim(left), setting_claim(right)) {
        (Some(left), Some(right)) => {
            return left.template == right.template && left.value != right.value;
        }
        // Do not equate a code identifier with a similarly worded prose claim,
        // or infer how an expression relates to a literal assignment.
        (Some(_), None) | (None, Some(_)) => return false,
        (None, None) => {}
    }
    if left_negated || right_negated {
        return false;
    }
    // Admission and final composition both call this predicate. Reuse the
    // same categorical subject/value contract as clustering: keeping two
    // backends in separate clusters is not enough if one is then silently
    // chosen, or its opposing source is dropped by the candidate budget.
    if super::clustering::categorical_settings_conflict(
        super::clustering::categorical_setting(left).as_ref(),
        super::clustering::categorical_setting(right).as_ref(),
    ) {
        return true;
    }
    let (Some(left), Some(right)) = (claim(left), claim(right)) else {
        return false;
    };
    left.template == right.template && left.value != right.value
}

/// One complete, single-line `key=value` or `key: value` statement.
/// Whitespace around the operator is immaterial; key spelling, namespace,
/// operator and unit remain exact. A YAML-style colon must be followed by
/// horizontal whitespace so a URI, host:port or clock is not an assignment.
/// This deliberately does not parse expressions, shell programs or documents.
fn setting_claim(text: &str) -> Option<Claim> {
    let text = text.trim();
    let text = text
        .strip_suffix('.')
        .or_else(|| text.strip_suffix(';'))
        .unwrap_or(text)
        .trim_end();
    let ticks = text.bytes().take_while(|&byte| byte == b'`').count();
    let text = if ticks == 0 {
        text
    } else {
        let closing = text.bytes().rev().take_while(|&byte| byte == b'`').count();
        if closing != ticks || ticks >= text.len() - closing {
            return None;
        }
        &text[ticks..text.len() - closing]
    };
    if text.contains(['\r', '\n']) {
        return None;
    }
    let (key, operator, value) = if let Some((key, value)) = text.split_once('=') {
        (key, "=", value)
    } else {
        let (key, value) = text.split_once(':')?;
        if !value.starts_with([' ', '\t']) {
            return None;
        }
        (key, ":", value)
    };
    let key = key.trim();
    let identifier = key.strip_prefix("--").unwrap_or(key);
    if !identifier.split('.').all(|part| {
        let mut bytes = part.bytes();
        bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }) {
        return None;
    }
    let (value, unit) = scalar(value.trim())?;
    Some(Claim {
        template: vec![
            Token::Text(key.to_owned()),
            Token::Text(operator.to_owned()),
            Token::Scalar { unit },
        ],
        value,
    })
}

fn claim(text: &str) -> Option<Claim> {
    let mut template = Vec::new();
    let mut value = None;
    let mut has_subject = false;
    let mut has_assignment = false;
    for raw in text.split_whitespace() {
        let token = raw
            .trim_start_matches(['"', '\'', '`', '(', '[', '{'])
            .trim_end_matches(['"', '\'', '`', ')', ']', '}', ',', ';', '.', '!', '?'])
            .to_owned();
        let lower = token.to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        // Bounds, alternatives, approximations and conditionals can be
        // simultaneously true. Decline rather than guess their semantics.
        if matches!(
            lower.as_str(),
            "and"
                | "or"
                | "either"
                | "between"
                | "least"
                | "most"
                | "minimum"
                | "maximum"
                | "min"
                | "max"
                | "over"
                | "under"
                | "above"
                | "below"
                | "than"
                | "about"
                | "around"
                | "roughly"
                | "approximately"
                | "approx"
                | "may"
                | "might"
                | "can"
                | "could"
                | "if"
                | "unless"
                | "except"
                | "example"
                | "e.g"
                | "range"
        ) || token.contains(['<', '>', '≤', '≥', '±', '~', '/'])
            || token.contains("..")
        {
            return None;
        }
        if let Some((number, unit)) = scalar(&token) {
            // A numeric subject ("Port 5432 is open") does not assign a
            // property of the same entity as "Port 6432 is open".
            if !has_subject || !has_assignment || value.is_some() {
                return None;
            }
            value = Some(number);
            template.push(Token::Scalar { unit });
        } else {
            let assignment = matches!(
                lower.as_str(),
                "is" | "are" | "use" | "uses" | "equals" | "set" | "="
            ) || token.ends_with(':');
            has_assignment |= assignment;
            has_subject |= !assignment && !matches!(lower.as_str(), "a" | "an" | "the");
            // Numeric identifiers are exact context too, not value slots.
            template.push(Token::Text(if token.chars().any(char::is_numeric) {
                token
            } else {
                lower
            }));
        }
    }
    Some(Claim {
        template,
        value: value?,
    })
}

/// A signed integer or ordinary decimal, optionally followed by an ASCII unit
/// or percent sign. Scientific notation, versions, dates, locale separators
/// and arithmetic expressions are deliberately outside this narrow grammar.
fn scalar(token: &str) -> Option<(String, String)> {
    let unit_start = token
        .char_indices()
        .rev()
        .take_while(|(_, ch)| ch.is_ascii_alphabetic() || *ch == '%')
        .last()
        .map_or(token.len(), |(index, _)| index);
    let (number, unit) = token.split_at(unit_start);
    let (negative, unsigned) = if let Some(rest) = number.strip_prefix('-') {
        (true, rest)
    } else {
        (false, number.strip_prefix('+').unwrap_or(number))
    };
    let (whole, fractional) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty()
        || !whole.bytes().all(|ch| ch.is_ascii_digit())
        || !fractional.bytes().all(|ch| ch.is_ascii_digit())
        || (unsigned.contains('.') && fractional.is_empty())
    {
        return None;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fractional = fractional.trim_end_matches('0');
    let mut canonical = String::new();
    if negative && (whole != "0" || !fractional.is_empty()) {
        canonical.push('-');
    }
    canonical.push_str(whole);
    if !fractional.is_empty() {
        canonical.push('.');
        canonical.push_str(fractional);
    }
    Some((canonical, unit.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disagreement(left: &str, right: &str) -> bool {
        conflicts(left, false, right, false)
    }

    #[test]
    fn different_values_of_the_same_setting_are_alternatives() {
        for (left, right) in [
            ("5432", "6432"),
            ("-30", "30"),
            ("1.25", "1.75"),
            ("9007199254740992", "9007199254740993"),
            ("10ms", "20ms"),
            ("0.00000000000000000000001", "0.00000000000000000000002"),
            ("12.5%", "15%"),
        ] {
            let left = format!("The production database setting is {left}.");
            let right = format!("The production database setting is {right}.");
            assert!(disagreement(&left, &right), "{left} / {right}");
            assert!(disagreement(&right, &left), "symmetric alternatives");
        }
    }

    #[test]
    fn equivalent_decimal_spellings_are_not_a_dispute() {
        for (left, right) in [
            ("1", "1.000"),
            ("+001.25", "1.2500"),
            ("-0.0", "+0"),
            ("10ms", "010.0ms"),
        ] {
            assert!(!disagreement(
                &format!("The database timeout is {left}."),
                &format!("The database timeout is {right}."),
            ));
        }
    }

    #[test]
    fn contexts_units_and_numeric_identifiers_are_not_wildcards() {
        for (left, right) in [
            (
                "The production database port is 5432.",
                "The staging database port is 6432.",
            ),
            (
                "The database timeout is 30ms.",
                "The database timeout is 40s.",
            ),
            (
                "The node1 database port is 5432.",
                "The node2 database port is 6432.",
            ),
            ("The database port is 5432.", "The cache port is 6432."),
            ("Port 5432 is open.", "Port 6432 is open."),
        ] {
            assert!(!disagreement(left, right), "{left} / {right}");
        }
    }

    #[test]
    fn bounds_lists_approximations_and_conditions_are_not_disputes() {
        for template in [
            "The database timeout is at least VALUE seconds.",
            "The database timeout is approximately VALUE seconds.",
            "The database timeout is VALUE seconds if busy.",
            "The database timeout is < VALUE seconds.",
            "The database port is VALUE or 7432.",
            "The database port is VALUE and retry limit is 3.",
        ] {
            assert!(
                !disagreement(
                    &template.replace("VALUE", "30"),
                    &template.replace("VALUE", "40")
                ),
                "{template}"
            );
        }
    }

    #[test]
    fn negative_restrictions_are_not_mutually_exclusive_assignments() {
        let left = "Do not use port 5432 for the database.";
        let right = "Do not use port 6432 for the database.";
        assert!(!conflicts(left, true, right, true));
        assert!(!conflicts(
            left,
            true,
            "Use port 6432 for the database.",
            false
        ));
    }

    #[test]
    fn punctuation_and_case_do_not_rewrite_source_text() {
        assert!(disagreement(
            "The database port is `5432`.",
            "THE database PORT is (6432)!"
        ));
        assert!(!disagreement(
            "The database port is `5432`.",
            "THE database PORT is (5432)!"
        ));
        assert!(disagreement(
            "The café timeout is 30ms.",
            "The café timeout is 40ms."
        ));
    }

    #[test]
    fn unsupported_literals_and_missing_values_do_not_guess() {
        for (left, right) in [
            ("1e3", "2e3"),
            ("1.2.3", "1.2.4"),
            ("2026-09-19", "2026-09-20"),
            ("1,234", "1,235"),
            (".5", ".6"),
            ("３０", "４０"),
        ] {
            assert!(!disagreement(
                &format!("The database setting is {left}."),
                &format!("The database setting is {right}."),
            ));
        }
        assert!(!disagreement(
            "No numeric value here.",
            "Nothing here either."
        ));
        assert!(!disagreement(
            "The database port is 5432.",
            "The database port is unknown."
        ));
    }

    #[test]
    fn meaningful_unit_case_is_preserved() {
        assert!(!disagreement(
            "The storage limit is 10MB.",
            "The storage limit is 20mb."
        ));
        assert!(!disagreement(
            "The power setting is 10MW.",
            "The power setting is 20mW."
        ));
    }

    #[test]
    fn compact_and_spaced_settings_expose_the_same_scalar_slot() {
        for (left, right) in [
            ("PORT=5432", "PORT = 6432"),
            ("port: 5432", "port : 6432"),
            ("db.timeout = -30ms", "db.timeout=30ms"),
            ("--timeout=1.25s", "--timeout = 1.75s"),
            ("`PORT=5432`.", "``PORT = 6432``"),
            ("PORT=5432;", "PORT=6432."),
            ("limit=9007199254740992", "limit=9007199254740993"),
        ] {
            assert!(disagreement(left, right), "{left} / {right}");
            assert!(disagreement(right, left), "symmetric settings");
        }
    }

    #[test]
    fn equivalent_setting_literals_do_not_manufacture_conflicts() {
        for (left, right) in [
            ("PORT=+005432", "PORT = 5432.0"),
            ("timeout: -0ms", "timeout : +00.000ms"),
            ("ratio=1.2500", "ratio=+01.25"),
            ("`limit=10`", "limit = 10;"),
        ] {
            assert!(!disagreement(left, right), "{left} / {right}");
        }
    }

    #[test]
    fn setting_keys_keep_case_namespace_operator_and_unit_identity() {
        for (left, right) in [
            ("PORT=5432", "port=6432"),
            ("production.port=5432", "staging.port=6432"),
            ("node1.port=5432", "node2.port=6432"),
            ("port=5432", "--port=6432"),
            ("port=5432", "port: 6432"),
            ("timeout=30ms", "timeout=40s"),
            ("power=10MW", "power=20mW"),
            ("port=5432", "The port is 6432."),
        ] {
            assert!(!disagreement(left, right), "{left} / {right}");
        }
    }

    #[test]
    fn a_negation_word_inside_a_setting_key_is_not_a_prohibition() {
        for key in ["NO_RETRY", "cache.invalid_limit", "--no-retry"] {
            let left = format!("{key}=1");
            let right = format!("{key}=2");
            assert!(crate::core::ask::has_negation(&left));
            assert!(conflicts(&left, true, &right, true));
        }
        assert!(!conflicts(
            "Do not use PORT=5432.",
            true,
            "Do not use PORT=6432.",
            true,
        ));
    }

    #[test]
    fn endpoints_operators_expressions_and_multiple_assignments_are_not_guessed() {
        for (left, right) in [
            ("server:5432", "server:6432"),
            ("12:30", "12:40"),
            ("https://host:5432", "https://host:6432"),
            ("PORT==5432", "PORT==6432"),
            ("PORT!=5432", "PORT!=6432"),
            ("PORT+=1", "PORT+=2"),
            ("PORT=5432+1", "PORT=6432+1"),
            ("PORT=5432 or 6432", "PORT=7432 or 8432"),
            ("PORT=5432 RETRIES=1", "PORT=6432 RETRIES=2"),
            ("PORT=5432\nRETRIES=1", "PORT=6432\nRETRIES=2"),
            ("PORT=1e3", "PORT=2e3"),
            ("version=1.2.3", "version=1.2.4"),
            ("[0]=1", "[0]=2"),
            ("`PORT=5432", "`PORT=6432"),
        ] {
            assert!(setting_claim(left).is_none(), "unsupported setting: {left}");
            assert!(setting_claim(right).is_none(), "unsupported setting: {right}");
            assert!(!disagreement(left, right), "{left} / {right}");
        }
    }

    #[test]
    fn public_ask_exposes_setting_conflicts_with_exact_citations() {
        use crate::core::ask::{AskCandidate, AskRequest, ask_data_json, evaluate_ask};

        for (left, right, question) in [
            ("PORT=5432", "PORT = 6432", "PORT"),
            ("timeout: 30ms", "timeout: 40ms", "timeout"),
            ("NO_RETRY=1", "NO_RETRY=2", "NO_RETRY"),
            ("BACKEND=sqlite", "BACKEND=postgres", "BACKEND"),
            (
                "RUST_TOOLCHAIN=\"nightly\"",
                "RUST_TOOLCHAIN=\"stable\"",
                "RUST_TOOLCHAIN",
            ),
            (
                "The production storage backend is SQLite.",
                "The production storage backend is Postgres.",
                "production storage backend",
            ),
        ] {
            let mut candidates: Vec<_> = [left, right]
                .into_iter()
                .enumerate()
                .map(|(index, content)| AskCandidate {
                    memory_id: format!("setting-{index}"),
                    content: content.to_owned(),
                    confidence: 1.0,
                    trust_class: "human_explicit".to_owned(),
                    provenance_uri: Some(format!("manual://settings/{index}")),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    team_provenance: None,
                })
                .collect();
            let request = AskRequest {
                question: question.to_owned(),
                ..AskRequest::default()
            };
            let report = evaluate_ask(&request, &candidates);
            assert!(!report.abstained && !report.extractiveness_violated);
            assert!(report.conflict_detected && report.conflict_link.is_none());
            assert!(report.answer_text.is_none() && report.citations.is_empty());
            let sides = report.sides.as_ref().expect("both conflicting settings");
            assert_eq!(sides.len(), 2);
            for citation in sides.iter().flat_map(|side| &side.citations) {
                let original = candidates
                    .iter()
                    .find(|candidate| candidate.memory_id == citation.memory_id)
                    .expect("cited source");
                assert_eq!(
                    original.content.get(citation.byte_start..citation.byte_end),
                    Some(citation.text.as_str())
                );
            }
            let expected = ask_data_json(&report);
            candidates.reverse();
            assert_eq!(ask_data_json(&evaluate_ask(&request, &candidates)), expected);
        }
    }

    #[test]
    fn public_ask_retains_a_conflicting_setting_beyond_the_candidate_cap() {
        use crate::core::ask::{ASK_CANDIDATE_SCAN_CAP, AskCandidate, AskRequest, evaluate_ask};

        for (left, right, question) in [
            ("PORT=5432", "PORT = 6432", "PORT"),
            ("BACKEND=sqlite", "BACKEND=postgres", "BACKEND"),
        ] {
            let mut candidates: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 4)
                .map(|index| AskCandidate {
                    memory_id: format!("a-setting-{index:05}"),
                    content: left.to_owned(),
                    confidence: 1.0,
                    trust_class: "human_explicit".to_owned(),
                    provenance_uri: Some(format!("manual://settings/{index}")),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    team_provenance: None,
                })
                .collect();
            let mut opposing = candidates[0].clone();
            opposing.memory_id = "z-conflicting-setting".to_owned();
            opposing.content = right.to_owned();
            candidates.push(opposing);
            let report = evaluate_ask(
                &AskRequest {
                    question: question.to_owned(),
                    ..AskRequest::default()
                },
                &candidates,
            );
            assert!(!report.abstained && report.conflict_detected);
            assert!(report.answer_text.is_none());
            assert!(
                report
                    .sides
                    .as_ref()
                    .expect("both settings")
                    .iter()
                    .flat_map(|side| &side.citations)
                    .any(|citation| citation.memory_id == "z-conflicting-setting"
                        && citation.text == right)
            );
        }
    }

    #[test]
    fn categorical_disagreement_uses_the_same_subject_contract_as_clustering() {
        for (left, right) in [
            ("BACKEND=sqlite", "BACKEND=postgres"),
            ("BACKEND=\"SQLite\"", "BACKEND=\"sqlite\""),
            (
                "The production storage backend is SQLite.",
                "The production storage backend is Postgres.",
            ),
            ("The serialization format is JSON.", "The serialization format is YAML."),
        ] {
            assert!(disagreement(left, right), "{left} / {right}");
            assert!(
                disagreement(right, left),
                "categorical disagreement is symmetric"
            );
        }
    }

    #[test]
    fn categorical_conflicts_do_not_cross_subjects_or_invent_exclusivity() {
        for (left, right) in [
            ("BACKEND=sqlite", "backend=postgres"),
            ("production.backend=sqlite", "staging.backend=postgres"),
            (
                "The production storage backend is SQLite.",
                "The staging storage backend is Postgres.",
            ),
            ("The storage backend is SQLite.", "The storage backend is sqlite."),
            ("The storage backend is SQLite.", "The storage backend is unknown."),
            (
                "The storage backend is SQLite.",
                "The preferred storage backend is Postgres.",
            ),
            (
                "The storage backend is SQLite.",
                "The storage backend is Postgres if available.",
            ),
            (
                "The storage backend is SQLite.",
                "The supported storage backend is Postgres.",
            ),
            (
                "The supported backends are SQLite.",
                "The supported backends are Postgres.",
            ),
            ("BACKEND=sqlite or postgres", "BACKEND=mysql or redis"),
            ("Do not use SQLite.", "Do not use Postgres."),
        ] {
            assert!(!disagreement(left, right), "{left} / {right}");
            assert!(!disagreement(right, left), "{right} / {left}");
        }
    }

    #[test]
    fn public_ask_does_not_report_other_environments_as_categorical_opposition() {
        use crate::core::ask::{AskCandidate, AskRequest, evaluate_ask};

        let candidates: Vec<_> = [
            "The production storage backend is SQLite.",
            "The staging storage backend is Postgres.",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, content)| AskCandidate {
            memory_id: format!("backend-{index}"),
            content: content.to_owned(),
            confidence: 1.0,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some(format!("manual://backends/{index}")),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            team_provenance: None,
        })
        .collect();
        let report = evaluate_ask(
            &AskRequest {
                question: "production storage backend".to_owned(),
                ..AskRequest::default()
            },
            &candidates,
        );
        assert!(!report.abstained && !report.extractiveness_violated);
        assert!(!report.conflict_detected);
        assert!(report.sides.is_none());
        assert!(
            report
                .citations
                .iter()
                .any(|citation| citation.memory_id == "backend-0")
        );
    }
}
