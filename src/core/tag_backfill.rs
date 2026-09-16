//! Content-derived tag proposals for historical tag-less memories
//! (`bd-historical-tag-backfill-0pgro`).
//!
//! Historical memories were written before tagging was routine, so they carry
//! no tags at all and are unreachable by `--tag` recall even though `ee pack`
//! still finds them semantically. This module derives tag proposals from what
//! a memory already says, so a one-time backfill can make those rows visible
//! to `--tag` without inventing facts.
//!
//! # Why this is deliberately conservative
//!
//! A backfill that guesses is worse than no backfill: a wrong `ticker:` tag
//! silently poisons every future `--tag` query that trusts it, and unlike a
//! missing tag it gives no signal that anything is wrong. So every rule here
//! must be *evidence-bearing* — it fires only on an explicit marker that a
//! human put in the text on purpose, and it reports the exact span that
//! justified it. Rules that would fire on ordinary prose are excluded even
//! when they would raise coverage.
//!
//! In particular there is **no** bare-uppercase-token rule. Scanning prose for
//! capitalised words and calling them tickers would tag `JSON`, `HTTP`, `SEC`
//! and `CEO` as equities. Uppercase alone is not evidence.
//!
//! # Determinism
//!
//! [`derive_tags`] is a pure function of `(kind, content)`. Output is sorted
//! by tag and deduplicated, so the same memory always produces byte-identical
//! proposals and a dry-run report can be diffed against a later apply.

use std::collections::BTreeMap;

use crate::models::Tag;

/// Longest ticker symbol this module will accept.
///
/// Five covers every US listed symbol including the five-letter suffixed
/// classes (e.g. a trailing `W`/`R` for warrants and rights).
const MAX_TICKER_LEN: usize = 5;

/// Exchange prefixes recognised in a parenthetical quotation such as
/// `(NASDAQ: RDVT)`. Matching is case-insensitive on the prefix.
const EXCHANGE_PREFIXES: &[&str] = &[
    "NASDAQ",
    "NYSE",
    "NYSEAMERICAN",
    "NYSE AMERICAN",
    "AMEX",
    "OTC",
    "OTCQB",
    "OTCQX",
    "CBOE",
];

/// Uppercase tokens that look like symbols but are not, used to gate the
/// weaker adjacency rule. Filing and engineering prose is dense with these.
const TICKER_STOPLIST: &[&str] = &[
    "AI", "API", "CEO", "CFO", "CIK", "CLI", "COO", "CPU", "CSV", "CTO", "DB", "EBIT", "EPS",
    "ETF", "FAQ", "FY", "GAAP", "GDP", "HTML", "HTTP", "ID", "INC", "IPO", "IRS", "JSON", "LLC",
    "LLM", "LP", "LTD", "MD", "NDA", "OK", "PDF", "PLC", "Q1", "Q2", "Q3", "Q4", "R2", "REIT",
    "ROE", "ROI", "SEC", "SQL", "SSD", "TODO", "TOON", "TTM", "UI", "URL", "USA", "USD", "UTC",
    "XML", "YAML", "YOY",
];

/// Explicit attribution labels. The colon is required: a bare mention of the
/// word "analyst" in prose is not an attribution.
const ATTRIBUTION_LABELS: &[&str] = &["ANALYST", "ANALYSTS", "UNDERWRITER", "AUTHOR", "REVIEWER"];

/// Longest attribution slug retained, well under [`crate::models::MAX_TAG_BYTES`]
/// once the `analyst:` prefix is added.
const MAX_ATTRIBUTION_SLUG_LEN: usize = 48;

/// One proposed tag plus the reason it was proposed.
///
/// `evidence` is the literal span from the memory content that triggered the
/// rule (or the structured field value, for [`TagRule::GenreFromKind`]). It
/// exists so a dry-run report can show an operator *why* each tag is being
/// suggested rather than asking them to trust a bare list.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TagDerivation {
    /// Canonical tag, already normalized through [`Tag::parse`].
    pub tag: String,
    /// Which rule fired.
    pub rule: TagRule,
    /// The span of content (or field value) that justifies the tag.
    pub evidence: String,
}

/// The evidence-bearing rules this module implements.
///
/// Ordering is by descending confidence, which is also the order a report
/// should prefer when the same tag is derived more than once.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TagRule {
    /// The memory's structured `kind` column. No inference over prose at all,
    /// so this is the only rule that cannot be wrong about its own input.
    GenreFromKind,
    /// A cash-tag, e.g. `$RDVT`. The `$` sigil exists to mark a ticker.
    TickerCashtag,
    /// An exchange-qualified quotation, e.g. `(NASDAQ: RDVT)`.
    TickerExchangePrefix,
    /// A symbol-shaped token adjacent to a `CIK` reference, e.g.
    /// `RDVT (` in a filing summary that also cites a CIK.
    TickerCikAdjacent,
    /// An explicitly labelled attribution, e.g. `Analyst: Jane Roe`.
    AttributionLabeled,
}

impl TagRule {
    /// Stable lowercase wire form for reports and JSONL logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GenreFromKind => "genre_from_kind",
            Self::TickerCashtag => "ticker_cashtag",
            Self::TickerExchangePrefix => "ticker_exchange_prefix",
            Self::TickerCikAdjacent => "ticker_cik_adjacent",
            Self::AttributionLabeled => "attribution_labeled",
        }
    }
}

/// Derive tag proposals for one memory from its kind and content.
///
/// Returns a deterministic, deduplicated, tag-sorted list. When the same tag
/// is justified by more than one rule, the highest-confidence rule wins (see
/// [`TagRule`] ordering) so the report names the strongest evidence.
///
/// An empty result is a normal, honest outcome: it means nothing in this
/// memory justified a tag, and the row should be left alone.
#[must_use]
pub fn derive_tags(kind: &str, content: &str) -> Vec<TagDerivation> {
    let chars: Vec<char> = content.chars().collect();
    let mut best: BTreeMap<String, TagDerivation> = BTreeMap::new();

    // Keep the strongest evidence per tag. Written as a `get_mut` probe with
    // an early return rather than `entry().and_modify().or_insert()` so the
    // closure never borrows `derivation` in the same statement that moves it.
    let mut push = |derivation: TagDerivation| {
        if let Some(existing) = best.get_mut(&derivation.tag) {
            if derivation.rule < existing.rule {
                *existing = derivation;
            }
            return;
        }
        best.insert(derivation.tag.clone(), derivation);
    };

    if let Some(derivation) = genre_from_kind(kind) {
        push(derivation);
    }
    for derivation in ticker_derivations(&chars) {
        push(derivation);
    }
    for derivation in attribution_derivations(&chars) {
        push(derivation);
    }

    best.into_values().collect()
}

/// Map the structured `kind` column onto a coarse genre tag.
///
/// `Custom` kinds deliberately produce nothing: an unknown kind carries no
/// agreed meaning, and guessing one would be exactly the fabrication this
/// module exists to avoid.
fn genre_from_kind(kind: &str) -> Option<TagDerivation> {
    let genre = match kind.trim().to_ascii_lowercase().as_str() {
        "decision" => "decision",
        "rule" | "convention" | "playbook-step" | "command" => "process",
        "fact" => "fact",
        "failure" | "anti-pattern" => "failure",
        "risk" => "risk",
        _ => return None,
    };
    build_derivation(
        &format!("genre:{genre}"),
        TagRule::GenreFromKind,
        kind.trim(),
    )
}

/// Collect every ticker proposal in `chars`.
fn ticker_derivations(chars: &[char]) -> Vec<TagDerivation> {
    let mut out = Vec::new();
    let cik_present = contains_word(chars, &['C', 'I', 'K']);

    for (start, end) in uppercase_runs(chars) {
        let length = end - start;
        if length == 0 || length > MAX_TICKER_LEN {
            continue;
        }
        let symbol: String = chars[start..end].iter().collect();

        if preceded_by_cashtag(chars, start) {
            if let Some(derivation) = ticker_derivation(
                &symbol,
                TagRule::TickerCashtag,
                &span(chars, start.saturating_sub(1), end),
            ) {
                out.push(derivation);
                continue;
            }
        }

        if let Some(prefix_start) = exchange_prefix_before(chars, start) {
            if let Some(derivation) = ticker_derivation(
                &symbol,
                TagRule::TickerExchangePrefix,
                &span(chars, prefix_start, end),
            ) {
                out.push(derivation);
                continue;
            }
        }

        if cik_present
            && !TICKER_STOPLIST.contains(&symbol.as_str())
            && length >= 2
            && symbol_shaped_adjacency(chars, start, end)
        {
            if let Some(derivation) = ticker_derivation(
                &symbol,
                TagRule::TickerCikAdjacent,
                &span(chars, start, (end + 2).min(chars.len())),
            ) {
                out.push(derivation);
            }
        }
    }

    out
}

fn ticker_derivation(symbol: &str, rule: TagRule, evidence: &str) -> Option<TagDerivation> {
    build_derivation(
        &format!("ticker:{}", symbol.to_ascii_lowercase()),
        rule,
        evidence,
    )
}

/// Collect attribution proposals from explicit `Label: Name` markers.
fn attribution_derivations(chars: &[char]) -> Vec<TagDerivation> {
    let mut out = Vec::new();
    for label in ATTRIBUTION_LABELS {
        let label_chars: Vec<char> = label.chars().collect();
        let mut from = 0_usize;
        while let Some(at) = find_word_ci(chars, &label_chars, from) {
            from = at + label_chars.len();
            let mut cursor = from;
            while matches!(chars.get(cursor), Some(ch) if *ch == ' ' || *ch == '\t') {
                cursor += 1;
            }
            if chars.get(cursor) != Some(&':') {
                continue;
            }
            cursor += 1;
            let value_start = cursor;
            while matches!(chars.get(cursor), Some(ch) if !is_value_terminator(*ch)) {
                cursor += 1;
            }
            let raw: String = chars[value_start..cursor].iter().collect();
            let Some(slug) = slugify_attribution(&raw) else {
                continue;
            };
            if let Some(derivation) = build_derivation(
                &format!("analyst:{slug}"),
                TagRule::AttributionLabeled,
                &span(chars, at, cursor),
            ) {
                out.push(derivation);
            }
        }
    }
    out
}

/// Build a derivation, dropping the proposal when the tag is not a legal
/// [`Tag`]. Validation is never bypassed: an unrepresentable proposal is
/// simply not made.
fn build_derivation(tag: &str, rule: TagRule, evidence: &str) -> Option<TagDerivation> {
    Tag::parse(tag).ok().map(|parsed| TagDerivation {
        tag: parsed.to_string(),
        rule,
        evidence: evidence.trim().to_owned(),
    })
}

/// Maximal runs of ASCII uppercase letters, as `(start, end_exclusive)`.
fn uppercase_runs(chars: &[char]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut index = 0_usize;
    while index < chars.len() {
        if chars[index].is_ascii_uppercase() {
            let start = index;
            while index < chars.len() && chars[index].is_ascii_uppercase() {
                index += 1;
            }
            runs.push((start, index));
        } else {
            index += 1;
        }
    }
    runs
}

/// True when the run at `start` is immediately preceded by `$` at a word
/// boundary, i.e. a cash-tag.
fn preceded_by_cashtag(chars: &[char], start: usize) -> bool {
    if start == 0 || chars[start - 1] != '$' {
        return false;
    }
    start < 2 || !is_word_char(chars[start - 2])
}

/// When the run at `start` is preceded by `<EXCHANGE>:` (with optional
/// whitespace around the colon), return the index the exchange literal starts
/// at so the evidence span can include it.
fn exchange_prefix_before(chars: &[char], start: usize) -> Option<usize> {
    let mut cursor = start;
    while cursor > 0 && matches!(chars[cursor - 1], ' ' | '\t') {
        cursor -= 1;
    }
    if cursor == 0 || chars[cursor - 1] != ':' {
        return None;
    }
    cursor -= 1;
    while cursor > 0 && matches!(chars[cursor - 1], ' ' | '\t') {
        cursor -= 1;
    }
    for prefix in EXCHANGE_PREFIXES {
        let prefix_chars: Vec<char> = prefix.chars().collect();
        if cursor < prefix_chars.len() {
            continue;
        }
        let candidate_start = cursor - prefix_chars.len();
        let matches_prefix = chars[candidate_start..cursor]
            .iter()
            .zip(prefix_chars.iter())
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected));
        if matches_prefix && (candidate_start == 0 || !is_word_char(chars[candidate_start - 1])) {
            return Some(candidate_start);
        }
    }
    None
}

/// The bead's field shape: `SYM (` or `(SYM)`.
fn symbol_shaped_adjacency(chars: &[char], start: usize, end: usize) -> bool {
    let before_ok = start == 0 || !is_word_char(chars[start - 1]);
    if !before_ok {
        return false;
    }
    if start > 0 && chars[start - 1] == '(' && chars.get(end) == Some(&')') {
        return true;
    }
    chars.get(end) == Some(&' ') && chars.get(end + 1) == Some(&'(')
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn is_value_terminator(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | ';' | ',' | '.' | '(' | ')' | '|')
}

/// True when `word` appears in `chars` delimited by non-word characters.
fn contains_word(chars: &[char], word: &[char]) -> bool {
    find_word_ci(chars, word, 0).is_some()
}

/// Case-insensitive search for `word` at a word boundary, starting at `from`.
fn find_word_ci(chars: &[char], word: &[char], from: usize) -> Option<usize> {
    if word.is_empty() || chars.len() < word.len() {
        return None;
    }
    let last_start = chars.len() - word.len();
    for start in from..=last_start {
        let matched = chars[start..start + word.len()]
            .iter()
            .zip(word.iter())
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected));
        if !matched {
            continue;
        }
        let before_ok = start == 0 || !is_word_char(chars[start - 1]);
        let after_index = start + word.len();
        let after_ok = after_index >= chars.len() || !is_word_char(chars[after_index]);
        if before_ok && after_ok {
            return Some(start);
        }
    }
    None
}

fn span(chars: &[char], start: usize, end: usize) -> String {
    let end = end.min(chars.len());
    if start >= end {
        return String::new();
    }
    chars[start..end].iter().collect()
}

/// Reduce an attribution value to a tag-safe slug, or `None` when the value
/// is not a plausible name (empty, digits-only, or absurdly long).
fn slugify_attribution(raw: &str) -> Option<String> {
    let mut slug = String::new();
    let mut pending_separator = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            pending_separator = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            pending_separator = true;
        }
        if slug.len() >= MAX_ATTRIBUTION_SLUG_LEN {
            break;
        }
    }
    if slug.len() < 2 || !slug.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    Some(slug)
}

#[cfg(test)]
mod tests {
    use super::{TagRule, derive_tags, slugify_attribution};

    fn tags(kind: &str, content: &str) -> Vec<String> {
        derive_tags(kind, content)
            .into_iter()
            .map(|derivation| derivation.tag)
            .collect()
    }

    fn rule_for(kind: &str, content: &str, tag: &str) -> Option<TagRule> {
        derive_tags(kind, content)
            .into_iter()
            .find(|derivation| derivation.tag == tag)
            .map(|derivation| derivation.rule)
    }

    #[test]
    fn cashtag_is_an_unambiguous_ticker() {
        assert_eq!(
            rule_for(
                "fact",
                "Opened a position in $RDVT this week.",
                "ticker:rdvt"
            ),
            Some(TagRule::TickerCashtag)
        );
    }

    #[test]
    fn exchange_qualified_quotation_is_a_ticker() {
        for content in [
            "Red Violet (NASDAQ: RDVT) reported results.",
            "Red Violet (nasdaq:RDVT) reported results.",
            "Shares of ACME (NYSE: ACME) fell.",
        ] {
            assert!(
                derive_tags("fact", content)
                    .iter()
                    .any(|d| d.rule == TagRule::TickerExchangePrefix),
                "expected an exchange-prefixed ticker in: {content}"
            );
        }
    }

    #[test]
    fn cik_adjacent_symbol_matches_the_field_report_shape() {
        // The bead's own field example: a filing summary naming both the
        // symbol and a CIK.
        let content = "RDVT (Red Violet, Inc.) CIK 0001719489 underwrite verdict: pass.";
        assert_eq!(
            rule_for("decision", content, "ticker:rdvt"),
            Some(TagRule::TickerCikAdjacent)
        );
    }

    #[test]
    fn adjacency_rule_requires_a_cik_in_the_same_memory() {
        // Same shape, no CIK anywhere: the weakest rule must not fire.
        let content = "RDVT (Red Violet, Inc.) was discussed on the call.";
        assert!(
            !tags("fact", content).contains(&"ticker:rdvt".to_owned()),
            "adjacency alone must not mint a ticker without a CIK reference"
        );
    }

    #[test]
    fn common_uppercase_prose_is_never_tagged_as_a_ticker() {
        // The whole point of the conservative design. None of these may
        // produce a ticker tag, even though every one is an uppercase run.
        let content = "The JSON API returned HTTP 500; the CEO and CFO asked the SEC about GAAP \
                       and USD EPS. See the CLI and SQL notes. CIK 0001719489 is on file.";
        let derived = tags("fact", content);
        assert!(
            derived.iter().all(|tag| !tag.starts_with("ticker:")),
            "stoplist tokens must never become tickers, got: {derived:?}"
        );
    }

    #[test]
    fn bare_uppercase_token_without_any_marker_is_not_a_ticker() {
        assert!(
            tags("fact", "We migrated ACME to the new runtime.")
                .iter()
                .all(|tag| !tag.starts_with("ticker:")),
            "an unmarked uppercase word is not evidence of a ticker"
        );
    }

    #[test]
    fn symbols_longer_than_five_letters_are_rejected() {
        assert!(
            tags("fact", "$TOOLONGSYM moved today. CIK 1 is on file.")
                .iter()
                .all(|tag| !tag.starts_with("ticker:")),
            "over-long uppercase runs are not ticker-shaped"
        );
    }

    #[test]
    fn genre_comes_from_the_structured_kind_not_from_prose() {
        assert_eq!(tags("decision", "anything"), vec!["genre:decision"]);
        assert_eq!(tags("rule", "anything"), vec!["genre:process"]);
        assert_eq!(tags("convention", "anything"), vec!["genre:process"]);
        assert_eq!(tags("playbook-step", "anything"), vec!["genre:process"]);
        assert_eq!(tags("command", "anything"), vec!["genre:process"]);
        assert_eq!(tags("fact", "anything"), vec!["genre:fact"]);
        assert_eq!(tags("failure", "anything"), vec!["genre:failure"]);
        assert_eq!(tags("anti-pattern", "anything"), vec!["genre:failure"]);
        assert_eq!(tags("risk", "anything"), vec!["genre:risk"]);
    }

    #[test]
    fn unknown_kind_yields_no_genre() {
        assert!(
            tags("some-custom-kind", "anything").is_empty(),
            "an unrecognised kind carries no agreed meaning and must not be guessed"
        );
    }

    #[test]
    fn labeled_attribution_becomes_a_slug() {
        assert_eq!(
            rule_for(
                "fact",
                "Analyst: Jane Q. Roe\nVerdict: pass.",
                "analyst:jane-q"
            ),
            Some(TagRule::AttributionLabeled),
            "derived: {:?}",
            derive_tags("fact", "Analyst: Jane Q. Roe\nVerdict: pass.")
        );
    }

    #[test]
    fn unlabeled_mention_of_an_analyst_is_not_an_attribution() {
        assert!(
            tags("fact", "The analyst consensus moved higher this quarter.")
                .iter()
                .all(|tag| !tag.starts_with("analyst:")),
            "prose mentioning analysts is not an attribution"
        );
    }

    #[test]
    fn derivations_are_deterministic_sorted_and_deduplicated() {
        let content = "$RDVT and (NASDAQ: RDVT) and $RDVT again. CIK 1 on file.";
        let first = derive_tags("decision", content);
        let second = derive_tags("decision", content);
        assert_eq!(first, second, "derivation must be a pure function");

        let ticker_hits: Vec<_> = first.iter().filter(|d| d.tag == "ticker:rdvt").collect();
        assert_eq!(ticker_hits.len(), 1, "duplicate tags must collapse to one");

        let names: Vec<&str> = first.iter().map(|d| d.tag.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "output must be tag-sorted");
    }

    #[test]
    fn strongest_evidence_wins_when_rules_collide() {
        // Cashtag outranks the CIK adjacency rule for the same symbol.
        let content = "$RDVT today. RDVT (Red Violet) CIK 0001719489.";
        assert_eq!(
            rule_for("fact", content, "ticker:rdvt"),
            Some(TagRule::TickerCashtag),
            "the highest-confidence rule must be reported"
        );
    }

    #[test]
    fn every_derived_tag_is_a_legal_tag() {
        let content = "$RDVT (NASDAQ: ACME) CIK 1. Analyst: Jane Roe.";
        for derivation in derive_tags("decision", content) {
            assert!(
                crate::models::Tag::parse(&derivation.tag).is_ok(),
                "derived tag must round-trip through Tag::parse: {}",
                derivation.tag
            );
            assert!(
                !derivation.evidence.is_empty(),
                "every derivation must carry the span that justified it: {derivation:?}"
            );
        }
    }

    #[test]
    fn empty_content_and_empty_kind_derive_nothing() {
        assert!(tags("", "").is_empty());
        assert!(tags("", "   \n  ").is_empty());
    }

    #[test]
    fn attribution_slug_rejects_implausible_values() {
        assert_eq!(slugify_attribution("Jane Roe"), Some("jane-roe".to_owned()));
        assert_eq!(slugify_attribution("  "), None);
        assert_eq!(slugify_attribution("12345"), None, "digits are not a name");
        assert_eq!(slugify_attribution("x"), None, "too short to be a name");
    }

    #[test]
    fn rule_wire_names_are_stable() {
        assert_eq!(TagRule::GenreFromKind.as_str(), "genre_from_kind");
        assert_eq!(TagRule::TickerCashtag.as_str(), "ticker_cashtag");
        assert_eq!(
            TagRule::TickerExchangePrefix.as_str(),
            "ticker_exchange_prefix"
        );
        assert_eq!(TagRule::TickerCikAdjacent.as_str(), "ticker_cik_adjacent");
        assert_eq!(TagRule::AttributionLabeled.as_str(), "attribution_labeled");
    }
}
