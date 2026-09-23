//! Conservative single-valued setting alternatives, not general language inference.
//!
//! Only an affirmative assignment with one value slot is eligible. The entire
//! ordered statement outside that slot (including the unit and subject) must
//! agree. Different environments, identifiers, ranges, lists and negative
//! restrictions are not contradictory just because they contain different
//! numbers or dates. More general or paraphrased disputes still need an
//! explicitly stored relation. Categorical settings reuse the clustering
//! parser so alternatives can also be disclosed as opposing evidence.

#[derive(Debug, Eq, PartialEq)]
enum Token {
    Text(String),
    Scalar { unit: String },
    CalendarDate,
    LocalTime,
    Instant,
    Categorical,
}

#[derive(Debug)]
struct Claim {
    template: Vec<Token>,
    value: String,
}

/// Detect a different numeric, temporal or categorical value in an affirmative claim.
/// Prose uses the caller's shared polarity detector, not another vocabulary
/// of negation words. Exact setting syntax instead binds identifiers as keys:
/// `NO_RETRY=1` and `NO_RETRY=enabled` are assignments, not English prohibitions.
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

/// A lexical near-duplicate may corroborate a recognized assignment only when
/// both its subject and value agree. Absence of a conflict is insufficient:
/// different environments, unknown values and qualified prose are not support
/// for a known setting. Unparsed pairs retain the existing prose safeguards.
pub(super) fn settings_compatible(left: &str, right: &str) -> bool {
    // This guard is also used by span clustering after its Jaccard check.
    // Reversed commands contain the same words but are not corroboration.
    if !super::ordering::compatible(left, right) {
        return false;
    }
    match (setting_claim(left), setting_claim(right)) {
        (Some(left), Some(right)) => left.template == right.template && left.value == right.value,
        (None, None) => true,
        _ => false,
    }
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
    let (value, slot) = setting_value(value.trim())?;
    Some(Claim {
        template: vec![
            Token::Text(key.to_owned()),
            Token::Text(operator.to_owned()),
            slot,
        ],
        value,
    })
}

/// Configuration literals are not prose: a `false` value or a `NO_` key does
/// not negate the assignment. Keep symbolic values case-sensitive, including
/// quoted spellings, and retain distinct numeric, temporal and symbolic slots.
/// Unknown values, expressions, lists and malformed quotes grant no inference.
fn setting_value(raw: &str) -> Option<(String, Token)> {
    if let Some((value, unit)) = scalar(raw) {
        return Some((value, Token::Scalar { unit }));
    }
    let value = match raw.chars().next()? {
        quote @ ('`' | '\'' | '"') => raw.strip_prefix(quote)?.strip_suffix(quote)?,
        _ => raw,
    };
    if let Some(literal) = temporal_literal(value) {
        return Some(literal);
    }
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
    Some((value.to_owned(), Token::Categorical))
}

fn claim(text: &str) -> Option<Claim> {
    let mut template = Vec::new();
    let mut value = None;
    let mut has_subject = false;
    let mut has_assignment = false;
    let mut has_temporal_value = false;
    let mut has_temporal_bound = false;
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
        // "The launch is before DATE" expresses a bound, not a date
        // assignment. Two different bounds may both be true. Retain this
        // guard even when the qualifier follows the temporal value.
        has_temporal_bound |= matches!(
            lower.as_str(),
            "before"
                | "after"
                | "by"
                | "since"
                | "until"
                | "during"
                | "from"
                | "through"
                | "earlier"
                | "later"
                | "starting"
                | "ending"
        );
        if let Some((number, unit)) = scalar(&token) {
            // A numeric subject ("Port 5432 is open") does not assign a
            // property of the same entity as "Port 6432 is open".
            if !has_subject || !has_assignment || value.is_some() {
                return None;
            }
            value = Some(number);
            template.push(Token::Scalar { unit });
        } else if let Some((literal, slot)) = temporal_literal(&token) {
            if !has_subject || !has_assignment || value.is_some() {
                return None;
            }
            has_temporal_value = true;
            value = Some(literal);
            template.push(slot);
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
    if has_temporal_value && has_temporal_bound {
        return None;
    }
    Some(Claim {
        template,
        value: value?,
    })
}

/// An unambiguous Gregorian day, local clock, or qualified timestamp.
/// Do not guess locale-specific date order, relative dates, named zones,
/// durations, leap seconds or versions. Dates and local clocks do not identify
/// instants. Canonicalization is for comparison only, never source rewriting.
fn temporal_literal(token: &str) -> Option<(String, Token)> {
    fn digits(bytes: &[u8]) -> Option<u32> {
        bytes.iter().try_fold(0, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + u32::from(byte - b'0'))
        })
    }

    let bytes = token.as_bytes();
    if bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        let year = digits(&bytes[..4])?;
        let month = digits(&bytes[5..7])?;
        let day = digits(&bytes[8..])?;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let last_day = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        if year == 0 || day == 0 || day > last_day {
            return None;
        }
        return Some((token.to_owned(), Token::CalendarDate));
    }
    if matches!(bytes.len(), 5 | 8) && bytes[2] == b':' {
        let hour = digits(&bytes[..2])?;
        let minute = digits(&bytes[3..5])?;
        let second = if bytes.len() == 8 {
            if bytes[5] != b':' {
                return None;
            }
            digits(&bytes[6..])?
        } else {
            0
        };
        if hour >= 24 || minute >= 60 || second >= 60 {
            return None;
        }
        return Some((
            format!("{hour:02}:{minute:02}:{second:02}"),
            Token::LocalTime,
        ));
    }
    instant_literal(token)
}

/// A deliberately bounded RFC3339 subset: a valid civil date, seconds, and an
/// explicit UTC offset. Keep fractional seconds as decimal digits instead of
/// rounding through a float or imposing a nanosecond precision limit. The
/// resulting whole-second/fraction pair is internal; citations retain the
/// original offset, spelling and precision. `-00:00` specifies a known UTC
/// instant with an unknown local offset; comparison does not infer that zone.
fn instant_literal(token: &str) -> Option<(String, Token)> {
    let bytes = token.as_bytes();
    if bytes.len() < 20 || !matches!(bytes[10], b'T' | b't') {
        return None;
    }
    let date = token.get(..10)?;
    let clock = token.get(11..19)?;
    if temporal_literal(date)?.1 != Token::CalendarDate
        || temporal_literal(clock)?.1 != Token::LocalTime
    {
        return None;
    }

    let mut zone_start = 19;
    let mut fraction = "";
    if bytes.get(zone_start) == Some(&b'.') {
        zone_start += 1;
        let fraction_start = zone_start;
        while bytes.get(zone_start).is_some_and(u8::is_ascii_digit) {
            zone_start += 1;
        }
        if fraction_start == zone_start {
            return None;
        }
        fraction = token.get(fraction_start..zone_start)?.trim_end_matches('0');
    }
    let zone = token.get(zone_start..)?;
    let offset_seconds = if zone.eq_ignore_ascii_case("Z") {
        0
    } else {
        let zone = zone.as_bytes();
        if zone.len() != 6
            || !matches!(zone[0], b'+' | b'-')
            || zone[3] != b':'
            || !zone[1..3].iter().chain(&zone[4..6]).all(u8::is_ascii_digit)
        {
            return None;
        }
        let hours = i64::from((zone[1] - b'0') * 10 + zone[2] - b'0');
        let minutes = i64::from((zone[4] - b'0') * 10 + zone[5] - b'0');
        if hours >= 24 || minutes >= 60 {
            return None;
        }
        let seconds = hours * 3600 + minutes * 60;
        if zone[0] == b'-' { -seconds } else { seconds }
    };

    // Date/clock validation above guarantees ASCII fields and years 1..=9999.
    // Count Gregorian days from 0001-01-01, allowing offset subtraction across
    // midnight and even outside the local civil year without overflow.
    let year = date[..4].parse::<i64>().ok()?;
    let month = date[5..7].parse::<usize>().ok()?;
    let day = date[8..].parse::<i64>().ok()?;
    let prior_year = year - 1;
    let month_starts: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut days = 365 * prior_year + prior_year / 4 - prior_year / 100
        + prior_year / 400
        + month_starts[month - 1]
        + day
        - 1;
    if month > 2 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
        days += 1;
    }
    let hours = clock[..2].parse::<i64>().ok()?;
    let minutes = clock[3..5].parse::<i64>().ok()?;
    let seconds = clock[6..].parse::<i64>().ok()?;
    let utc_seconds = days * 86400 + hours * 3600 + minutes * 60 + seconds - offset_seconds;
    Some((format!("{utc_seconds}:{fraction}"), Token::Instant))
}

/// A signed integer or ordinary decimal, optionally followed by an ASCII unit
/// or percent sign. Scientific notation, versions, locale separators and
/// arithmetic expressions are deliberately outside this narrow grammar.
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
            ("2026-02-29", "2026-02-30"),
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
            ("backend: 'sqlite'", "backend : sqlite"),
            ("`--backend=sqlite`", "--backend = \"sqlite\""),
            ("NO_RETRY=enabled", "NO_RETRY = `enabled`"),
            ("launch: '2026-09-22'", "launch : 2026-09-22"),
            ("time=09:30", "time = 09:30:00"),
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
            ("backend: sqlite", "Backend: postgres"),
            ("production.backend: sqlite", "staging.backend: postgres"),
            ("backend=sqlite", "backend: postgres"),
            ("backend=sqlite", "--backend=postgres"),
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
            let left = format!("{key}=enabled");
            let right = format!("{key}=disabled");
            assert!(conflicts(&left, true, &right, true));
            assert!(conflicts(&right, true, &left, true));
        }
        assert!(!conflicts(
            "Do not use PORT=5432.",
            true,
            "Do not use PORT=6432.",
            true,
        ));
        assert!(!conflicts(
            "Do not use backend=sqlite.",
            true,
            "Do not use backend=postgres.",
            true,
        ));
    }

    #[test]
    fn categorical_setting_literals_bind_exact_values_not_prose_polarity() {
        for (left, right) in [
            ("backend: sqlite", "backend : postgres"),
            ("--backend=sqlite", "--backend = postgres"),
            ("`backend: sqlite`", "``backend: postgres``"),
            ("CACHE_ENABLED=true", "CACHE_ENABLED=false"),
            ("profile: Release", "profile: release"),
            ("backend:\t'sqlite'", "backend: \"postgres\""),
        ] {
            assert!(
                conflicts(
                    left,
                    crate::core::ask::has_negation(left),
                    right,
                    crate::core::ask::has_negation(right),
                ),
                "{left} / {right}"
            );
            assert!(disagreement(right, left), "symmetric settings");
        }
    }

    #[test]
    fn symbolic_literals_do_not_accept_expressions_or_unknowns() {
        for value in [
            "",
            "'sqlite",
            "sqlite'",
            "sqlite or postgres",
            "sqlite/mysql",
            "${BACKEND}",
            "[sqlite,postgres]",
            "{backend:sqlite}",
            "unknown",
            "unspecified",
            "sqlite\nMODE=async",
            "sqlite;MODE=async",
        ] {
            assert!(setting_value(value).is_none(), "{value:?}");
            assert!(setting_claim(&format!("backend: {value}")).is_none());
        }
    }

    #[test]
    fn recognized_settings_require_positive_agreement_for_corroboration() {
        for (left, right, expected) in [
            ("backend: sqlite", "backend : 'sqlite'", true),
            ("NO_RETRY=enabled", "`NO_RETRY = enabled`", true),
            ("timeout=+030.00ms", "timeout=30ms", true),
            ("launch: 2026-09-22", "launch : '2026-09-22'", true),
            ("time=09:30", "time=09:30:00", true),
            ("launch: 2026-09-22", "launch: 2026-09-23", false),
            ("launch: 2026-09-22", "launch: unknown", false),
            ("backend: sqlite", "backend: postgres", false),
            ("backend: sqlite", "Backend: sqlite", false),
            ("backend: sqlite", "backend=sqlite", false),
            (
                "production.backend: sqlite",
                "staging.backend: sqlite",
                false,
            ),
            ("backend: sqlite", "backend: unknown", false),
            ("backend: sqlite", "backend: sqlite if available", false),
            ("backend: sqlite", "The backend is sqlite.", false),
            ("profile=Release", "profile=release", false),
        ] {
            assert_eq!(
                settings_compatible(left, right),
                expected,
                "{left} / {right}"
            );
            assert_eq!(settings_compatible(right, left), expected, "symmetry");
        }
        assert!(settings_compatible(
            "ordinary prose",
            "other ordinary prose"
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
            assert!(
                setting_claim(right).is_none(),
                "unsupported setting: {right}"
            );
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
            ("backend: sqlite", "backend: postgres", "backend"),
            ("--backend=sqlite", "--backend=postgres", "backend"),
            ("NO_RETRY=enabled", "NO_RETRY=disabled", "NO_RETRY"),
            ("CACHE_ENABLED=true", "CACHE_ENABLED=false", "CACHE_ENABLED"),
            ("LAUNCH=2026-09-22", "LAUNCH=2026-09-23", "LAUNCH"),
            ("time: 09:30", "time: 10:30", "time"),
            (
                "START=2026-09-22T09:30:00-04:00",
                "START=2026-09-22T14:30:00Z",
                "START",
            ),
            (
                "START=2026-09-22T13:30:00.001Z",
                "START=2026-09-22T13:30:00.002Z",
                "START",
            ),
            (
                "The deployment timestamp is 2026-09-22T09:30:00-04:00.",
                "The deployment timestamp is 2026-09-22T14:30:00Z.",
                "deployment timestamp",
            ),
            (
                "The production launch date is 2026-09-22.",
                "The production launch date is 2026-09-23.",
                "production launch date",
            ),
            (
                "The deployment time is 09:30.",
                "The deployment time is 10:30.",
                "deployment time",
            ),
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
            assert_eq!(
                ask_data_json(&evaluate_ask(&request, &candidates)),
                expected
            );
        }
    }

    #[test]
    fn public_ask_retains_a_conflicting_setting_beyond_the_candidate_cap() {
        use crate::core::ask::{ASK_CANDIDATE_SCAN_CAP, AskCandidate, AskRequest, evaluate_ask};

        for (left, right, question) in [
            ("PORT=5432", "PORT = 6432", "PORT"),
            ("BACKEND=sqlite", "BACKEND=postgres", "BACKEND"),
            ("backend: sqlite", "backend: postgres", "backend"),
            ("NO_RETRY=enabled", "NO_RETRY=disabled", "NO_RETRY"),
            ("LAUNCH=2026-09-22", "LAUNCH=2026-09-23", "LAUNCH"),
            ("time: 09:30", "time: 10:30", "time"),
            (
                "START=2026-09-22T09:30:00-04:00",
                "START=2026-09-22T14:30:00Z",
                "START",
            ),
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
            (
                "The serialization format is JSON.",
                "The serialization format is YAML.",
            ),
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
            (
                "The storage backend is SQLite.",
                "The storage backend is sqlite.",
            ),
            (
                "The storage backend is SQLite.",
                "The storage backend is unknown.",
            ),
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

    #[test]
    fn calendar_and_clock_assignments_compare_only_the_same_temporal_slot() {
        for (left, right) in [
            ("launch: 2026-09-22", "launch: '2026-09-23'"),
            ("time=09:30", "time=10:30:00"),
            (
                "The production launch date is 2026-09-22.",
                "The production launch date is 2026-09-23.",
            ),
            (
                "The café opening time is `09:30`.",
                "The café opening time is (10:30)!",
            ),
        ] {
            assert!(disagreement(left, right), "{left} / {right}");
            assert!(disagreement(right, left), "symmetric temporal alternatives");
        }
        for (left, right) in [
            ("launch=2026-09-22", "launch=09:30"),
            ("production.date=2026-09-22", "staging.date=2026-09-23"),
            ("launch=2026-09-22", "Launch=2026-09-23"),
            ("The time is 09:30 UTC.", "The time is 10:30 EDT."),
            (
                "The launch date is 2026-09-22.",
                "The launch date is unknown.",
            ),
            (
                "Release 2026-09-22 is stable.",
                "Release 2026-09-23 is stable.",
            ),
            (
                "The launch dates are 2026-09-22 and 2026-09-23.",
                "The launch dates are 2026-09-24 and 2026-09-25.",
            ),
        ] {
            assert!(!disagreement(left, right), "{left} / {right}");
        }
    }

    #[test]
    fn calendar_and_clock_literals_validate_boundaries_without_locale_guessing() {
        for valid in [
            "0001-01-01",
            "9999-12-31",
            "2000-02-29",
            "2024-02-29",
            "2026-04-30",
            "00:00",
            "23:59",
            "23:59:59",
            "2026-09-22T09:30:00Z",
        ] {
            assert!(temporal_literal(valid).is_some(), "{valid}");
        }
        for invalid in [
            "0000-01-01",
            "1900-02-29",
            "2100-02-29",
            "2026-02-29",
            "2026-04-31",
            "2026-00-01",
            "2026-13-01",
            "2026-01-00",
            "2026-9-22",
            "09/22/2026",
            "22-09-2026",
            "tomorrow",
            "1.2.3",
            "24:00",
            "23:60",
            "23:59:60",
            "9:30",
            "09:30Z",
            "09:30:xx",
            "２０２６-09-22",
        ] {
            assert!(temporal_literal(invalid).is_none(), "{invalid}");
        }
        assert_eq!(temporal_literal("09:30"), temporal_literal("09:30:00"));
    }

    #[test]
    fn temporal_bounds_ranges_and_restrictions_do_not_invent_conflicts() {
        for template in [
            "The launch is before VALUE.",
            "The launch is after VALUE.",
            "The launch is by VALUE.",
            "The launch is from VALUE.",
            "The launch is VALUE or later.",
            "The launch is VALUE if approved.",
            "The launch is approximately VALUE.",
        ] {
            for (left, right) in [
                ("2026-09-22", "2026-09-23"),
                ("2026-09-22T09:30:00Z", "2026-09-23T09:30:00Z"),
            ] {
                assert!(
                    !disagreement(
                        &template.replace("VALUE", left),
                        &template.replace("VALUE", right),
                    ),
                    "{template}"
                );
            }
        }
        assert!(!conflicts(
            "The launch date is 2026-09-22.",
            true,
            "The launch date is 2026-09-23.",
            true,
        ));
    }

    #[test]
    fn timestamp_offsets_and_fractional_spellings_compare_as_exact_instants() {
        for (left, right) in [
            ("2026-09-22T09:30:00-04:00", "2026-09-22T13:30:00Z"),
            ("2026-01-01T00:15:00+01:00", "2025-12-31T23:15:00Z"),
            ("2000-03-01T00:00:00+00:01", "2000-02-29T23:59:00Z"),
            ("1900-03-01T00:00:00+00:01", "1900-02-28T23:59:00Z"),
            ("2026-09-22T19:15:00+05:45", "2026-09-22T13:30:00Z"),
            (
                "2026-09-22t13:30:00.00100z",
                "2026-09-22T13:30:00.001+00:00",
            ),
            ("2026-09-22T13:30:00.000-00:00", "2026-09-22T13:30:00Z"),
            ("0001-01-01T00:01:00+00:01", "0001-01-01T00:00:00Z"),
            ("9999-12-31T23:59:59-00:00", "9999-12-31T23:59:59Z"),
        ] {
            let left_literal = temporal_literal(left).expect(left);
            let right_literal = temporal_literal(right).expect(right);
            assert_eq!(left_literal.1, Token::Instant);
            assert_eq!(left_literal, right_literal, "{left} / {right}");
            let left = format!("START={left}");
            let right = format!("START={right}");
            assert!(!disagreement(&left, &right));
            assert!(!disagreement(&right, &left));
            assert!(settings_compatible(&left, &right));
        }
    }

    #[test]
    fn timestamp_differences_do_not_round_away_or_cross_temporal_kinds() {
        for (left, right) in [
            ("2026-09-22T09:30:00+01:00", "2026-09-22T09:30:00+02:00"),
            (
                "2026-09-22T09:30:00.000000000000000000001Z",
                "2026-09-22T09:30:00.000000000000000000002Z",
            ),
        ] {
            let left = format!("START={left}");
            let right = format!("START={right}");
            assert!(disagreement(&left, &right));
            assert!(disagreement(&right, &left));
            assert!(!settings_compatible(&left, &right));
        }
        for other in ["2026-09-22", "09:30", "unknown"] {
            let left = "START=2026-09-22T09:30:00Z";
            let right = format!("START={other}");
            assert!(!disagreement(left, &right));
            assert!(!settings_compatible(left, &right));
        }
    }

    #[test]
    fn malformed_unqualified_and_leap_second_timestamps_are_not_guessed() {
        for invalid in [
            "0000-01-01T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-09-22T24:00:00Z",
            "2026-09-22T23:59:60Z",
            "2026-09-22T09:30Z",
            "2026-09-22 09:30:00Z",
            "2026-09-22T09:30:00",
            "2026-09-22T09:30:00+24:00",
            "2026-09-22T09:30:00+00:60",
            "2026-09-22T09:30:00+0100",
            "2026-09-22T09:30:00++1:00",
            "2026-09-22T09:30:00.Z",
            "2026-09-22T09:30:00.+01:00",
            "2026-09-22T09:30:00.123",
            "2026-09-22T09:30:00Zjunk",
            "2026-09-22T09:30:00UTC",
            "2026-09-22T09:30:00Z[America/New_York]",
            "２０２６-09-22T09:30:00Z",
            "2026-09-22T09:30:00.１２３Z",
        ] {
            assert!(temporal_literal(invalid).is_none(), "{invalid}");
            assert!(setting_claim(&format!("START={invalid}")).is_none());
        }
    }

    #[test]
    fn public_ask_does_not_invent_conflicts_for_equivalent_timestamp_offsets() {
        use crate::core::ask::{AskCandidate, AskRequest, ask_data_json, evaluate_ask};

        for (left, right) in [
            (
                "START=2026-09-22T09:30:00-04:00",
                "START=2026-09-22T13:30:00Z",
            ),
            (
                "START=2026-09-22T13:30:00.00100Z",
                "START=2026-09-22T14:30:00.001+01:00",
            ),
        ] {
            let mut candidates: Vec<_> = [left, right]
                .into_iter()
                .enumerate()
                .map(|(index, content)| AskCandidate {
                    memory_id: format!("timestamp-{index}"),
                    content: content.to_owned(),
                    confidence: 1.0,
                    trust_class: "human_explicit".to_owned(),
                    provenance_uri: Some(format!("manual://timestamps/{index}")),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    team_provenance: None,
                })
                .collect();
            let request = AskRequest {
                question: "START".to_owned(),
                ..AskRequest::default()
            };
            let report = evaluate_ask(&request, &candidates);
            assert!(!report.abstained && !report.extractiveness_violated);
            assert!(!report.conflict_detected && report.sides.is_none());
            assert!(!report.citations.is_empty());
            for citation in &report.citations {
                let original = candidates
                    .iter()
                    .find(|candidate| candidate.memory_id == citation.memory_id)
                    .expect("original timestamp source");
                assert_eq!(
                    original.content.get(citation.byte_start..citation.byte_end),
                    Some(citation.text.as_str())
                );
                assert_eq!(citation.text, original.content);
            }
            let expected = ask_data_json(&report);
            candidates.reverse();
            assert_eq!(
                ask_data_json(&evaluate_ask(&request, &candidates)),
                expected
            );
        }
    }
}
