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
            SearchQueryClause::Phrase(_) | SearchQueryClause::ExcludedPhrase(_) => {}
        }
    }
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
    let reparsed = parse_search_query(&printed);
    assert_eq!(
        parsed, reparsed,
        "parse(format!(parse(input))) must equal parse(input); input={input:?} printed={printed:?}"
    );
    assert_clauses_canonical(&reparsed);

    let reprinted = reparsed.to_string();
    assert_eq!(
        printed, reprinted,
        "Display output must be a fixed point under reparse; printed={printed:?} reprinted={reprinted:?}"
    );
});
