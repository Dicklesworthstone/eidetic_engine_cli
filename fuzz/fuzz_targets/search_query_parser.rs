#![no_main]

use ee::search::{ParsedSearchQuery, SearchQueryClause, parse_search_query};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 65_536;

fn assert_clauses_canonical(parsed: &ParsedSearchQuery) {
    for clause in parsed.clauses() {
        match clause {
            SearchQueryClause::Term(term) | SearchQueryClause::ExcludedTerm(term) => {
                assert!(
                    !term.is_empty(),
                    "bare term clauses must be non-empty after parsing"
                );
                assert!(
                    !term.chars().any(char::is_whitespace),
                    "bare term clauses must not contain whitespace: {term:?}"
                );
            }
            SearchQueryClause::Phrase(phrase) | SearchQueryClause::ExcludedPhrase(phrase) => {
                assert!(
                    !phrase.is_empty(),
                    "phrase clauses must be non-empty after parsing"
                );
            }
        }
    }
}

fn assert_display_canonical(displayed: &str, parsed: &ParsedSearchQuery) {
    assert!(
        !displayed.chars().any(char::is_control),
        "displayed query must not contain raw control characters: {displayed:?}"
    );

    if parsed.is_empty() {
        assert!(
            displayed.is_empty(),
            "empty parsed query must display empty"
        );
        return;
    }

    assert!(
        !displayed.starts_with(char::is_whitespace),
        "displayed query must not have leading whitespace: {displayed:?}"
    );
    assert!(
        !displayed.ends_with(char::is_whitespace),
        "displayed query must not have trailing whitespace: {displayed:?}"
    );

    let mut in_quote = false;
    let mut escaped = false;
    let mut previous_outside_was_space = false;
    for character in displayed.chars() {
        if escaped {
            escaped = false;
            previous_outside_was_space = false;
            continue;
        }
        if in_quote {
            match character {
                '\\' => escaped = true,
                '"' => in_quote = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => {
                in_quote = true;
                previous_outside_was_space = false;
            }
            value if value.is_whitespace() => {
                assert_eq!(
                    value, ' ',
                    "outside-phrase separators must be ASCII spaces: {displayed:?}"
                );
                assert!(
                    !previous_outside_was_space,
                    "outside-phrase separators must be single spaces: {displayed:?}"
                );
                previous_outside_was_space = true;
            }
            _ => previous_outside_was_space = false,
        }
    }
    assert!(
        !in_quote && !escaped,
        "displayed query must close quotes and escapes: {displayed:?}"
    );
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();

    let parsed = parse_search_query(input);
    assert_clauses_canonical(&parsed);

    let printed = parsed.to_string();
    assert_display_canonical(&printed, &parsed);

    let reparsed = parse_search_query(&printed);
    assert_eq!(
        parsed, reparsed,
        "parse(format!(parse(input))) must equal parse(input); input={input:?} printed={printed:?}"
    );
    assert_clauses_canonical(&reparsed);

    let reprinted = reparsed.to_string();
    assert_display_canonical(&reprinted, &reparsed);
    assert_eq!(
        printed, reprinted,
        "Display output must be a fixed point under reparse; printed={printed:?} reprinted={reprinted:?}"
    );
});
