//! Small, total parser for agent-facing search query strings.
//!
//! The parser intentionally accepts every UTF-8 string and emits a canonical
//! printable form. Retrieval engines may still interpret the resulting terms
//! differently, but this boundary gives property and fuzz tests a narrow target
//! for query normalization.

use std::fmt;
use std::iter::Peekable;
use std::str::Chars;

use super::scoring::AnchorMatchContext;
use crate::models::{MemoryAnchorSource, extract_precision_memory_anchors};

/// Parsed search query with deterministic clause ordering.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParsedSearchQuery {
    clauses: Vec<SearchQueryClause>,
}

impl ParsedSearchQuery {
    #[must_use]
    pub fn clauses(&self) -> &[SearchQueryClause] {
        &self.clauses
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }
}

impl fmt::Display for ParsedSearchQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, clause) in self.clauses.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            fmt::Display::fmt(clause, formatter)?;
        }
        Ok(())
    }
}

/// One canonical search query clause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchQueryClause {
    Term(String),
    Phrase(String),
    ExcludedTerm(String),
    ExcludedPhrase(String),
}

impl fmt::Display for SearchQueryClause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Term(term) => write_printable_unquoted(term, formatter),
            Self::Phrase(phrase) => write_quoted(phrase, formatter),
            Self::ExcludedTerm(term) => {
                formatter.write_str("-")?;
                write_printable_unquoted(term, formatter)
            }
            Self::ExcludedPhrase(phrase) => {
                formatter.write_str("-")?;
                write_quoted(phrase, formatter)
            }
        }
    }
}

/// Parse a search query string into a canonical, printable representation.
///
/// This function is total over UTF-8 input: malformed or incomplete syntax is
/// normalized into ordinary terms/phrases rather than returned as an error.
#[must_use]
pub fn parse_search_query(input: &str) -> ParsedSearchQuery {
    let mut chars = input.chars().peekable();
    let mut clauses = Vec::new();

    while chars.peek().is_some() {
        skip_separators(&mut chars);
        if chars.peek().is_none() {
            break;
        }
        if let Some(clause) = parse_clause(&mut chars) {
            clauses.push(clause);
        }
    }

    ParsedSearchQuery { clauses }
}

fn parse_clause(chars: &mut Peekable<Chars<'_>>) -> Option<SearchQueryClause> {
    let excluded = if matches!(chars.peek(), Some('-')) {
        chars.next();
        true
    } else {
        false
    };

    let quoted = matches!(chars.peek(), Some('"'));
    let value = if quoted {
        chars.next();
        if !remaining_contains_closing_quote(chars) {
            let value = parse_unclosed_quoted_bare(chars);
            if excluded && value.is_empty() {
                return Some(SearchQueryClause::Term("-".to_string()));
            }
            if value.is_empty() {
                return None;
            }
            return if excluded {
                Some(SearchQueryClause::ExcludedTerm(value))
            } else {
                Some(SearchQueryClause::Term(value))
            };
        }
        parse_quoted(chars)
    } else {
        parse_bare(chars)
    };

    if excluded && !quoted && value.is_empty() {
        return Some(SearchQueryClause::Term("-".to_string()));
    }

    if value.is_empty() {
        return None;
    }

    match (excluded, quoted) {
        (false, false) => Some(SearchQueryClause::Term(value)),
        (false, true) => Some(SearchQueryClause::Phrase(value)),
        (true, false) => Some(SearchQueryClause::ExcludedTerm(value)),
        (true, true) => Some(SearchQueryClause::ExcludedPhrase(value)),
    }
}

fn skip_separators(chars: &mut Peekable<Chars<'_>>) {
    while matches!(chars.peek(), Some(value) if is_query_separator(*value)) {
        chars.next();
    }
}

fn parse_bare(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut value = String::new();
    while let Some(next) = chars.peek().copied() {
        if is_query_separator(next) || next == '"' {
            break;
        }
        value.push(next);
        chars.next();
    }
    value
}

fn parse_unclosed_quoted_bare(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut value = String::new();
    while let Some(next) = chars.peek().copied() {
        if is_query_separator(next) {
            break;
        }
        if next == '"' {
            break;
        }
        if next == '\\' {
            chars.next();
            if matches!(chars.peek(), Some('"')) {
                chars.next();
                break;
            }
            value.push(next);
            continue;
        }
        value.push(next);
        chars.next();
    }
    value
}

fn remaining_contains_closing_quote(chars: &Peekable<Chars<'_>>) -> bool {
    let mut escaped = false;
    for next in chars.clone() {
        if escaped {
            escaped = false;
            continue;
        }
        if next == '\\' {
            escaped = true;
            continue;
        }
        if next == '"' {
            return true;
        }
    }
    false
}

fn parse_quoted(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut value = String::new();
    let mut last_was_normalized_space = false;
    while let Some(next) = chars.next() {
        match next {
            '"' => break,
            '\\' => match chars.next() {
                Some(escaped) => {
                    push_quoted_printable(&mut value, escaped, &mut last_was_normalized_space)
                }
                None => value.push('\\'),
            },
            other => push_quoted_printable(&mut value, other, &mut last_was_normalized_space),
        }
    }
    value
}

fn is_query_separator(character: char) -> bool {
    character.is_whitespace() || character.is_control()
}

fn push_quoted_printable(
    value: &mut String,
    character: char,
    last_was_normalized_space: &mut bool,
) {
    if character.is_control() {
        if !*last_was_normalized_space {
            value.push(' ');
            *last_was_normalized_space = true;
        }
    } else {
        value.push(character);
        *last_was_normalized_space = false;
    }
}

fn write_quoted(value: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str("\"")?;
    let mut last_was_normalized_space = false;
    for character in value.chars() {
        match character {
            '"' => formatter.write_str("\\\"")?,
            '\\' => formatter.write_str("\\\\")?,
            other if other.is_control() => {
                if !last_was_normalized_space {
                    formatter.write_str(" ")?;
                    last_was_normalized_space = true;
                }
            }
            other => write!(formatter, "{other}")?,
        }
        if !character.is_control() {
            last_was_normalized_space = false;
        }
    }
    formatter.write_str("\"")
}

fn write_printable_unquoted(value: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut last_was_normalized_space = false;
    for character in value.chars() {
        if character.is_control() {
            if !last_was_normalized_space {
                formatter.write_str(" ")?;
                last_was_normalized_space = true;
            }
        } else {
            write!(formatter, "{character}")?;
            last_was_normalized_space = false;
        }
    }
    Ok(())
}

/// Build the redaction-safe anchor-match context for a search query
/// (bd-1n0np.3.3).
///
/// Runs the same precision anchor extraction used at ingest over the query
/// string and projects each anchor to a `(kind, hash)` pair. The result feeds
/// [`anchor_match_score`](super::scoring::anchor_match_score): a query that
/// names exact code surfaces (`src/db/mod.rs`, schema `ee.response.v2`,
/// `EE_PACK_TRACE`, `cargo fmt --check`, …) can additively boost already-
/// retrieved candidates carrying the same hashed anchors, while Frankensearch
/// keeps ownership of candidate selection and base ranking. Only hashes cross
/// this boundary — raw query surfaces are never compared or stored. Prose with
/// no exact code surface yields a cold-start context (no boost).
#[must_use]
pub fn query_anchor_match_context(query: &str) -> AnchorMatchContext {
    let anchors = extract_precision_memory_anchors(
        "mem_searchquery00000000000000000",
        query,
        MemoryAnchorSource::Explicit,
        Some("search.query"),
    );
    AnchorMatchContext::new(anchors.into_iter().map(|anchor| {
        (
            anchor.anchor_kind.as_str().to_owned(),
            anchor.anchor_value_hash,
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::{SearchQueryClause, parse_search_query, query_anchor_match_context};

    #[test]
    fn search_query_parser_normalizes_terms_phrases_and_exclusions() {
        let query = parse_search_query(r#"  release  "cargo fmt" -"bad idea"  --flag "#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("release".to_string()),
                SearchQueryClause::Phrase("cargo fmt".to_string()),
                SearchQueryClause::ExcludedPhrase("bad idea".to_string()),
                SearchQueryClause::ExcludedTerm("-flag".to_string()),
            ]
        );
        assert_eq!(
            query.to_string(),
            r#"release "cargo fmt" -"bad idea" --flag"#
        );
    }

    #[test]
    fn search_query_parser_roundtrips_escaped_phrases() {
        let query = parse_search_query(r#""quoted \"value\" and \\ slash""#);
        let printed = query.to_string();

        assert_eq!(parse_search_query(&printed), query);
    }

    #[test]
    fn search_query_parser_preserves_dangling_exclusion_marker() {
        let query = parse_search_query("alpha - beta -");

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("alpha".to_string()),
                SearchQueryClause::Term("-".to_string()),
                SearchQueryClause::Term("beta".to_string()),
                SearchQueryClause::Term("-".to_string()),
            ]
        );
        assert_eq!(query.to_string(), "alpha - beta -");
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_still_drops_empty_quoted_clauses() {
        let query = parse_search_query(r#"alpha "" -"" beta"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("alpha".to_string()),
                SearchQueryClause::Term("beta".to_string()),
            ]
        );
        assert_eq!(query.to_string(), "alpha beta");
    }

    #[test]
    fn search_query_parser_splits_adjacent_bare_and_quoted_clauses() {
        let query = parse_search_query(r#"alpha"quoted value"beta"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("alpha".to_string()),
                SearchQueryClause::Phrase("quoted value".to_string()),
                SearchQueryClause::Term("beta".to_string()),
            ]
        );
        assert_eq!(query.to_string(), r#"alpha "quoted value" beta"#);
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_drops_dangling_quote_without_raw_quote_in_term() {
        let query = parse_search_query(r#"alpha" beta"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("alpha".to_string()),
                SearchQueryClause::Term("beta".to_string()),
            ]
        );
        assert_eq!(query.to_string(), "alpha beta");
    }

    #[test]
    fn search_query_parser_treats_unclosed_phrase_as_terms() {
        let query = parse_search_query(r#""alpha beta -gamma"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("alpha".to_string()),
                SearchQueryClause::Term("beta".to_string()),
                SearchQueryClause::ExcludedTerm("gamma".to_string()),
            ]
        );
        assert_eq!(query.to_string(), "alpha beta -gamma");
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_does_not_emit_escape_marker_for_unclosed_phrase() {
        let query = parse_search_query(r#"alpha "beta \" gamma"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("alpha".to_string()),
                SearchQueryClause::Term("beta".to_string()),
                SearchQueryClause::Term("gamma".to_string()),
            ]
        );
        assert_eq!(query.to_string(), "alpha beta gamma");
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_keeps_exclusion_for_dangling_quoted_term() {
        let query = parse_search_query(r#"-"alpha beta"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::ExcludedTerm("alpha".to_string()),
                SearchQueryClause::Term("beta".to_string()),
            ]
        );
        assert_eq!(query.to_string(), "-alpha beta");
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_treats_unbalanced_groups_and_brackets_as_literals() {
        let query = parse_search_query(r#"title:[release TO beta) (dangling [open] -scope:(bad]"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("title:[release".to_string()),
                SearchQueryClause::Term("TO".to_string()),
                SearchQueryClause::Term("beta)".to_string()),
                SearchQueryClause::Term("(dangling".to_string()),
                SearchQueryClause::Term("[open]".to_string()),
                SearchQueryClause::ExcludedTerm("scope:(bad]".to_string()),
            ]
        );
        assert_eq!(
            query.to_string(),
            r#"title:[release TO beta) (dangling [open] -scope:(bad]"#
        );
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_unescapes_bracket_like_characters_only_inside_phrases() {
        let query = parse_search_query(r#""escaped \[brackets\] and \(group\)" path:\[literal\]"#);

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Phrase("escaped [brackets] and (group)".to_string()),
                SearchQueryClause::Term(r#"path:\[literal\]"#.to_string()),
            ]
        );
        assert_eq!(
            query.to_string(),
            r#""escaped [brackets] and (group)" path:\[literal\]"#
        );
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_preserves_unicode_format_controls_and_replacement_chars() {
        let query =
            parse_search_query("field\u{200d}:value bad\u{fffd}field -\"zero\u{200d} width\"");

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("field\u{200d}:value".to_string()),
                SearchQueryClause::Term("bad\u{fffd}field".to_string()),
                SearchQueryClause::ExcludedPhrase("zero\u{200d} width".to_string()),
            ]
        );
        assert_eq!(
            query.to_string(),
            "field\u{200d}:value bad\u{fffd}field -\"zero\u{200d} width\""
        );
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_parser_normalizes_ascii_controls_to_printable_form() {
        let query =
            parse_search_query("alpha\u{0000}beta \"line\u{0007}\nfeed\" -gamma\u{001f}delta");

        assert_eq!(
            query.clauses(),
            &[
                SearchQueryClause::Term("alpha".to_string()),
                SearchQueryClause::Term("beta".to_string()),
                SearchQueryClause::Phrase("line feed".to_string()),
                SearchQueryClause::ExcludedTerm("gamma".to_string()),
                SearchQueryClause::Term("delta".to_string()),
            ]
        );
        assert_eq!(query.to_string(), r#"alpha beta "line feed" -gamma delta"#);
        assert!(
            !query.to_string().chars().any(char::is_control),
            "canonical query form must not emit raw control characters"
        );
        assert_eq!(parse_search_query(&query.to_string()), query);
    }

    #[test]
    fn search_query_clause_display_sanitizes_direct_control_char_values() {
        for clause in [
            SearchQueryClause::Term("alpha\u{0000}\nbeta".to_string()),
            SearchQueryClause::Phrase("line\u{0007}\nfeed".to_string()),
            SearchQueryClause::ExcludedTerm("gamma\u{001f}delta".to_string()),
            SearchQueryClause::ExcludedPhrase("tab\tfeed".to_string()),
        ] {
            assert!(
                !clause.to_string().chars().any(char::is_control),
                "direct clause Display must not emit raw controls: {clause:?}"
            );
        }

        assert_eq!(
            SearchQueryClause::Phrase("line\u{0007}\nfeed".to_string()).to_string(),
            r#""line feed""#
        );
        assert_eq!(
            SearchQueryClause::ExcludedTerm("gamma\u{001f}delta".to_string()).to_string(),
            "-gamma delta"
        );
    }

    #[test]
    fn query_anchor_match_context_extracts_hashed_surfaces() {
        let context = query_anchor_match_context("Edit `src/db/mod.rs` and honor `EE_PACK_TRACE`");
        assert!(!context.is_cold_start());
        // The query's path surface is carried as its canonical anchor hash,
        // matching what extraction stored for the same path.
        let path_hash = crate::models::memory_anchor_value_hash(
            crate::models::MemoryAnchorKind::Path,
            "src/db/mod.rs",
        );
        assert!(context.anchors.contains(&("path".to_owned(), path_hash)));
        // No raw surface string leaks into the context.
        assert!(
            context
                .anchors
                .iter()
                .all(|(_, hash)| hash.starts_with("blake3:") && !hash.contains("src/db/mod.rs"))
        );
        // Prose with no exact code surface yields a cold-start context (no boost).
        assert!(query_anchor_match_context("just ordinary prose words").is_cold_start());
    }
}
