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
    pub repair: Option<&'static str>,
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
            repair: Some(ANCHOR_INDEX_REPAIR),
        }),
        Some(generation) if generation < db_generation => degraded.push(RecallDegradation {
            code: ANCHOR_INDEX_STALE_CODE,
            severity: "low",
            message: format!(
                "anchor reverse index generation {generation} is behind database generation {db_generation}; results may miss recent memories"
            ),
            repair: Some(ANCHOR_INDEX_REPAIR),
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
            let mut kept = Vec::new();
            let mut spent = 0_usize;
            let mut dropped = 0_usize;
            for item in paged {
                let cost = recall_item_token_estimate(&item);
                if spent + cost <= budget {
                    spent += cost;
                    kept.push(item);
                } else {
                    dropped += 1;
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
        assert_eq!(empty.degraded[0].repair, Some(ANCHOR_INDEX_REPAIR));

        let rows = vec![row("mem_a", Some("src/a.rs"), None)];
        let stale = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(3), 5);
        assert_eq!(stale.degraded.len(), 1);
        assert_eq!(stale.degraded[0].code, ANCHOR_INDEX_STALE_CODE);
        assert_eq!(stale.degraded[0].severity, "low");
        assert_eq!(stale.degraded[0].repair, Some(ANCHOR_INDEX_REPAIR));
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

    #[test]
    fn candidate_scan_is_bounded() {
        let rows: Vec<RecallCandidateRow> = (0..(RECALL_CANDIDATE_SCAN_CAP + 50))
            .map(|index| row(&format!("mem_{index:06}"), Some("src/a.rs"), None))
            .collect();
        let report = evaluate_recall(&path_query(&["src/*.rs"]), &rows, Some(7), 7);
        assert_eq!(report.total_matched, RECALL_CANDIDATE_SCAN_CAP);
    }
}
