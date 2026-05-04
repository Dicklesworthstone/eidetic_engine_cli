//! Small, total parser for agent-facing search query strings.
//!
//! The parser intentionally accepts every UTF-8 string and emits a canonical
//! printable form. Retrieval engines may still interpret the resulting terms
//! differently, but this boundary gives property and fuzz tests a narrow target
//! for query normalization.

use std::fmt;
use std::iter::Peekable;
use std::str::Chars;

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
            Self::Term(term) => formatter.write_str(term),
            Self::Phrase(phrase) => write_quoted(phrase, formatter),
            Self::ExcludedTerm(term) => {
                formatter.write_str("-")?;
                formatter.write_str(term)
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
        skip_whitespace(&mut chars);
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
        parse_quoted(chars)
    } else {
        parse_bare(chars)
    };

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

fn skip_whitespace(chars: &mut Peekable<Chars<'_>>) {
    while matches!(chars.peek(), Some(value) if value.is_whitespace()) {
        chars.next();
    }
}

fn parse_bare(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut value = String::new();
    while let Some(next) = chars.peek().copied() {
        if next.is_whitespace() {
            break;
        }
        value.push(next);
        chars.next();
    }
    value
}

fn parse_quoted(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut value = String::new();
    while let Some(next) = chars.next() {
        match next {
            '"' => break,
            '\\' => match chars.next() {
                Some(escaped) => value.push(escaped),
                None => value.push('\\'),
            },
            other => value.push(other),
        }
    }
    value
}

fn write_quoted(value: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str("\"")?;
    for character in value.chars() {
        match character {
            '"' => formatter.write_str("\\\"")?,
            '\\' => formatter.write_str("\\\\")?,
            other => write!(formatter, "{other}")?,
        }
    }
    formatter.write_str("\"")
}

#[cfg(test)]
mod tests {
    use super::{SearchQueryClause, parse_search_query};

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
}
