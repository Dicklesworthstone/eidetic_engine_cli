//! Conservative numeric-setting alternatives, not general natural-language inference.
//!
//! Only an affirmative assignment with one scalar slot is eligible. The entire
//! ordered nonnumeric statement (including the unit and subject) must agree.
//! Different environments, identifiers, ranges, lists and negative restrictions
//! are not contradictory just because they contain different numbers. More
//! general or paraphrased disputes still need an explicitly stored relation.

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

/// Detect a different scalar setting in the same affirmative claim.
/// The caller supplies the shared ask polarity detector's result; this helper
/// never invents a second, subtly different vocabulary of negation words.
/// Decimal canonicalization uses strings, never floating point or integer
/// parsing, so sign, fractional precision and arbitrarily large values survive.
pub(crate) fn conflicts(left: &str, left_negated: bool, right: &str, right_negated: bool) -> bool {
    if left_negated || right_negated {
        return false;
    }
    let (Some(left), Some(right)) = (claim(left), claim(right)) else {
        return false;
    };
    left.template == right.template && left.value != right.value
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
}
