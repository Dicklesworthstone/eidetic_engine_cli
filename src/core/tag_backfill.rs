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

use std::path::Path;

use crate::core::memory::{MemoryTagsMode, MemoryTagsOptions};
use crate::db::DbConnection;
use crate::models::{DomainError, TAG_BACKFILL_LOG_SCHEMA_V1, TAG_BACKFILL_SCHEMA_V1, Tag};

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

/// One memory considered by the backfill, as read from the store.
///
/// `content` MUST be the untruncated body. List views may truncate (see
/// `MemorySummary::content_truncated`), and deriving from a truncated body
/// would silently miss evidence near the end — producing a *different*,
/// quietly worse answer rather than an obviously broken one. Callers that
/// only hold a truncated body must set `content_truncated` so the planner
/// refuses the row instead of guessing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackfillCandidate {
    /// Memory id.
    pub memory_id: String,
    /// Structured memory kind.
    pub kind: String,
    /// Full memory body.
    pub content: String,
    /// True when `content` was truncated by the read path.
    pub content_truncated: bool,
    /// Tags the memory already carries.
    pub existing_tags: Vec<String>,
}

/// Why a candidate was not given tags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    /// The memory already carries tags; this backfill only targets rows that
    /// are completely dark to `--tag`.
    AlreadyTagged,
    /// Nothing in the memory justified a tag. A normal, honest outcome.
    NoEvidence,
    /// The body was truncated, so absence of evidence is not evidence of
    /// absence. Refused rather than guessed.
    ContentTruncated,
}

impl SkipReason {
    /// Stable lowercase wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyTagged => "already_tagged",
            Self::NoEvidence => "no_evidence",
            Self::ContentTruncated => "content_truncated",
        }
    }
}

/// A single planned mutation: the tags to add to one memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackfillProposal {
    /// Memory that would be patched.
    pub memory_id: String,
    /// Tags to add, sorted and free of tags the memory already has.
    pub add_tags: Vec<String>,
    /// The derivations backing `add_tags`, in the same order.
    pub derivations: Vec<TagDerivation>,
}

/// A candidate the plan intentionally left alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackfillSkip {
    /// Memory that was skipped.
    pub memory_id: String,
    /// Why.
    pub reason: SkipReason,
}

/// The full dry-run plan: what would change, and what deliberately would not.
///
/// Skips are first-class rather than silently dropped, because "we looked at
/// 900 memories and could justify tags for 240" is the honest report an
/// operator needs in order to trust the 240.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackfillPlan {
    /// Memories that would be patched, ordered by memory id.
    pub proposals: Vec<BackfillProposal>,
    /// Memories deliberately untouched, ordered by memory id.
    pub skipped: Vec<BackfillSkip>,
}

impl BackfillPlan {
    /// Number of memories that would be mutated.
    #[must_use]
    pub fn proposed_memory_count(&self) -> usize {
        self.proposals.len()
    }

    /// Total number of tags that would be added across all memories.
    #[must_use]
    pub fn proposed_tag_count(&self) -> usize {
        self.proposals
            .iter()
            .map(|proposal| proposal.add_tags.len())
            .sum()
    }

    /// True when applying this plan would write nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }
}

/// Build a deterministic backfill plan over `candidates`.
///
/// Idempotency comes from the plan, not from the writer: this backfill
/// targets only rows that are completely tag-less, so once a row has been
/// given tags it is skipped as [`SkipReason::AlreadyTagged`] on every later
/// run. A second pass over an already-applied workspace therefore produces
/// an empty plan and the apply path performs no write at all, without
/// relying on the store to deduplicate.
#[must_use]
pub fn plan_backfill(candidates: &[BackfillCandidate]) -> BackfillPlan {
    let mut proposals = Vec::new();
    let mut skipped = Vec::new();

    for candidate in candidates {
        if !candidate.existing_tags.is_empty() {
            skipped.push(BackfillSkip {
                memory_id: candidate.memory_id.clone(),
                reason: SkipReason::AlreadyTagged,
            });
            continue;
        }
        if candidate.content_truncated {
            skipped.push(BackfillSkip {
                memory_id: candidate.memory_id.clone(),
                reason: SkipReason::ContentTruncated,
            });
            continue;
        }

        let derivations = derive_tags(&candidate.kind, &candidate.content);
        if derivations.is_empty() {
            skipped.push(BackfillSkip {
                memory_id: candidate.memory_id.clone(),
                reason: SkipReason::NoEvidence,
            });
            continue;
        }

        let add_tags = derivations
            .iter()
            .map(|derivation| derivation.tag.clone())
            .collect();
        proposals.push(BackfillProposal {
            memory_id: candidate.memory_id.clone(),
            add_tags,
            derivations,
        });
    }

    proposals.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));
    skipped.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));
    BackfillPlan { proposals, skipped }
}

/// Render one proposal as a single JSONL audit line.
///
/// Emitted for every mutation so the operator has a durable, greppable record
/// of exactly which tag was added to which memory and what text justified it.
/// `applied` distinguishes a dry-run preview from a real write, so the two
/// logs can never be mistaken for each other.
#[must_use]
pub fn proposal_log_line(proposal: &BackfillProposal, applied: bool) -> String {
    let derivations: Vec<serde_json::Value> = proposal
        .derivations
        .iter()
        .map(|derivation| {
            serde_json::json!({
                "tag": derivation.tag,
                "rule": derivation.rule.as_str(),
                "evidence": derivation.evidence,
            })
        })
        .collect();
    serde_json::json!({
        "schema": TAG_BACKFILL_LOG_SCHEMA_V1,
        "memoryId": proposal.memory_id,
        "addTags": proposal.add_tags,
        "derivations": derivations,
        "applied": applied,
    })
    .to_string()
}

/// Options for one `ee index backfill-tags` run.
#[derive(Clone, Copy, Debug)]
pub struct TagBackfillOptions<'a> {
    /// Workspace whose memories are scanned.
    pub workspace_path: &'a Path,
    /// Database backing that workspace.
    pub database_path: &'a Path,
    /// When false, plan only and write nothing.
    pub apply: bool,
    /// Optional cap on how many memories are patched in one run. `None`
    /// means no cap. The plan is always computed over the whole workspace so
    /// the reported totals stay truthful even when the writes are capped.
    pub limit: Option<usize>,
    /// Optional path for the JSONL mutation log.
    pub log_path: Option<&'a Path>,
    /// Actor recorded on each audit row.
    pub actor: Option<&'a str>,
}

/// What one memory's patch actually did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagBackfillOutcome {
    /// Memory that was patched.
    pub memory_id: String,
    /// Tags added.
    pub add_tags: Vec<String>,
    /// Audit rows minted by the patch.
    pub audit_ids: Vec<String>,
    /// Whether the store reported a durable write.
    pub persisted: bool,
    /// Whether the patch actually changed the tag set.
    pub changed: bool,
}

/// Result of a backfill run.
#[derive(Clone, Debug)]
pub struct TagBackfillReport {
    /// Report schema.
    pub schema: &'static str,
    /// Package version, for stable output.
    pub version: &'static str,
    /// True when this run only planned.
    pub dry_run: bool,
    /// Memories examined.
    pub scanned_memory_count: usize,
    /// The plan, including skips and their reasons.
    pub plan: BackfillPlan,
    /// Per-memory results. Empty on a dry run.
    pub outcomes: Vec<TagBackfillOutcome>,
    /// Proposals not attempted because `limit` was reached.
    pub deferred_memory_count: usize,
    /// Where the JSONL log was written, when one was requested.
    pub log_path: Option<String>,
}

impl TagBackfillReport {
    /// Memories actually mutated by this run.
    #[must_use]
    pub fn applied_memory_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.changed)
            .count()
    }

    /// Tags actually written by this run.
    #[must_use]
    pub fn applied_tag_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.changed)
            .map(|outcome| outcome.add_tags.len())
            .sum()
    }
}

/// Read every live memory in the workspace as a backfill candidate.
///
/// Deliberately does NOT go through [`crate::core::memory::list_memories`]:
/// that path truncates bodies for display (`MemorySummary::content_truncated`),
/// and a truncated body would make the planner refuse every long memory. The
/// store rows carry full content, so candidates are built from them directly
/// and `content_truncated` is always false here.
///
/// Tombstoned memories are excluded: retagging an expired memory would be a
/// mutation with no recall benefit, and the tag write path refuses them anyway.
///
/// # Errors
///
/// Returns a [`DomainError`] when the database cannot be opened or queried.
pub fn collect_candidates(
    database_path: &Path,
    workspace_path: &Path,
) -> Result<Vec<BackfillCandidate>, DomainError> {
    let conn =
        DbConnection::open_file_read_only(database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open database read-only: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let workspace_id = crate::core::memory::workspace_id_for_database(&conn, workspace_path);
    let stored = conn
        .list_memories(&workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;

    let ids: Vec<&str> = stored.iter().map(|memory| memory.id.as_str()).collect();
    let tags_by_memory =
        conn.get_memory_tags_batch(&ids)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to load memory tags: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;

    Ok(stored
        .into_iter()
        .map(|memory| {
            let existing_tags = tags_by_memory.get(&memory.id).cloned().unwrap_or_default();
            BackfillCandidate {
                memory_id: memory.id,
                kind: memory.kind,
                // Store rows are never truncated; see the doc comment above.
                content: memory.content,
                content_truncated: false,
                existing_tags,
            }
        })
        .collect())
}

/// Plan, and optionally apply, a content-derived tag backfill.
///
/// Applying reuses [`crate::core::memory::update_memory_tags`] with
/// [`MemoryTagsMode::Patch`] rather than writing tags directly, so every
/// mutation inherits that path's audit rows, index-job scheduling and
/// unchanged/changed semantics. There is deliberately no second write path.
///
/// # Errors
///
/// Returns a [`DomainError`] when candidates cannot be read, when a patch
/// fails, or when the JSONL log cannot be written.
pub fn run_backfill(options: &TagBackfillOptions<'_>) -> Result<TagBackfillReport, DomainError> {
    let candidates = collect_candidates(options.database_path, options.workspace_path)?;
    let scanned_memory_count = candidates.len();
    let plan = plan_backfill(&candidates);

    let attempt_count = options.limit.map_or(plan.proposals.len(), |limit| {
        limit.min(plan.proposals.len())
    });
    let deferred_memory_count = plan.proposals.len() - attempt_count;

    let mut outcomes = Vec::new();
    let mut log_lines = Vec::new();

    for proposal in plan.proposals.iter().take(attempt_count) {
        log_lines.push(proposal_log_line(proposal, options.apply));
        if !options.apply {
            continue;
        }
        let report = crate::core::memory::update_memory_tags(&MemoryTagsOptions {
            workspace_path: options.workspace_path,
            database_path: options.database_path,
            memory_id: &proposal.memory_id,
            mode: MemoryTagsMode::Patch {
                add: proposal.add_tags.clone(),
                remove: Vec::new(),
            },
            actor: options.actor,
            dry_run: false,
            include_tombstoned: false,
        })?;
        outcomes.push(TagBackfillOutcome {
            memory_id: proposal.memory_id.clone(),
            add_tags: proposal.add_tags.clone(),
            audit_ids: report.audit_ids.clone(),
            persisted: report.persisted,
            changed: report.changed,
        });
    }

    let log_path = match options.log_path {
        Some(path) => {
            write_log_lines(path, &log_lines)?;
            Some(path.display().to_string())
        }
        None => None,
    };

    Ok(TagBackfillReport {
        schema: TAG_BACKFILL_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        dry_run: !options.apply,
        scanned_memory_count,
        plan,
        outcomes,
        deferred_memory_count,
        log_path,
    })
}

/// Append `lines` to the JSONL log at `path`, creating parent directories.
///
/// Appends rather than truncates so a later run never destroys the record of
/// an earlier one — the log is audit evidence, and RULE 1 applies to it.
fn write_log_lines(path: &Path, lines: &[String]) -> Result<(), DomainError> {
    use std::io::Write as _;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to create tag-backfill log directory {}: {error}",
                parent.display()
            ),
            repair: Some("Choose a writable --log path.".to_owned()),
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to open tag-backfill log {}: {error}",
                path.display()
            ),
            repair: Some("Choose a writable --log path.".to_owned()),
        })?;
    for line in lines {
        writeln!(file, "{line}").map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to write tag-backfill log {}: {error}",
                path.display()
            ),
            repair: Some("Choose a writable --log path.".to_owned()),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BackfillCandidate, SkipReason, TagRule, derive_tags, plan_backfill, proposal_log_line,
        slugify_attribution,
    };

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

    fn candidate(id: &str, kind: &str, content: &str) -> BackfillCandidate {
        BackfillCandidate {
            memory_id: id.to_owned(),
            kind: kind.to_owned(),
            content: content.to_owned(),
            content_truncated: false,
            existing_tags: Vec::new(),
        }
    }

    #[test]
    fn plan_skips_memories_that_already_carry_tags() {
        let mut already = candidate("mem_b", "decision", "anything");
        already.existing_tags = vec!["genre:decision".to_owned()];
        let plan = plan_backfill(&[already]);

        assert!(
            plan.is_empty(),
            "tagged rows are not this backfill's target"
        );
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].reason, SkipReason::AlreadyTagged);
    }

    #[test]
    fn plan_refuses_truncated_content_instead_of_guessing() {
        // Absence of evidence in a truncated body is not evidence of absence:
        // the marker may simply have been cut off. Refuse the row.
        let mut truncated = candidate("mem_a", "decision", "Position in $RDVT...");
        truncated.content_truncated = true;
        let plan = plan_backfill(&[truncated]);

        assert!(plan.is_empty(), "a truncated body must not be derived from");
        assert_eq!(plan.skipped[0].reason, SkipReason::ContentTruncated);
    }

    #[test]
    fn plan_records_no_evidence_rather_than_dropping_the_row() {
        let plan = plan_backfill(&[candidate("mem_a", "some-custom-kind", "nothing here")]);

        assert!(plan.is_empty());
        assert_eq!(
            plan.skipped[0].reason,
            SkipReason::NoEvidence,
            "an unjustifiable row must be reported, not silently omitted"
        );
    }

    #[test]
    fn plan_proposes_only_justified_tags() {
        let plan = plan_backfill(&[candidate(
            "mem_a",
            "decision",
            "Underwrite $RDVT. Analyst: Jane Roe",
        )]);

        assert_eq!(plan.proposed_memory_count(), 1);
        assert_eq!(
            plan.proposals[0].add_tags,
            vec!["analyst:jane-roe", "genre:decision", "ticker:rdvt"]
        );
        assert_eq!(
            plan.proposals[0].derivations.len(),
            plan.proposals[0].add_tags.len(),
            "every proposed tag must carry its derivation"
        );
        assert_eq!(plan.proposed_tag_count(), 3);
    }

    #[test]
    fn applying_the_plan_makes_a_second_run_a_no_op() {
        let first = plan_backfill(&[candidate("mem_a", "decision", "Underwrite $RDVT.")]);
        assert!(!first.is_empty(), "first run must have work to do");

        // Simulate the apply: the row now carries the tags it was given.
        let mut applied = candidate("mem_a", "decision", "Underwrite $RDVT.");
        applied.existing_tags = first.proposals[0].add_tags.clone();
        let second = plan_backfill(&[applied]);

        assert!(
            second.is_empty(),
            "re-running over an applied workspace must write nothing"
        );
        assert_eq!(second.proposed_tag_count(), 0);
    }

    #[test]
    fn plan_is_deterministic_and_ordered_by_memory_id() {
        let candidates = vec![
            candidate("mem_c", "decision", "Underwrite $RDVT."),
            candidate("mem_a", "fact", "Nothing notable."),
            candidate("mem_b", "rule", "Run fmt before release."),
        ];
        let first = plan_backfill(&candidates);
        let second = plan_backfill(&candidates);
        assert_eq!(first, second, "planning must be a pure function");

        let proposed: Vec<&str> = first
            .proposals
            .iter()
            .map(|proposal| proposal.memory_id.as_str())
            .collect();
        let mut sorted = proposed.clone();
        sorted.sort_unstable();
        assert_eq!(proposed, sorted, "proposals must be memory-id ordered");

        let skipped: Vec<&str> = first
            .skipped
            .iter()
            .map(|skip| skip.memory_id.as_str())
            .collect();
        let mut sorted_skips = skipped.clone();
        sorted_skips.sort_unstable();
        assert_eq!(skipped, sorted_skips, "skips must be memory-id ordered");
    }

    #[test]
    fn log_line_records_the_tag_its_rule_and_its_evidence() {
        let plan = plan_backfill(&[candidate("mem_a", "fact", "Bought $RDVT today.")]);
        let line = proposal_log_line(&plan.proposals[0], true);
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("log line must be valid JSON");

        assert_eq!(parsed["schema"], super::TAG_BACKFILL_LOG_SCHEMA_V1);
        assert_eq!(parsed["memoryId"], "mem_a");
        assert_eq!(parsed["applied"], true);

        let derivations = parsed["derivations"]
            .as_array()
            .expect("derivations must be an array");
        let ticker = derivations
            .iter()
            .find(|entry| entry["tag"] == "ticker:rdvt")
            .expect("ticker derivation must be logged");
        assert_eq!(ticker["rule"], "ticker_cashtag");
        assert_eq!(
            ticker["evidence"], "$RDVT",
            "the log must record the span that justified the tag"
        );
    }

    #[test]
    fn dry_run_and_applied_log_lines_are_distinguishable() {
        let plan = plan_backfill(&[candidate("mem_a", "fact", "Bought $RDVT today.")]);
        let preview = proposal_log_line(&plan.proposals[0], false);
        let applied = proposal_log_line(&plan.proposals[0], true);
        assert_ne!(
            preview, applied,
            "a preview log must never be mistakable for a write log"
        );
    }

    #[test]
    fn log_appends_across_runs_instead_of_truncating() {
        // The JSONL log is audit evidence. A second run must never erase the
        // record of the first, so the writer opens in append mode.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("backfill.jsonl");

        super::write_log_lines(&log, &["first".to_owned()]).expect("first write");
        super::write_log_lines(&log, &["second".to_owned()]).expect("second write");

        let contents = std::fs::read_to_string(&log).expect("read log");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(
            lines,
            vec!["first", "second"],
            "an earlier run's log lines must survive a later run"
        );
    }

    #[test]
    fn log_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir
            .path()
            .join("nested")
            .join("deeper")
            .join("backfill.jsonl");

        super::write_log_lines(&log, &["only".to_owned()]).expect("write into new dirs");

        assert!(log.is_file(), "log file must exist under created parents");
    }

    #[test]
    fn writing_zero_lines_still_yields_a_readable_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("backfill.jsonl");

        super::write_log_lines(&log, &[]).expect("empty write");

        assert_eq!(
            std::fs::read_to_string(&log).expect("read log"),
            "",
            "an empty run produces an empty log, not a missing one"
        );
    }

    #[test]
    fn skip_reason_wire_names_are_stable() {
        assert_eq!(SkipReason::AlreadyTagged.as_str(), "already_tagged");
        assert_eq!(SkipReason::NoEvidence.as_str(), "no_evidence");
        assert_eq!(SkipReason::ContentTruncated.as_str(), "content_truncated");
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
