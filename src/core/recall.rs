//! ADR 0064 code-anchored recall — the reverse lookup from a code surface
//! (path globs, exact symbols, or a parsed git-diff path set) to the memories
//! anchored on it. bd-u875s.2.
//!
//! This module is the deterministic query engine: candidate matching, the
//! ADR §3 ranking objective (`freshness × confidence × level_tilt` with a
//! warnings-first kind bonus), conjunctive `--kind`/`--level` filtering,
//! token-budget truncation with a stable continuation cursor, and the three
//! ADR §5 degradation codes. It is pure: rows come in as
//! [`RecallCandidateRow`] values (fetched from the `memory_anchor_index`
//! derived table by the DB layer), and the same inputs always produce a
//! byte-identical [`RecallReport`]. A recall failure must never block an
//! edit, so everything here degrades instead of erroring.

use crate::models::{MemoryAnchorFreshnessState, MemoryAnchorKind};
use crate::search::scoring::{DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR, freshness_drift_multiplier};

/// Response payload schema carried under `ee.response.v2` `data.recall`.
pub const RECALL_SCHEMA_V1: &str = "ee.recall.v1";

/// Continuation cursor schema/prefix. Superseded by the ADR 0063 governor
/// cursor vocabulary once that surface lands; the wire shape here is stable
/// and tamper-evident in the meantime.
pub const RECALL_CURSOR_SCHEMA_V1: &str = "ee.recall.cursor.v1";

/// The reverse index has no rows for this workspace (nothing anchored yet).
/// Info-severity; never a hard error (ADR 0064 §5).
pub const ANCHOR_INDEX_EMPTY_CODE: &str = "anchor_index_empty";

/// Reverse-index generation is behind the DB generation.
pub const ANCHOR_INDEX_STALE_CODE: &str = "anchor_index_stale";

/// The index had anchored rows for the requested surface but `--kind`/
/// `--level`/`--stale` filters removed them all — distinct from
/// [`ANCHOR_INDEX_EMPTY_CODE`] so hook authors can tell the difference.
pub const RECALL_FILTERED_EMPTY_CODE: &str = "recall_filtered_empty";

/// Repair command for a stale or empty reverse index.
pub const ANCHOR_INDEX_REPAIR: &str = "ee index rebuild --workspace .";

/// Bounded candidate scan (ADR 0064 §3). The per-path/per-symbol lookups are
/// already narrow; this cap is the defensive ceiling for pathological
/// workspaces. Callers fetching more rows than this should truncate before
/// calling [`evaluate_recall`]; the engine also enforces it.
pub const RECALL_CANDIDATE_SCAN_CAP: usize = 4096;

/// Single-line content preview budget (chars), mirroring the existing
/// 240-char preview discipline.
pub const RECALL_CONTENT_PREVIEW_MAX_CHARS: usize = 240;

/// Memory kinds that receive the ADR §3 warnings-first bonus.
pub const RECALL_KIND_BONUS_KINDS: [&str; 3] = ["failure", "risk", "anti-pattern"];

/// Multiplier applied to [`RECALL_KIND_BONUS_KINDS`] memories.
pub const RECALL_KIND_BONUS: f32 = 1.15;

/// One candidate row from the `memory_anchor_index` reverse index, joined
/// with the owning memory's ranking fields. `normalized_path` is set for
/// `path` anchors, `symbol` for `symbol` anchors; the engine ignores rows
/// where neither is set.
#[derive(Clone, Debug, PartialEq)]
pub struct RecallCandidateRow {
    pub memory_id: String,
    pub anchor_kind: MemoryAnchorKind,
    pub normalized_path: Option<String>,
    pub symbol: Option<String>,
    pub freshness_state: MemoryAnchorFreshnessState,
    /// Reverse-index row generation (stamped at write time).
    pub row_generation: i64,
    /// Owning memory's level wire form (`procedural | semantic | episodic |
    /// working`).
    pub level: String,
    /// Owning memory's kind wire form (e.g. `rule`, `failure`, `risk`).
    pub kind: String,
    /// Owning memory's confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Owning memory's full content; the engine derives the preview.
    pub content: String,
    /// True when the owning memory carries a tombstone. Tombstoned memories
    /// are excluded at query time regardless of index hygiene.
    pub tombstoned: bool,
    pub tags: Vec<String>,
    pub provenance: Vec<RecallProvenanceRef>,
}

/// One provenance pointer on a recalled memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallProvenanceRef {
    pub uri: String,
    pub source_type: String,
}

/// A recall request. `paths` are case-sensitive fnmatch-style globs,
/// `symbols` are exact names, and `diff_paths` is an already-parsed changed
/// path set (see [`diff_changed_paths`]); the three selector families compose
/// as OR with result dedup by memory id. `kinds`/`levels` filter the matched
/// set conjunctively BEFORE ranking (ADR 0064 §2). With no selectors at all
/// the engine deterministically matches nothing — the CLI surface
/// (bd-u875s.3) requires at least one selector.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecallQuery {
    pub paths: Vec<String>,
    pub symbols: Vec<String>,
    pub diff_paths: Vec<String>,
    pub kinds: Vec<String>,
    pub levels: Vec<String>,
    /// Keep only `suspect | stale` items and append per-item repair hints —
    /// the agent-facing view of what ADR 0056 penalizes silently.
    pub stale_only: bool,
    /// Token budget for the item list; `None` means unbounded.
    pub max_tokens: Option<u32>,
    /// Rank offset for continuation (decoded from a validated cursor).
    pub offset: usize,
}

/// The anchor a memory was recalled through.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallAnchor {
    pub kind: String,
    pub path: Option<String>,
    pub symbol: Option<String>,
}

/// ADR §3 score factors, surfaced so every item is explainable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecallScoreComponents {
    pub freshness: f32,
    pub confidence: f32,
    pub level_tilt: f32,
    pub kind_bonus: f32,
}

/// One ranked recall item (`ee.recall.v1` `items[]`).
#[derive(Clone, Debug, PartialEq)]
pub struct RecallItem {
    pub memory_id: String,
    pub anchor: RecallAnchor,
    pub freshness_state: String,
    pub score_components: RecallScoreComponents,
    pub score: f32,
    pub level: String,
    pub kind: String,
    pub content_preview: String,
    pub provenance: Vec<RecallProvenanceRef>,
    pub tags: Vec<String>,
    /// Suggested next command for stale items; `None` when current.
    pub repair: Option<String>,
}

/// One `degraded[]` entry the recall pipeline may emit. Pinned to the
/// canonical envelope shape (`code`, `severity`, `message`, `repair`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

impl RecallDegradation {
    #[must_use]
    fn from_model_lifecycle(degradation: &crate::core::model::ModelLifecycleDegradation) -> Self {
        Self {
            code: degradation.code,
            severity: degradation.severity,
            message: degradation.message.clone(),
            repair: degradation.repair.clone(),
        }
    }
}

/// Deterministic recall result (`ee.recall.v1`).
#[derive(Clone, Debug, PartialEq)]
pub struct RecallReport {
    pub schema: &'static str,
    pub items: Vec<RecallItem>,
    /// `MAX(generation)` across the workspace's reverse-index rows; `None`
    /// when the index has no rows at all.
    pub index_generation: Option<i64>,
    pub db_generation: i64,
    pub degraded: Vec<RecallDegradation>,
    /// Post-filter match count before offset/budget truncation.
    pub total_matched: usize,
    pub truncated: bool,
    pub dropped_count: usize,
    pub continuation_cursor: Option<String>,
}

/// Level tilt per ADR 0064 §3. Unknown levels (defensive: the DB constrains
/// the vocabulary) sink to the working-memory tilt rather than inventing a
/// new constant.
#[must_use]
pub fn recall_level_tilt(level: &str) -> f32 {
    match level {
        "procedural" => 1.0,
        "semantic" => 0.8,
        "episodic" => 0.6,
        _ => 0.3,
    }
}

/// Kind bonus per ADR 0064 §3: warnings first.
#[must_use]
pub fn recall_kind_bonus(kind: &str) -> f32 {
    if RECALL_KIND_BONUS_KINDS.contains(&kind) {
        RECALL_KIND_BONUS
    } else {
        1.0
    }
}

/// Case-sensitive fnmatch-style glob match (ADR 0064 §2): `*` matches any
/// run of characters (including `/`), `?` matches one character, `[...]` and
/// `[!...]` match character sets. An empty pattern matches only the empty
/// string, so it never matches a real normalized path.
#[must_use]
pub fn recall_glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut p = 0;
    let mut t = 0;
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() {
            match pattern[p] {
                '*' => {
                    star = Some((p, t));
                    p += 1;
                    continue;
                }
                '?' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                '[' => match match_char_class(&pattern, p, text[t]) {
                    Some((true, next_p)) => {
                        p = next_p;
                        t += 1;
                        continue;
                    }
                    Some((false, _)) => {}
                    // Unterminated class: `[` is a literal.
                    None => {
                        if text[t] == '[' {
                            p += 1;
                            t += 1;
                            continue;
                        }
                    }
                },
                literal => {
                    if literal == text[t] {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
            }
        }
        // Mismatch: backtrack to the last `*`, consuming one more text char.
        match star {
            Some((star_p, star_t)) => {
                p = star_p + 1;
                t = star_t + 1;
                star = Some((star_p, star_t + 1));
            }
            None => return false,
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Match one `[...]` character class starting at `pattern[open]`. Returns
/// `(matched, index_after_class)` or `None` when the class is unterminated
/// (in which case `[` is treated as a literal by the caller's fallthrough).
fn match_char_class(pattern: &[char], open: usize, ch: char) -> Option<(bool, usize)> {
    let mut i = open + 1;
    let negated = matches!(pattern.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let class_start = i;
    let mut matched = false;
    while i < pattern.len() {
        if pattern[i] == ']' && i > class_start {
            return Some((matched != negated, i + 1));
        }
        if i + 2 < pattern.len() && pattern[i + 1] == '-' && pattern[i + 2] != ']' {
            if pattern[i] <= ch && ch <= pattern[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if pattern[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// Normalize a caller-supplied path selector to the reverse index's
/// normalized form: strip a leading `./`. Absolute selectors are returned
/// as-is and simply never match (normalized paths are workspace-relative by
/// construction); rejecting them here would turn a no-op into an error on
/// the hook path.
#[must_use]
pub fn normalize_recall_path_selector(selector: &str) -> String {
    selector.strip_prefix("./").unwrap_or(selector).to_owned()
}

/// Extract the changed path set from `git diff --name-only` output (one path
/// per line) or a unified diff (`+++ b/<path>` headers). Paths are
/// normalized, deduplicated, and sorted; `/dev/null` targets are skipped.
#[must_use]
pub fn diff_changed_paths(diff_text: &str) -> Vec<String> {
    let mut paths = std::collections::BTreeSet::new();
    for line in diff_text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            let target = rest.trim();
            if target == "/dev/null" {
                continue;
            }
            let target = target.strip_prefix("b/").unwrap_or(target);
            paths.insert(normalize_recall_path_selector(target));
            continue;
        }
        if line.starts_with("--- ")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("@@")
            || line.starts_with('+')
            || line.starts_with('-')
            || line.starts_with(' ')
            || line.starts_with('\\')
        {
            continue;
        }
        // `--name-only` form: a bare path per line.
        paths.insert(normalize_recall_path_selector(line));
    }
    paths.into_iter().collect()
}

/// Deterministic single-line content preview (≤ [`RECALL_CONTENT_PREVIEW_MAX_CHARS`]
/// chars): whitespace collapsed, char-boundary-safe truncation with a
/// trailing ellipsis.
#[must_use]
pub fn recall_content_preview(content: &str) -> String {
    let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= RECALL_CONTENT_PREVIEW_MAX_CHARS {
        return single_line;
    }
    let mut preview: String = single_line
        .chars()
        .take(RECALL_CONTENT_PREVIEW_MAX_CHARS - 1)
        .collect();
    preview.push('…');
    preview
}

/// Deterministic token estimate for one rendered recall item, consistent
/// with the whitespace-based estimate the handoff surface uses. Counts the
/// fields an agent actually reads: id, anchor display, preview, and tags.
#[must_use]
pub fn recall_item_token_estimate(item: &RecallItem) -> usize {
    let anchor_display = item
        .anchor
        .path
        .as_deref()
        .or(item.anchor.symbol.as_deref())
        .unwrap_or_default();
    let text_len_words = item.memory_id.split_whitespace().count()
        + anchor_display.split_whitespace().count()
        + item.content_preview.split_whitespace().count()
        + item
            .tags
            .iter()
            .map(|tag| tag.split_whitespace().count())
            .sum::<usize>();
    text_len_words.saturating_mul(4) / 3
}

/// Stable hash binding a continuation cursor to the logical query (selector
/// and filter fields only — budget and offset intentionally excluded so a
/// continuation page reuses the same hash).
#[must_use]
pub fn recall_query_hash(query: &RecallQuery) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RECALL_CURSOR_SCHEMA_V1.as_bytes());
    let mut feed = |label: &str, values: &[String]| {
        hasher.update(b"\0");
        hasher.update(label.as_bytes());
        let mut sorted: Vec<&String> = values.iter().collect();
        sorted.sort();
        for value in sorted {
            hasher.update(b"\0");
            hasher.update(value.as_bytes());
        }
    };
    feed("paths", &query.paths);
    feed("symbols", &query.symbols);
    feed("diff_paths", &query.diff_paths);
    feed("kinds", &query.kinds);
    feed("levels", &query.levels);
    hasher.update(if query.stale_only {
        b"\0stale:1"
    } else {
        b"\0stale:0"
    });
    hasher.finalize().to_hex().chars().take(12).collect()
}

/// Why a continuation cursor was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecallCursorError {
    Malformed,
    SignatureMismatch,
    QueryMismatch,
    StaleGeneration { cursor: i64, current: i64 },
}

impl std::fmt::Display for RecallCursorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => formatter.write_str("recall cursor is malformed"),
            Self::SignatureMismatch => formatter.write_str("recall cursor signature mismatch"),
            Self::QueryMismatch => {
                formatter.write_str("recall cursor was issued for a different query")
            }
            Self::StaleGeneration { cursor, current } => write!(
                formatter,
                "recall cursor generation {cursor} is stale (current {current})"
            ),
        }
    }
}

/// A decoded continuation cursor. Encoding is tamper-evident (BLAKE3
/// signature over the payload) and generation-bound: a cursor issued against
/// an older DB generation is rejected rather than silently reordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallCursor {
    pub offset: usize,
    pub query_hash: String,
    pub db_generation: i64,
}

impl RecallCursor {
    #[must_use]
    pub fn encode(&self) -> String {
        let payload = format!(
            "{RECALL_CURSOR_SCHEMA_V1}:g{}:o{}:q{}",
            self.db_generation, self.offset, self.query_hash
        );
        format!("{payload}:s{}", cursor_signature(&payload))
    }

    /// Decode and structurally validate a cursor string (schema, field
    /// shapes, signature). Query/generation binding is checked by
    /// [`Self::validate`].
    pub fn decode(encoded: &str) -> Result<Self, RecallCursorError> {
        let (payload, signature) = encoded
            .rsplit_once(":s")
            .ok_or(RecallCursorError::Malformed)?;
        if cursor_signature(payload) != signature {
            return Err(RecallCursorError::SignatureMismatch);
        }
        let rest = payload
            .strip_prefix(RECALL_CURSOR_SCHEMA_V1)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or(RecallCursorError::Malformed)?;
        let mut parts = rest.split(':');
        let generation = parts
            .next()
            .and_then(|part| part.strip_prefix('g'))
            .and_then(|raw| raw.parse::<i64>().ok())
            .ok_or(RecallCursorError::Malformed)?;
        let offset = parts
            .next()
            .and_then(|part| part.strip_prefix('o'))
            .and_then(|raw| raw.parse::<usize>().ok())
            .ok_or(RecallCursorError::Malformed)?;
        let query_hash = parts
            .next()
            .and_then(|part| part.strip_prefix('q'))
            .ok_or(RecallCursorError::Malformed)?
            .to_owned();
        if parts.next().is_some() {
            return Err(RecallCursorError::Malformed);
        }
        Ok(Self {
            offset,
            query_hash,
            db_generation: generation,
        })
    }

    /// Bind-check a decoded cursor against the live query and DB generation.
    pub fn validate(
        &self,
        expected_query_hash: &str,
        current_db_generation: i64,
    ) -> Result<(), RecallCursorError> {
        if self.query_hash != expected_query_hash {
            return Err(RecallCursorError::QueryMismatch);
        }
        if self.db_generation != current_db_generation {
            return Err(RecallCursorError::StaleGeneration {
                cursor: self.db_generation,
                current: current_db_generation,
            });
        }
        Ok(())
    }
}

fn cursor_signature(payload: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.recall.cursor.v1\0sig\0");
    hasher.update(payload.as_bytes());
    hasher.finalize().to_hex().chars().take(12).collect()
}

/// Evaluate a recall query over candidate rows (ADR 0064 §§2–5).
///
/// `index_generation` is `MAX(generation)` over the workspace's reverse-index
/// rows (`None` when the index is empty); `db_generation` is the live
/// workspace generation. Determinism contract: identical inputs produce a
/// byte-identical report, ranking ties resolve by ascending memory id, and a
/// smaller token budget yields a strict prefix of a larger budget's items.
#[must_use]
pub fn evaluate_recall(
    query: &RecallQuery,
    rows: &[RecallCandidateRow],
    index_generation: Option<i64>,
    db_generation: i64,
) -> RecallReport {
    let mut degraded = Vec::new();

    match index_generation {
        None => degraded.push(RecallDegradation {
            code: ANCHOR_INDEX_EMPTY_CODE,
            severity: "info",
            message: "anchor reverse index has no rows for this workspace; nothing is anchored yet"
                .to_owned(),
            repair: Some(ANCHOR_INDEX_REPAIR.to_owned()),
        }),
        Some(generation) if generation < db_generation => degraded.push(RecallDegradation {
            code: ANCHOR_INDEX_STALE_CODE,
            severity: "low",
            message: format!(
                "anchor reverse index generation {generation} is behind database generation {db_generation}; results may miss recent memories"
            ),
            repair: Some(ANCHOR_INDEX_REPAIR.to_owned()),
        }),
        Some(_) => {}
    }

    let normalized_paths: Vec<String> = query
        .paths
        .iter()
        .map(|selector| normalize_recall_path_selector(selector))
        .collect();
    let diff_set: std::collections::BTreeSet<String> = query
        .diff_paths
        .iter()
        .map(|selector| normalize_recall_path_selector(selector))
        .collect();
    let symbol_set: std::collections::BTreeSet<&str> =
        query.symbols.iter().map(String::as_str).collect();

    // Surface matching (OR across selector families), tombstone exclusion,
    // bounded scan, dedup by memory id keeping the freshest anchor.
    let mut best_per_memory: std::collections::BTreeMap<&str, &RecallCandidateRow> =
        std::collections::BTreeMap::new();
    for row in rows.iter().take(RECALL_CANDIDATE_SCAN_CAP) {
        if row.tombstoned {
            continue;
        }
        let path_matched = row.normalized_path.as_deref().is_some_and(|path| {
            diff_set.contains(path)
                || normalized_paths
                    .iter()
                    .any(|pattern| recall_glob_match(pattern, path))
        });
        let symbol_matched = row
            .symbol
            .as_deref()
            .is_some_and(|symbol| symbol_set.contains(symbol));
        if !path_matched && !symbol_matched {
            continue;
        }
        best_per_memory
            .entry(row.memory_id.as_str())
            .and_modify(|kept| {
                if anchor_row_preference(row) < anchor_row_preference(kept) {
                    *kept = row;
                }
            })
            .or_insert(row);
    }
    let surface_match_count = best_per_memory.len();

    // Conjunctive pre-ranking filters (ADR §2).
    let filtered: Vec<&RecallCandidateRow> = best_per_memory
        .into_values()
        .filter(|row| query.kinds.is_empty() || query.kinds.iter().any(|kind| kind == &row.kind))
        .filter(|row| {
            query.levels.is_empty() || query.levels.iter().any(|level| level == &row.level)
        })
        .filter(|row| {
            !query.stale_only
                || matches!(
                    row.freshness_state,
                    MemoryAnchorFreshnessState::Suspect | MemoryAnchorFreshnessState::Stale
                )
        })
        .collect();

    if surface_match_count > 0 && filtered.is_empty() {
        degraded.push(RecallDegradation {
            code: RECALL_FILTERED_EMPTY_CODE,
            severity: "info",
            message: format!(
                "{surface_match_count} anchored memorie(s) matched the surface but kind/level/stale filters removed them all"
            ),
            repair: None,
        });
    }

    // Rank (ADR §3): score descending, stable tie-break by memory id.
    let mut scored: Vec<RecallItem> = filtered.into_iter().map(score_row).collect();
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    let total_matched = scored.len();
    let paged: Vec<RecallItem> = scored.into_iter().skip(query.offset).collect();

    // Token-budget truncation: keep the longest prefix under budget.
    let (items, dropped_count) = match query.max_tokens {
        None => (paged, 0),
        Some(budget) => {
            let budget = budget as usize;
            let total = paged.len();
            let mut kept = Vec::new();
            let mut spent = 0_usize;
            let mut dropped = 0_usize;
            for (idx, item) in paged.into_iter().enumerate() {
                let cost = recall_item_token_estimate(&item);
                if spent + cost <= budget {
                    spent += cost;
                    kept.push(item);
                } else {
                    dropped = total - idx;
                    break;
                }
            }
            (kept, dropped)
        }
    };

    let truncated = dropped_count > 0;
    let continuation_cursor = truncated.then(|| {
        RecallCursor {
            offset: query.offset + items.len(),
            query_hash: recall_query_hash(query),
            db_generation,
        }
        .encode()
    });

    RecallReport {
        schema: RECALL_SCHEMA_V1,
        items,
        index_generation,
        db_generation,
        degraded,
        total_matched,
        truncated,
        dropped_count,
        continuation_cursor,
    }
}

/// Dedup preference when one memory matches through several anchors: freshest
/// first, then path anchors before symbol anchors, then the anchor value —
/// all deterministic.
fn anchor_row_preference(row: &RecallCandidateRow) -> (u8, MemoryAnchorKind, String) {
    (
        row.freshness_state.rank(),
        row.anchor_kind,
        row.normalized_path
            .clone()
            .or_else(|| row.symbol.clone())
            .unwrap_or_default(),
    )
}

fn score_row(row: &RecallCandidateRow) -> RecallItem {
    let freshness =
        freshness_drift_multiplier(row.freshness_state, DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR);
    let confidence = row.confidence.clamp(0.0, 1.0);
    let level_tilt = recall_level_tilt(&row.level);
    let kind_bonus = recall_kind_bonus(&row.kind);
    let score = freshness * confidence * level_tilt * kind_bonus;
    let stale_ish = matches!(
        row.freshness_state,
        MemoryAnchorFreshnessState::Suspect | MemoryAnchorFreshnessState::Stale
    );
    RecallItem {
        memory_id: row.memory_id.clone(),
        anchor: RecallAnchor {
            kind: row.anchor_kind.as_str().to_owned(),
            path: row.normalized_path.clone(),
            symbol: row.symbol.clone(),
        },
        freshness_state: row.freshness_state.as_str().to_owned(),
        score_components: RecallScoreComponents {
            freshness,
            confidence,
            level_tilt,
            kind_bonus,
        },
        score,
        level: row.level.clone(),
        kind: row.kind.clone(),
        content_preview: recall_content_preview(&row.content),
        provenance: row.provenance.clone(),
        tags: row.tags.clone(),
        repair: stale_ish.then(|| format!("ee why {} --workspace . --json", row.memory_id)),
    }
}

/// Fetch candidates from the `memory_anchor_index` derived table and
/// evaluate the query (bd-u875s.2 DB wiring). Narrow indexed lookups serve
/// exact path selectors, diff path sets, and symbols; glob selectors fall
/// back to a bounded scan of the workspace's path rows. Tags are
/// batch-loaded; provenance maps the owning memory's provenance URI.
pub fn run_recall(
    connection: &crate::db::DbConnection,
    workspace_id: &str,
    query: &RecallQuery,
) -> crate::db::Result<RecallReport> {
    let db_generation = i64::try_from(
        connection
            .get_workspace_generation(workspace_id)?
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX);
    let index_generation = connection.memory_anchor_index_generation(workspace_id)?;

    let normalized_selectors: Vec<String> = query
        .paths
        .iter()
        .map(|selector| normalize_recall_path_selector(selector))
        .collect();
    let has_glob_selector = normalized_selectors
        .iter()
        .any(|selector| selector.contains(['*', '?', '[']));
    let mut exact_paths: Vec<String> = normalized_selectors
        .iter()
        .filter(|selector| !selector.contains(['*', '?', '[']))
        .cloned()
        .chain(
            query
                .diff_paths
                .iter()
                .map(|selector| normalize_recall_path_selector(selector)),
        )
        .collect();
    exact_paths.sort();
    exact_paths.dedup();

    let mut candidates = Vec::new();
    if has_glob_selector {
        candidates.extend(connection.query_anchor_index_path_candidates(
            workspace_id,
            None,
            RECALL_CANDIDATE_SCAN_CAP,
        )?);
    } else if !exact_paths.is_empty() {
        candidates.extend(connection.query_anchor_index_path_candidates(
            workspace_id,
            Some(&exact_paths),
            RECALL_CANDIDATE_SCAN_CAP,
        )?);
    }
    if !query.symbols.is_empty() {
        candidates.extend(connection.query_anchor_index_symbol_candidates(
            workspace_id,
            &query.symbols,
            RECALL_CANDIDATE_SCAN_CAP,
        )?);
    }

    let memory_ids: Vec<&str> = {
        let mut ids: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.memory_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let tags_by_memory = connection.get_memory_tags_batch(&memory_ids)?;

    let rows: Vec<RecallCandidateRow> = candidates
        .into_iter()
        .map(|candidate| {
            let tags = tags_by_memory
                .get(&candidate.memory_id)
                .cloned()
                .unwrap_or_default();
            let provenance = candidate
                .provenance_uri
                .as_ref()
                .map(|uri| {
                    vec![RecallProvenanceRef {
                        uri: uri.clone(),
                        source_type: "memory_provenance".to_owned(),
                    }]
                })
                .unwrap_or_default();
            RecallCandidateRow {
                memory_id: candidate.memory_id,
                anchor_kind: candidate.anchor_kind,
                normalized_path: candidate.normalized_path,
                symbol: candidate.symbol,
                freshness_state: candidate.freshness_state,
                row_generation: candidate.generation,
                level: candidate.level,
                kind: candidate.kind,
                confidence: candidate.confidence,
                content: candidate.content,
                tombstoned: candidate.tombstoned,
                tags,
                provenance,
            }
        })
        .collect();

    let mut report = evaluate_recall(query, &rows, index_generation, db_generation);
    if let Some(degradation) = recall_model_lifecycle_degradation(connection, workspace_id)
        && !report.degraded.iter().any(|existing| {
            existing.code == degradation.code && existing.message == degradation.message
        })
    {
        report.degraded.push(degradation);
    }
    Ok(report)
}

fn recall_model_lifecycle_degradation(
    connection: &crate::db::DbConnection,
    workspace_id: &str,
) -> Option<RecallDegradation> {
    let workspace = connection.get_workspace(workspace_id).ok().flatten()?;
    let report = crate::core::model::build_model_lifecycle_report_for_workspace(
        std::path::Path::new(&workspace.path),
        None,
        Some(connection),
    )
    .ok()?;
    report
        .semantic_surface_degradation("recall")
        .map(|degradation| RecallDegradation::from_model_lifecycle(&degradation))
}

// ---------------------------------------------------------------------------
// CLI-facing surface helpers (bd-u875s.3)
// ---------------------------------------------------------------------------

/// `git_unavailable`-family degraded code (ADR 0064 §2) for a failed
/// read-only git shell-out behind `--diff`/`--diff-staged`. Recall-specific
/// rather than the shared `git_unavailable` because that code's fixture
/// pins swarm-brief/workspace-hygiene repair strings; a git failure here
/// degrades the diff selector to an empty path set and never blocks recall.
pub const RECALL_GIT_UNAVAILABLE_CODE: &str = "recall_git_unavailable";

/// Collect the changed-path set for `--diff <ref>` / `--diff-staged` by
/// shelling out to git read-only (`git -C <workspace> diff --name-only`).
/// Path extraction only; hunk ranges are reserved for future span-level
/// matching (ADR 0064 §2). Errors are returned as plain strings so the CLI
/// layer can degrade (`git_unavailable`) instead of failing the command.
pub fn collect_diff_paths_via_git(
    workspace_path: &std::path::Path,
    reference: Option<&str>,
    staged: bool,
) -> Result<Vec<String>, String> {
    if let Some(reference) = reference {
        // Refs are positional git arguments; refuse option-shaped values so
        // a hostile selector cannot smuggle flags into the invocation.
        if reference.starts_with('-') || reference.is_empty() {
            return Err(format!(
                "invalid git ref {reference:?}: refs must not be empty or start with '-'"
            ));
        }
    }
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(workspace_path)
        .arg("diff")
        .arg("--name-only");
    if staged {
        command.arg("--cached");
    }
    if let Some(reference) = reference {
        command.arg(reference);
    }
    command.arg("--");
    let output = command
        .output()
        .map_err(|error| format!("failed to spawn git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git diff exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(diff_changed_paths(&String::from_utf8_lossy(&output.stdout)))
}

/// Outcome of resolving an optional `--cursor` flag against the live query
/// and DB generation (budget-continuation lane, `ee.recall.cursor.v1`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecallCursorResolution {
    /// No cursor supplied; start at rank offset zero.
    Fresh,
    /// Cursor validated; resume from this rank offset.
    Resume(usize),
    /// Cursor malformed, tampered, or bound to a different query
    /// (`cursor_invalid`).
    RejectedInvalid,
    /// Cursor was issued at an older DB generation (`cursor_stale`); pages
    /// cannot partition the result set honestly across writes.
    RejectedStale {
        cursor_generation: i64,
        current_generation: i64,
    },
}

/// Resolve an optional encoded cursor against the query's stable hash and
/// the current DB generation. Rejections map to the ADR 0063 cursor
/// vocabulary (`cursor_invalid` / `cursor_stale`); they degrade, never error.
#[must_use]
pub fn resolve_recall_cursor(
    encoded: Option<&str>,
    query: &RecallQuery,
    current_db_generation: i64,
) -> RecallCursorResolution {
    let Some(encoded) = encoded else {
        return RecallCursorResolution::Fresh;
    };
    match RecallCursor::decode(encoded) {
        Err(_) => RecallCursorResolution::RejectedInvalid,
        Ok(cursor) => match cursor.validate(&recall_query_hash(query), current_db_generation) {
            Ok(()) => RecallCursorResolution::Resume(cursor.offset),
            Err(RecallCursorError::StaleGeneration { cursor, current }) => {
                RecallCursorResolution::RejectedStale {
                    cursor_generation: cursor,
                    current_generation: current,
                }
            }
            Err(_) => RecallCursorResolution::RejectedInvalid,
        },
    }
}

/// One response-level degraded entry for the recall CLI surface: the three
/// engine codes plus the CLI-only git/cursor/budget-truncation entries, with
/// optional structured `details` for the envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct RecallDegradedEntry {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
    pub details: Option<serde_json::Value>,
}

impl RecallDegradedEntry {
    /// Lift an engine degradation into the CLI view.
    #[must_use]
    pub fn from_engine(entry: &RecallDegradation) -> Self {
        Self {
            code: entry.code.to_owned(),
            severity: entry.severity.to_owned(),
            message: entry.message.clone(),
            repair: entry.repair.clone(),
            details: None,
        }
    }

    /// `git_unavailable` (warning): the `--diff` selector degraded to an
    /// empty path set because the read-only git shell-out failed.
    #[must_use]
    pub fn git_unavailable(reason: &str) -> Self {
        Self {
            code: RECALL_GIT_UNAVAILABLE_CODE.to_owned(),
            severity: "warning".to_owned(),
            message: format!(
                "--diff selector degraded to an empty path set because git was unavailable: {reason}"
            ),
            repair: Some(
                "Re-run inside a git worktree with git on PATH, or use --path/--symbol selectors."
                    .to_owned(),
            ),
            details: None,
        }
    }

    /// `cursor_invalid` (low), mirroring the canonical ADR 0063 wording.
    #[must_use]
    pub fn cursor_invalid() -> Self {
        Self {
            code: "cursor_invalid".to_owned(),
            severity: "low".to_owned(),
            message: "Continuation cursor failed validation (MAC mismatch, parameter mismatch, \
                      or legacy format)."
                .to_owned(),
            repair: Some(
                "Re-run the command without --cursor to start a fresh page sequence.".to_owned(),
            ),
            details: None,
        }
    }

    /// `cursor_stale` (low), mirroring the canonical ADR 0063 wording.
    #[must_use]
    pub fn cursor_stale(cursor_generation: i64, current_generation: i64) -> Self {
        Self {
            code: "cursor_stale".to_owned(),
            severity: "low".to_owned(),
            message: format!(
                "Continuation cursor was issued at DB generation {cursor_generation} but the \
                 workspace is now at generation {current_generation}; pages cannot partition the \
                 result set honestly across writes."
            ),
            repair: Some(
                "Re-run the command without --cursor to start a fresh page sequence.".to_owned(),
            ),
            details: None,
        }
    }

    /// `output_truncated_budget` (info): trailing items dropped to satisfy
    /// `--budget-tokens`. One truncation vocabulary across surfaces (ADR
    /// 0064 §5 supersedes the early `recall_budget_truncated` name); the
    /// recall budget lane reuses the governor code with recall-appropriate
    /// repair text and carries the `ee.recall.cursor.v1` continuation cursor
    /// in `details`.
    #[must_use]
    pub fn budget_truncated(
        dropped_count: usize,
        continuation_cursor: &str,
        budget_tokens: u32,
    ) -> Self {
        Self {
            code: crate::output::governor::OUTPUT_TRUNCATED_BUDGET_CODE.to_owned(),
            severity: "info".to_owned(),
            message: format!(
                "Dropped {dropped_count} trailing item(s) at the declared truncation point to \
                 satisfy the recall budget of {budget_tokens} tokens."
            ),
            repair: Some(
                "Re-run with a larger --budget-tokens value, or resume from \
                 details.continuationCursor with --cursor."
                    .to_owned(),
            ),
            details: Some(serde_json::json!({
                "droppedCount": dropped_count,
                "continuationCursor": continuation_cursor,
            })),
        }
    }

    /// Envelope-shaped JSON (`code`, `severity`, `message`, `repair`,
    /// optional `details`).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        });
        if let Some(details) = &self.details
            && let Some(object) = entry.as_object_mut()
        {
            object.insert("details".to_owned(), details.clone());
        }
        entry
    }
}

/// The normalized query echoed back under `data.recall.query` (ADR 0064
/// appendix). Selector and filter fields only — offsets are cursor-internal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecallQueryEcho {
    pub paths: Vec<String>,
    pub symbols: Vec<String>,
    pub diff_ref: Option<String>,
    pub diff_staged: bool,
    pub kinds: Vec<String>,
    pub levels: Vec<String>,
    pub stale_only: bool,
    pub budget_tokens: Option<u32>,
}

/// Four-decimal score rounding for stable JSON output, mirroring the
/// CLI-wide `score_json_value` discipline.
fn score_json(value: f32) -> serde_json::Value {
    let rounded = (f64::from(value) * 10_000.0).round() / 10_000.0;
    serde_json::Number::from_f64(rounded).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

fn recall_item_json(item: &RecallItem) -> serde_json::Value {
    serde_json::json!({
        "memoryId": item.memory_id,
        "anchor": {
            "kind": item.anchor.kind,
            "path": item.anchor.path,
            "symbol": item.anchor.symbol,
        },
        "freshnessState": item.freshness_state,
        "scoreComponents": {
            "freshness": score_json(item.score_components.freshness),
            "confidence": score_json(item.score_components.confidence),
            "levelTilt": score_json(item.score_components.level_tilt),
            "kindBonus": score_json(item.score_components.kind_bonus),
        },
        "score": score_json(item.score),
        "level": item.level,
        "kind": item.kind,
        "contentPreview": item.content_preview,
        "provenance": item.provenance.iter().map(|reference| serde_json::json!({
            "uri": reference.uri,
            "sourceType": reference.source_type,
        })).collect::<Vec<_>>(),
        "tags": item.tags,
        "repair": item.repair,
    })
}

/// Build the `data` payload for the `ee.response.v2` envelope:
/// `{"command": "recall", "recall": {…ee.recall.v1…}}`. The declared
/// governor truncation point is `data.recall.items[]`.
#[must_use]
pub fn recall_data_json(report: &RecallReport, query: &RecallQueryEcho) -> serde_json::Value {
    serde_json::json!({
        "command": "recall",
        "recall": {
            "schema": report.schema,
            "query": {
                "paths": query.paths,
                "symbols": query.symbols,
                "diffRef": query.diff_ref,
                "diffStaged": query.diff_staged,
                "kinds": query.kinds,
                "levels": query.levels,
                "staleOnly": query.stale_only,
                "budgetTokens": query.budget_tokens,
            },
            "items": report.items.iter().map(recall_item_json).collect::<Vec<_>>(),
            "indexGeneration": report.index_generation,
            "dbGeneration": report.db_generation,
            "totalMatched": report.total_matched,
            "truncated": report.truncated,
            "droppedCount": report.dropped_count,
            "continuationCursor": report.continuation_cursor,
        },
    })
}

/// Render the token-tight markdown prepend block (pack markdown discipline:
/// smallest output, provenance per item, repair hints on stale items).
#[must_use]
pub fn render_recall_markdown(report: &RecallReport, degraded: &[RecallDegradedEntry]) -> String {
    let mut output = String::new();
    if report.truncated {
        output.push_str(&format!(
            "## recall · {} of {} anchored memories ({} dropped by budget)\n",
            report.items.len(),
            report.total_matched,
            report.dropped_count
        ));
    } else {
        output.push_str(&format!(
            "## recall · {} anchored memorie(s)\n",
            report.items.len()
        ));
    }
    for (rank, item) in report.items.iter().enumerate() {
        let anchor_display = item
            .anchor
            .path
            .as_deref()
            .or(item.anchor.symbol.as_deref())
            .unwrap_or("-");
        output.push_str(&format!(
            "\n{}. {} · {} · {}/{} · {}\n   {}\n",
            rank + 1,
            item.memory_id,
            anchor_display,
            item.level,
            item.kind,
            item.freshness_state,
            item.content_preview
        ));
        if !item.provenance.is_empty() {
            let uris: Vec<&str> = item
                .provenance
                .iter()
                .map(|reference| reference.uri.as_str())
                .collect();
            output.push_str(&format!("   src: {}\n", uris.join(", ")));
        }
        if let Some(repair) = &item.repair {
            output.push_str(&format!("   repair: {repair}\n"));
        }
    }
    for entry in degraded {
        match &entry.repair {
            Some(repair) => {
                output.push_str(&format!("\ndegraded: {} ({})\n", entry.code, repair));
            }
            None => output.push_str(&format!("\ndegraded: {}\n", entry.code)),
        }
    }
    output
}

/// Build the empty page returned when a continuation cursor is rejected.
/// Generations are reported honestly; items stay empty so a rejected cursor
/// can never duplicate or skip elements of a prior page sequence.
#[must_use]
pub fn empty_recall_report_for_rejected_cursor(
    index_generation: Option<i64>,
    db_generation: i64,
) -> RecallReport {
    RecallReport {
        schema: RECALL_SCHEMA_V1,
        items: Vec::new(),
        index_generation,
        db_generation,
        degraded: Vec::new(),
        total_matched: 0,
        truncated: false,
        dropped_count: 0,
        continuation_cursor: None,
    }
}

#[cfg(test)]
mod cli_surface_tests {
    use super::*;

    fn sample_item() -> RecallItem {
        RecallItem {
            memory_id: "mem_00000000000000000000000001".to_owned(),
            anchor: RecallAnchor {
                kind: "path".to_owned(),
                path: Some("src/db/mod.rs".to_owned()),
                symbol: None,
            },
            freshness_state: "current".to_owned(),
            score_components: RecallScoreComponents {
                freshness: 1.0,
                confidence: 0.8,
                level_tilt: 1.0,
                kind_bonus: 1.0,
            },
            score: 0.8,
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content_preview: "Always run the verify script.".to_owned(),
            provenance: vec![RecallProvenanceRef {
                uri: "test://prov".to_owned(),
                source_type: "memory_provenance".to_owned(),
            }],
            tags: vec!["ci".to_owned()],
            repair: None,
        }
    }

    fn sample_report(items: Vec<RecallItem>) -> RecallReport {
        let total_matched = items.len();
        RecallReport {
            schema: RECALL_SCHEMA_V1,
            items,
            index_generation: Some(7),
            db_generation: 7,
            degraded: Vec::new(),
            total_matched,
            truncated: false,
            dropped_count: 0,
            continuation_cursor: None,
        }
    }

    #[test]
    fn cursor_resolution_fresh_resume_invalid_stale() {
        let query = RecallQuery {
            paths: vec!["src/db/mod.rs".to_owned()],
            ..RecallQuery::default()
        };
        assert_eq!(
            resolve_recall_cursor(None, &query, 7),
            RecallCursorResolution::Fresh
        );

        let cursor = RecallCursor {
            offset: 3,
            query_hash: recall_query_hash(&query),
            db_generation: 7,
        };
        assert_eq!(
            resolve_recall_cursor(Some(&cursor.encode()), &query, 7),
            RecallCursorResolution::Resume(3)
        );
        assert_eq!(
            resolve_recall_cursor(Some("garbage"), &query, 7),
            RecallCursorResolution::RejectedInvalid
        );
        assert_eq!(
            resolve_recall_cursor(Some(&cursor.encode()), &query, 9),
            RecallCursorResolution::RejectedStale {
                cursor_generation: 7,
                current_generation: 9,
            }
        );
        // A cursor bound to a different query is invalid, not stale.
        let other_query = RecallQuery {
            paths: vec!["src/core/recall.rs".to_owned()],
            ..RecallQuery::default()
        };
        assert_eq!(
            resolve_recall_cursor(Some(&cursor.encode()), &other_query, 7),
            RecallCursorResolution::RejectedInvalid
        );
    }

    #[test]
    fn data_json_shape_matches_adr_appendix() {
        let report = sample_report(vec![sample_item()]);
        let query = RecallQueryEcho {
            paths: vec!["src/db/*.rs".to_owned()],
            ..RecallQueryEcho::default()
        };
        let data = recall_data_json(&report, &query);
        assert_eq!(data["command"], "recall");
        let recall = &data["recall"];
        assert_eq!(recall["schema"], RECALL_SCHEMA_V1);
        assert_eq!(recall["query"]["paths"][0], "src/db/*.rs");
        assert_eq!(recall["query"]["staleOnly"], false);
        assert_eq!(recall["items"][0]["memoryId"], sample_item().memory_id);
        assert_eq!(recall["items"][0]["anchor"]["path"], "src/db/mod.rs");
        assert_eq!(recall["items"][0]["scoreComponents"]["confidence"], 0.8);
        assert_eq!(recall["indexGeneration"], 7);
        assert_eq!(recall["dbGeneration"], 7);
        assert_eq!(recall["truncated"], false);
        assert_eq!(recall["continuationCursor"], serde_json::Value::Null);
    }

    #[test]
    fn markdown_block_is_token_tight_with_provenance_and_degraded() {
        let mut stale = sample_item();
        stale.memory_id = "mem_00000000000000000000000002".to_owned();
        stale.freshness_state = "stale".to_owned();
        stale.repair =
            Some("ee why mem_00000000000000000000000002 --workspace . --json".to_owned());
        let report = sample_report(vec![sample_item(), stale]);
        let degraded = vec![RecallDegradedEntry::from_engine(&RecallDegradation {
            code: ANCHOR_INDEX_STALE_CODE,
            severity: "low",
            message: "behind".to_owned(),
            repair: Some(ANCHOR_INDEX_REPAIR.to_owned()),
        })];
        let markdown = render_recall_markdown(&report, &degraded);
        assert!(markdown.starts_with("## recall · 2 anchored memorie(s)\n"));
        assert!(markdown.contains("1. mem_00000000000000000000000001 · src/db/mod.rs"));
        assert!(markdown.contains("src: test://prov"));
        assert!(markdown.contains("repair: ee why mem_00000000000000000000000002"));
        assert!(markdown.contains("degraded: anchor_index_stale (ee index rebuild"));
    }

    #[test]
    fn markdown_block_reports_budget_truncation_counts() {
        let mut report = sample_report(vec![sample_item()]);
        report.total_matched = 5;
        report.truncated = true;
        report.dropped_count = 4;
        let markdown = render_recall_markdown(&report, &[]);
        assert!(markdown.starts_with("## recall · 1 of 5 anchored memories (4 dropped by budget)"));
    }

    #[test]
    fn budget_truncated_entry_carries_cursor_details() {
        let entry = RecallDegradedEntry::budget_truncated(4, "cursor-string", 400);
        assert_eq!(
            entry.code,
            crate::output::governor::OUTPUT_TRUNCATED_BUDGET_CODE
        );
        assert_eq!(entry.severity, "info");
        let json = entry.to_json();
        assert_eq!(json["details"]["droppedCount"], 4);
        assert_eq!(json["details"]["continuationCursor"], "cursor-string");
    }

    #[test]
    fn git_ref_validation_rejects_option_shaped_refs() {
        let workspace = std::path::Path::new(".");
        let result = collect_diff_paths_via_git(workspace, Some("--output=/tmp/x"), false);
        assert!(result.is_err());
        let result = collect_diff_paths_via_git(workspace, Some(""), false);
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(memory_id: &str, path: Option<&str>, symbol: Option<&str>) -> RecallCandidateRow {
        RecallCandidateRow {
            memory_id: memory_id.to_owned(),
            anchor_kind: if path.is_some() {
                MemoryAnchorKind::Path
            } else {
                MemoryAnchorKind::Symbol
            },
            normalized_path: path.map(str::to_owned),
            symbol: symbol.map(str::to_owned),
            freshness_state: MemoryAnchorFreshnessState::Current,
            row_generation: 7,
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            confidence: 0.8,
            content: "Always run the verify script before pushing.".to_owned(),
            tombstoned: false,
            tags: vec!["ci".to_owned()],
            provenance: vec![RecallProvenanceRef {
                uri: "test://prov".to_owned(),
                source_type: "test".to_owned(),
            }],
        }
    }

    fn path_query(globs: &[&str]) -> RecallQuery {
        RecallQuery {
            paths: globs.iter().map(|glob| (*glob).to_owned()).collect(),
            ..RecallQuery::default()
        }
    }

    #[test]
    fn glob_matching_edges() {
        // Empty glob matches nothing real.
        assert!(!recall_glob_match("", "src/db/mod.rs"));
        assert!(recall_glob_match("", ""));
        // Exact, star (crossing `/`), question mark, char classes.
        assert!(recall_glob_match("src/db/mod.rs", "src/db/mod.rs"));
        assert!(recall_glob_match("src/*.rs", "src/db/mod.rs"));
        assert!(recall_glob_match("src/**", "src/core/recall.rs"));
        assert!(recall_glob_match("src/db/mod.r?", "src/db/mod.rs"));
        assert!(recall_glob_match("src/[cd]b/mod.rs", "src/db/mod.rs"));
        assert!(recall_glob_match("src/[!x]b/mod.rs", "src/db/mod.rs"));
        assert!(!recall_glob_match("src/[!d]b/mod.rs", "src/db/mod.rs"));
        assert!(recall_glob_match(
            "tests/[a-f]ixture.rs",
            "tests/fixture.rs"
        ));
        // Case sensitivity is preserved.
        assert!(!recall_glob_match("SRC/*.rs", "src/db/mod.rs"));
        // Trailing-star and star-only forms.
        assert!(recall_glob_match("*", "anything/at/all.rs"));
        assert!(!recall_glob_match("src/*.toml", "src/db/mod.rs"));
        // Unterminated class falls back to literal `[`.
        assert!(recall_glob_match("src/[ab", "src/[ab"));
        assert!(!recall_glob_match("src/[ab", "src/a"));
    }

    #[test]
    fn path_selector_normalization() {
        assert_eq!(
            normalize_recall_path_selector("./src/db/mod.rs"),
            "src/db/mod.rs"
        );
        assert_eq!(
            normalize_recall_path_selector("src/db/mod.rs"),
            "src/db/mod.rs"
        );
        // Absolute selectors stay as-is and simply never match the
        // workspace-relative index.
        assert_eq!(normalize_recall_path_selector("/etc/passwd"), "/etc/passwd");
        let rows = vec![row("mem_a", Some("src/db/mod.rs"), None)];
        let report = evaluate_recall(&path_query(&["/src/db/mod.rs"]), &rows, Some(7), 7);
        assert!(report.items.is_empty());
    }

    #[test]
    fn diff_path_parsing_handles_both_forms() {
        let name_only = "src/db/mod.rs\nsrc/core/recall.rs\n\nsrc/db/mod.rs\n";
        assert_eq!(
            diff_changed_paths(name_only),
            vec!["src/core/recall.rs".to_owned(), "src/db/mod.rs".to_owned()]
        );
        let unified = "diff --git a/src/db/mod.rs b/src/db/mod.rs\nindex 111..222 100644\n--- a/src/db/mod.rs\n+++ b/src/db/mod.rs\n@@ -1,2 +1,2 @@\n-old line\n+new line\n context\ndiff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n";
        assert_eq!(
            diff_changed_paths(unified),
            vec!["src/db/mod.rs".to_owned()]
        );
    }

    #[test]
    fn ranking_is_deterministic_with_stable_tie_breaks() {
        let mut first = row("mem_b", Some("src/a.rs"), None);
        let mut second = row("mem_a", Some("src/b.rs"), None);
        // Identical scores -> ascending memory id.
        first.confidence = 0.8;
        second.confidence = 0.8;
        let rows = vec![first.clone(), second.clone()];
        let report = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(7), 7);
        let ids: Vec<&str> = report
            .items
            .iter()
            .map(|item| item.memory_id.as_str())
            .collect();
        assert_eq!(ids, vec!["mem_a", "mem_b"]);
        // Same inputs -> identical report (determinism).
        let again = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(7), 7);
        assert_eq!(report, again);
        // Reversed input order does not change ranking.
        let reversed = evaluate_recall(&path_query(&["src/*.rs"]), &[second, first], Some(7), 7);
        assert_eq!(report, reversed);
    }

    #[test]
    fn scoring_follows_adr_objective() {
        let mut warning = row("mem_warn", Some("src/a.rs"), None);
        warning.kind = "failure".to_owned();
        warning.level = "episodic".to_owned();
        warning.confidence = 0.5;
        warning.freshness_state = MemoryAnchorFreshnessState::Suspect;
        let report = evaluate_recall(&path_query(&["src/*.rs"]), &[warning], Some(7), 7);
        let item = &report.items[0];
        assert!((item.score_components.freshness - 0.7).abs() < 1e-6);
        assert!((item.score_components.confidence - 0.5).abs() < 1e-6);
        assert!((item.score_components.level_tilt - 0.6).abs() < 1e-6);
        assert!((item.score_components.kind_bonus - 1.15).abs() < 1e-6);
        let expected = 0.7 * 0.5 * 0.6 * 1.15;
        assert!((item.score - expected).abs() < 1e-6);
        // Suspect/stale items carry a repair hint.
        assert_eq!(
            item.repair.as_deref(),
            Some("ee why mem_warn --workspace . --json")
        );
    }

    #[test]
    fn tombstoned_memories_are_excluded() {
        let mut dead = row("mem_dead", Some("src/a.rs"), None);
        dead.tombstoned = true;
        let live = row("mem_live", Some("src/a.rs"), None);
        let report = evaluate_recall(&path_query(&["src/*.rs"]), &[dead, live], Some(7), 7);
        let ids: Vec<&str> = report
            .items
            .iter()
            .map(|item| item.memory_id.as_str())
            .collect();
        assert_eq!(ids, vec!["mem_live"]);
    }

    #[test]
    fn dedup_keeps_freshest_anchor_per_memory() {
        let mut stale_path = row("mem_a", Some("src/a.rs"), None);
        stale_path.freshness_state = MemoryAnchorFreshnessState::Stale;
        let mut fresh_symbol = row("mem_a", None, Some("Recall::run"));
        fresh_symbol.anchor_kind = MemoryAnchorKind::Symbol;
        let query = RecallQuery {
            paths: vec!["src/*.rs".to_owned()],
            symbols: vec!["Recall::run".to_owned()],
            ..RecallQuery::default()
        };
        let report = evaluate_recall(&query, &[stale_path, fresh_symbol], Some(7), 7);
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].anchor.kind, "symbol");
        assert_eq!(report.items[0].freshness_state, "current");
    }

    #[test]
    fn filters_compose_conjunctively_and_emit_filtered_empty() {
        let rows = vec![row("mem_a", Some("src/a.rs"), None)];
        let mut query = path_query(&["src/*.rs"]);
        query.kinds = vec!["failure".to_owned()];
        let report = evaluate_recall(&query, &rows, Some(7), 7);
        assert!(report.items.is_empty());
        assert_eq!(report.degraded.len(), 1);
        assert_eq!(report.degraded[0].code, RECALL_FILTERED_EMPTY_CODE);
        assert_eq!(report.degraded[0].severity, "info");
    }

    #[test]
    fn empty_index_and_stale_index_emit_distinct_codes() {
        let empty = evaluate_recall(&path_query(&["src/*.rs"]), &[], None, 5);
        assert_eq!(empty.degraded.len(), 1);
        assert_eq!(empty.degraded[0].code, ANCHOR_INDEX_EMPTY_CODE);
        assert_eq!(empty.degraded[0].severity, "info");
        assert_eq!(empty.degraded[0].repair, Some(ANCHOR_INDEX_REPAIR.to_owned()));

        let rows = vec![row("mem_a", Some("src/a.rs"), None)];
        let stale = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(3), 5);
        assert_eq!(stale.degraded.len(), 1);
        assert_eq!(stale.degraded[0].code, ANCHOR_INDEX_STALE_CODE);
        assert_eq!(stale.degraded[0].severity, "low");
        assert_eq!(stale.degraded[0].repair, Some(ANCHOR_INDEX_REPAIR.to_owned()));
        // Stale detection never blocks results.
        assert_eq!(stale.items.len(), 1);

        let current = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(5), 5);
        assert!(current.degraded.is_empty());
    }

    #[test]
    fn budget_truncation_boundaries() {
        let rows: Vec<RecallCandidateRow> = (0..4)
            .map(|index| row(&format!("mem_{index}"), Some("src/a.rs"), None))
            .collect();
        let unbounded = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(7), 7);
        assert_eq!(unbounded.items.len(), 4);
        assert!(!unbounded.truncated);
        assert!(unbounded.continuation_cursor.is_none());

        let per_item = recall_item_token_estimate(&unbounded.items[0]);
        assert!(per_item > 0);

        // Zero budget: nothing fits, everything dropped.
        let mut query = path_query(&["src/*.rs"]);
        query.max_tokens = Some(0);
        let zero = evaluate_recall(&query, &rows, Some(7), 7);
        assert!(zero.items.is_empty());
        assert_eq!(zero.dropped_count, 4);
        assert!(zero.truncated);
        assert!(zero.continuation_cursor.is_some());

        // Exactly-fits: all four kept, no cursor.
        let exact_budget = u32::try_from(per_item * 4).expect("budget fits in u32");
        query.max_tokens = Some(exact_budget);
        let exact = evaluate_recall(&query, &rows, Some(7), 7);
        assert_eq!(exact.items.len(), 4);
        assert!(!exact.truncated);
        assert!(exact.continuation_cursor.is_none());

        // One token short: a strict prefix is kept, the rest dropped.
        query.max_tokens = Some(exact_budget - 1);
        let short = evaluate_recall(&query, &rows, Some(7), 7);
        assert_eq!(short.items.len(), 3);
        assert_eq!(short.dropped_count, 1);
        assert!(short.truncated);
        // Smaller budget yields a strict prefix of the larger budget's items.
        assert_eq!(short.items[..], exact.items[..3],);
    }

    #[test]
    fn continuation_cursor_round_trip_and_paging() {
        let rows: Vec<RecallCandidateRow> = (0..4)
            .map(|index| row(&format!("mem_{index}"), Some("src/a.rs"), None))
            .collect();
        let mut query = path_query(&["src/*.rs"]);
        let per_item = {
            let probe = evaluate_recall(&query, &rows, Some(7), 7);
            recall_item_token_estimate(&probe.items[0])
        };
        query.max_tokens = Some(u32::try_from(per_item * 2).expect("budget fits"));
        let first_page = evaluate_recall(&query, &rows, Some(7), 7);
        assert_eq!(first_page.items.len(), 2);
        assert_eq!(first_page.dropped_count, 2);
        let encoded = first_page
            .continuation_cursor
            .clone()
            .expect("cursor present");

        let cursor = RecallCursor::decode(&encoded).expect("cursor decodes");
        cursor
            .validate(&recall_query_hash(&query), 7)
            .expect("cursor validates");
        assert_eq!(cursor.offset, 2);

        let mut second_query = query.clone();
        second_query.offset = cursor.offset;
        let second_page = evaluate_recall(&second_query, &rows, Some(7), 7);
        assert_eq!(second_page.items.len(), 2);
        assert!(!second_page.truncated);
        // Pages partition the ranked list without overlap.
        let first_ids: Vec<&str> = first_page
            .items
            .iter()
            .map(|item| item.memory_id.as_str())
            .collect();
        let second_ids: Vec<&str> = second_page
            .items
            .iter()
            .map(|item| item.memory_id.as_str())
            .collect();
        assert_eq!(first_ids, vec!["mem_0", "mem_1"]);
        assert_eq!(second_ids, vec!["mem_2", "mem_3"]);
    }

    #[test]
    fn cursor_rejects_tamper_query_mismatch_and_stale_generation() {
        let cursor = RecallCursor {
            offset: 2,
            query_hash: "abcdefabcdef".to_owned(),
            db_generation: 7,
        };
        let encoded = cursor.encode();
        assert_eq!(RecallCursor::decode(&encoded), Ok(cursor.clone()));

        // Tampered payload -> signature mismatch.
        let tampered = encoded.replace(":o2:", ":o9:");
        assert_eq!(
            RecallCursor::decode(&tampered),
            Err(RecallCursorError::SignatureMismatch)
        );
        assert_eq!(
            RecallCursor::decode("garbage"),
            Err(RecallCursorError::Malformed)
        );

        // Wrong query hash -> query mismatch.
        assert_eq!(
            cursor.validate("000000000000", 7),
            Err(RecallCursorError::QueryMismatch)
        );
        // Generation moved -> stale rejection, never silent reordering.
        assert_eq!(
            cursor.validate("abcdefabcdef", 9),
            Err(RecallCursorError::StaleGeneration {
                cursor: 7,
                current: 9
            })
        );
        assert_eq!(cursor.validate("abcdefabcdef", 7), Ok(()));
    }

    #[test]
    fn query_hash_is_order_insensitive_and_budget_insensitive() {
        let base = RecallQuery {
            paths: vec!["src/*.rs".to_owned(), "docs/*.md".to_owned()],
            symbols: vec!["A::b".to_owned()],
            ..RecallQuery::default()
        };
        let mut reordered = base.clone();
        reordered.paths.reverse();
        assert_eq!(recall_query_hash(&base), recall_query_hash(&reordered));
        let mut budgeted = base.clone();
        budgeted.max_tokens = Some(100);
        budgeted.offset = 2;
        assert_eq!(recall_query_hash(&base), recall_query_hash(&budgeted));
        let mut different = base;
        different.stale_only = true;
        assert_ne!(recall_query_hash(&different), recall_query_hash(&reordered));
    }

    #[test]
    fn stale_only_filters_and_keeps_repair_hints() {
        let current = row("mem_current", Some("src/a.rs"), None);
        let mut suspect = row("mem_suspect", Some("src/a.rs"), None);
        suspect.freshness_state = MemoryAnchorFreshnessState::Suspect;
        let mut query = path_query(&["src/*.rs"]);
        query.stale_only = true;
        let report = evaluate_recall(&query, &[current, suspect], Some(7), 7);
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].memory_id, "mem_suspect");
        assert!(report.items[0].repair.is_some());
    }

    #[test]
    fn content_preview_is_single_line_and_bounded() {
        let multiline = "first line\nsecond   line\tthird";
        assert_eq!(
            recall_content_preview(multiline),
            "first line second line third"
        );
        let long = "word ".repeat(100);
        let preview = recall_content_preview(&long);
        assert!(preview.chars().count() <= RECALL_CONTENT_PREVIEW_MAX_CHARS);
        assert!(preview.ends_with('…'));
        // Multibyte safety: truncation respects char boundaries.
        let unicode = "é".repeat(500);
        let unicode_preview = recall_content_preview(&unicode);
        assert_eq!(
            unicode_preview.chars().count(),
            RECALL_CONTENT_PREVIEW_MAX_CHARS
        );
    }

    #[test]
    fn no_selectors_match_nothing_deterministically() {
        let rows = vec![row("mem_a", Some("src/a.rs"), None)];
        let report = evaluate_recall(&RecallQuery::default(), &rows, Some(7), 7);
        assert!(report.items.is_empty());
        assert_eq!(report.total_matched, 0);
        // No surface was requested, so no filtered-empty degradation either.
        assert!(report.degraded.is_empty());
    }

    fn wrapper_test_db() -> (crate::db::DbConnection, String) {
        let connection = crate::db::DbConnection::open_memory().expect("open in-memory db");
        connection.migrate().expect("migrate");
        let workspace_id = format!("wsp_{:026}", 1);
        connection
            .insert_workspace(
                &workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/recall-wrapper-test".to_owned(),
                    name: Some("recall-wrapper-test".to_owned()),
                },
            )
            .expect("insert workspace");
        (connection, workspace_id)
    }

    fn wrapper_test_file_db() -> (tempfile::TempDir, crate::db::DbConnection, String) {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_path = temp.path().canonicalize().expect("canonical workspace");
        std::fs::create_dir_all(workspace_path.join(".ee")).expect("create .ee");
        let database_path = workspace_path.join(".ee").join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path).expect("open file db");
        connection.migrate().expect("migrate");
        let workspace_id = format!("wsp_{:026}", 2);
        connection
            .insert_workspace(
                &workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("recall-file-wrapper-test".to_owned()),
                },
            )
            .expect("insert workspace");
        (temp, connection, workspace_id)
    }

    fn wrapper_insert_memory(
        connection: &crate::db::DbConnection,
        workspace_id: &str,
        id: &str,
        content: &str,
    ) {
        connection
            .insert_memory(
                id,
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://recall-wrapper".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["recall-test".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert memory");
    }

    #[test]
    fn run_recall_round_trips_through_the_reverse_index() {
        let (connection, workspace_id) = wrapper_test_db();
        let memory_id = format!("mem_{:026}", 1);
        wrapper_insert_memory(
            &connection,
            &workspace_id,
            &memory_id,
            "Check `src/db/mod.rs` and `DbConnection::open_memory()` before edits.",
        );

        // Exact path, glob, and symbol selectors all resolve through the
        // derived table written by insert_memory's single extraction walk.
        for query in [
            RecallQuery {
                paths: vec!["src/db/mod.rs".to_owned()],
                ..RecallQuery::default()
            },
            RecallQuery {
                paths: vec!["src/db/*.rs".to_owned()],
                ..RecallQuery::default()
            },
            RecallQuery {
                symbols: vec!["DbConnection::open_memory".to_owned()],
                ..RecallQuery::default()
            },
        ] {
            let report = run_recall(&connection, &workspace_id, &query).expect("run recall");
            assert_eq!(report.items.len(), 1, "query {query:?} must match");
            assert_eq!(report.items[0].memory_id, memory_id);
            assert_eq!(report.items[0].tags, vec!["recall-test".to_owned()]);
            assert_eq!(report.items[0].provenance.len(), 1);
            // Freshly written rows carry the current generation: no
            // degradations, index generation matches DB generation.
            assert!(
                report.degraded.is_empty(),
                "unexpected: {:?}",
                report.degraded
            );
            assert_eq!(report.index_generation, Some(report.db_generation));
        }
    }

    #[test]
    fn run_recall_threads_model_lifecycle_lexical_only_degradation() {
        let (_temp, connection, workspace_id) = wrapper_test_file_db();
        let memory_id = format!("mem_{:026}", 9);
        wrapper_insert_memory(
            &connection,
            &workspace_id,
            &memory_id,
            "Check `src/core/model.rs` when semantic lifecycle readiness changes.",
        );

        let report = run_recall(
            &connection,
            &workspace_id,
            &RecallQuery {
                paths: vec!["src/core/model.rs".to_owned()],
                ..RecallQuery::default()
            },
        )
        .expect("run recall");

        assert_eq!(report.items.len(), 1);
        let lifecycle = report
            .degraded
            .iter()
            .find(|degradation| degradation.code == "embed_model_unavailable")
            .expect("model lifecycle degraded entry");
        assert!(
            lifecycle.message.contains("lexical-only"),
            "unexpected lifecycle message: {}",
            lifecycle.message
        );
    }

    #[test]
    fn run_recall_reports_empty_then_stale_index_honestly() {
        let (connection, workspace_id) = wrapper_test_db();

        // No memories at all: empty-index degradation, never an error.
        let empty = run_recall(
            &connection,
            &workspace_id,
            &RecallQuery {
                paths: vec!["src/**".to_owned()],
                ..RecallQuery::default()
            },
        )
        .expect("run recall on empty index");
        assert!(empty.items.is_empty());
        assert_eq!(empty.degraded.len(), 1);
        assert_eq!(empty.degraded[0].code, ANCHOR_INDEX_EMPTY_CODE);

        // One anchored memory: fresh. A later anchorless write advances the
        // DB generation without touching the reverse index, so recall
        // reports the index stale until a rebuild re-stamps it.
        let anchored = format!("mem_{:026}", 2);
        wrapper_insert_memory(
            &connection,
            &workspace_id,
            &anchored,
            "Durable note about `src/core/recall.rs` ranking.",
        );
        let anchorless = format!("mem_{:026}", 3);
        wrapper_insert_memory(
            &connection,
            &workspace_id,
            &anchorless,
            "Plain prose note with no code anchors at all.",
        );
        let query = RecallQuery {
            paths: vec!["src/core/recall.rs".to_owned()],
            ..RecallQuery::default()
        };
        let stale = run_recall(&connection, &workspace_id, &query).expect("run recall");
        assert_eq!(
            stale.items.len(),
            1,
            "stale detection must not block results"
        );
        assert_eq!(stale.degraded.len(), 1);
        assert_eq!(stale.degraded[0].code, ANCHOR_INDEX_STALE_CODE);

        // The rebuild path re-stamps rows at the current generation.
        connection
            .refresh_memory_anchor_index_for_memory(
                &workspace_id,
                &anchored,
                "Durable note about `src/core/recall.rs` ranking.",
            )
            .expect("refresh reverse index");
        let fresh = run_recall(&connection, &workspace_id, &query).expect("run recall");
        assert!(
            fresh.degraded.is_empty(),
            "unexpected: {:?}",
            fresh.degraded
        );
        assert_eq!(fresh.items.len(), 1);
    }

    #[test]
    fn run_recall_excludes_tombstoned_memories() {
        let (connection, workspace_id) = wrapper_test_db();
        let memory_id = format!("mem_{:026}", 4);
        wrapper_insert_memory(
            &connection,
            &workspace_id,
            &memory_id,
            "Tombstone target anchored to `src/db/migrate.rs`.",
        );
        let query = RecallQuery {
            paths: vec!["src/db/migrate.rs".to_owned()],
            ..RecallQuery::default()
        };
        assert_eq!(
            run_recall(&connection, &workspace_id, &query)
                .expect("run recall")
                .items
                .len(),
            1
        );
        connection
            .tombstone_memory(&memory_id)
            .expect("tombstone memory");
        let report = run_recall(&connection, &workspace_id, &query).expect("run recall");
        assert!(
            report.items.is_empty(),
            "tombstoned memories must be excluded at query time"
        );
    }

    #[test]
    fn candidate_scan_is_bounded() {
        let rows: Vec<RecallCandidateRow> = (0..(RECALL_CANDIDATE_SCAN_CAP + 50))
            .map(|index| row(&format!("mem_{index:06}"), Some("src/a.rs"), None))
            .collect();
        let report = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(7), 7);
        assert_eq!(report.total_matched, RECALL_CANDIDATE_SCAN_CAP);
    }
}
