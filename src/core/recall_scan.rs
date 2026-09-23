//! Bounded, source-admitted traversal of code anchors before result limits.
//!
//! Path and symbol streams advance fairly with keyset pages in one snapshot.
//! Metadata is cheap to scan; only the ranked retained memories load bodies and
//! tags. Nonmatches, obsolete rows and duplicate anchors cannot consume result
//! slots. A hard scan ceiling is reported, never passed off as exhaustive recall.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::{Row, Value};

use super::{
    RECALL_CANDIDATE_SCAN_CAP, RecallCandidateRow, RecallDegradation, RecallQuery,
    anchor_row_preference, normalize_recall_path_selector, recall_glob_match, score_row,
};
use crate::db::{DbConnection, DbError, DbOperation};
use crate::models::memory_anchor::memory_anchor_value_hash;
use crate::models::{MemoryAnchorFreshnessState, MemoryAnchorKind};

#[path = "recall_projection.rs"]
mod projection;

const PAGE_SIZE: usize = 256;
const SOURCE_ROW_LIMIT: usize = 65_536;
const SQL_SELECTOR_LIMIT: usize = 128;

type Key = (String, String, String);

pub(super) struct Scan {
    pub(super) rows: Vec<RecallCandidateRow>,
    pub(super) degraded: Vec<RecallDegradation>,
}

struct Stream {
    kind: &'static str,
    predicate: String,
    selectors: Vec<Value>,
    after: Option<Key>,
    done: bool,
}

fn error() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify anchored recall source authority; no partial result returned"
            .to_owned(),
    }
}

fn text(row: &Row, column: usize) -> crate::db::Result<&str> {
    row.get(column).and_then(Value::as_str).ok_or_else(error)
}

fn optional_text(row: &Row, column: usize) -> crate::db::Result<Option<String>> {
    match row.get(column) {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(value)) => Ok(Some(value.clone())),
        _ => Err(error()),
    }
}

impl Stream {
    fn new(kind: &'static str, selectors: Vec<(String, bool)>) -> Self {
        let mut params = Vec::new();
        let column = if kind == "path" {
            "i.normalized_path"
        } else {
            "i.symbol"
        };
        // Coarse SQL narrowing must be a SUPERSET of the Rust matcher. Exact
        // values and literal glob prefixes use binary comparisons, not LIKE's
        // case folding or SQLite GLOB's different [!...] interpretation.
        let predicate = if selectors.len() > SQL_SELECTOR_LIMIT
            || selectors
                .iter()
                .any(|(value, prefix)| *prefix && value.is_empty())
        {
            String::new()
        } else {
            let terms = selectors
                .into_iter()
                .map(|(value, prefix)| {
                    params.push(Value::Text(value));
                    let slot = params.len() + 1; // ?1 is workspace
                    if prefix {
                        format!("substr({column}, 1, length(?{slot})) = ?{slot} COLLATE BINARY")
                    } else {
                        format!("{column} = ?{slot} COLLATE BINARY")
                    }
                })
                .collect::<Vec<_>>();
            format!(" AND ({})", terms.join(" OR "))
        };
        Self {
            kind,
            predicate,
            selectors: params,
            after: None,
            done: false,
        }
    }

    fn page(
        &self,
        db: &DbConnection,
        workspace: &str,
        limit: usize,
    ) -> crate::db::Result<Vec<Row>> {
        let mut params = vec![Value::Text(workspace.to_owned())];
        params.extend(self.selectors.iter().cloned());
        let mut predicate = self.predicate.clone();
        if let Some((memory, kind, hash)) = &self.after {
            let slot = params.len() + 1;
            params.extend([
                Value::Text(memory.clone()),
                Value::Text(kind.clone()),
                Value::Text(hash.clone()),
            ]);
            predicate.push_str(&format!(" AND (i.memory_id > ?{slot} OR (i.memory_id = ?{slot} AND i.anchor_kind > ?{}) OR (i.memory_id = ?{slot} AND i.anchor_kind = ?{} AND i.anchor_value_hash > ?{}))", slot + 1, slot + 1, slot + 2));
        }
        let limit_slot = params.len() + 1;
        params.push(Value::BigInt(i64::try_from(limit).map_err(|_| error())?));
        // First ten columns deliberately share admission::denial's source
        // projection. LEFT joins expose missing owners rather than hiding them.
        let sql = format!(
            "SELECT m.id, m.workspace_id, m.created_at, m.updated_at, m.valid_from, m.valid_to, m.superseded_at, m.tombstoned_at, s.memory_id, s.revealed_at, i.memory_id, i.anchor_kind, i.anchor_value_hash, i.normalized_path, i.symbol, i.freshness_state, a.freshness_state, i.generation, m.level, m.kind, m.confidence, a.memory_id FROM memory_anchor_index i LEFT JOIN memories m ON m.id = i.memory_id LEFT JOIN memory_seals s ON s.memory_id = m.id LEFT JOIN memory_anchors a ON a.memory_id = i.memory_id AND a.anchor_kind = i.anchor_kind AND a.anchor_value_hash = i.anchor_value_hash WHERE i.workspace_id = ?1 AND i.anchor_kind = '{}'{predicate} ORDER BY i.memory_id ASC, i.anchor_kind ASC, i.anchor_value_hash ASC LIMIT ?{limit_slot}",
            self.kind
        );
        db.query(&sql, &params).map_err(|_| error())
    }
}

fn streams(query: &RecallQuery) -> Vec<Stream> {
    let mut path_selectors = BTreeSet::new();
    for pattern in &query.paths {
        let pattern = normalize_recall_path_selector(pattern);
        if let Some(index) = pattern.find(['*', '?', '[']) {
            path_selectors.insert((pattern[..index].to_owned(), true));
        } else {
            path_selectors.insert((pattern, false));
        }
    }
    for path in &query.diff_paths {
        path_selectors.insert((normalize_recall_path_selector(path), false));
    }
    let mut streams = Vec::new();
    if !path_selectors.is_empty() {
        streams.push(Stream::new("path", path_selectors.into_iter().collect()));
    }
    let symbols = query.symbols.iter().cloned().collect::<BTreeSet<_>>();
    if !symbols.is_empty() {
        streams.push(Stream::new(
            "symbol",
            symbols.into_iter().map(|s| (s, false)).collect(),
        ));
    }
    streams
}

pub(super) fn load(
    db: &DbConnection,
    workspace: &str,
    query: &RecallQuery,
    at: DateTime<Utc>,
) -> crate::db::Result<Scan> {
    load_bounded(
        db,
        workspace,
        query,
        at,
        SOURCE_ROW_LIMIT,
        RECALL_CANDIDATE_SCAN_CAP,
    )
}

fn load_bounded(
    db: &DbConnection,
    workspace: &str,
    query: &RecallQuery,
    at: DateTime<Utc>,
    max_rows: usize,
    max_results: usize,
) -> crate::db::Result<Scan> {
    let paths = query
        .paths
        .iter()
        .map(|p| normalize_recall_path_selector(p))
        .collect::<Vec<_>>();
    let diffs = query
        .diff_paths
        .iter()
        .map(|p| normalize_recall_path_selector(p))
        .collect::<BTreeSet<_>>();
    let symbols = query
        .symbols
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut streams = streams(query);
    let mut scanned = 0;
    let mut best = BTreeMap::<String, RecallCandidateRow>::new();
    let mut denied = BTreeMap::<String, &'static str>::new();
    let mut surface_matches = BTreeSet::new();
    let mut invalid_anchors = 0;
    while scanned < max_rows && streams.iter().any(|stream| !stream.done) {
        let active = streams.iter().filter(|stream| !stream.done).count();
        let allowance = PAGE_SIZE.min((max_rows - scanned).div_ceil(active));
        for stream in &mut streams {
            if stream.done || scanned >= max_rows {
                continue;
            }
            let limit = allowance.min(max_rows - scanned);
            let page = stream.page(db, workspace, limit)?;
            stream.done = page.len() < limit;
            scanned += page.len();
            for row in page {
                let key = (
                    text(&row, 10)?.to_owned(),
                    text(&row, 11)?.to_owned(),
                    text(&row, 12)?.to_owned(),
                );
                if stream.after.as_ref().is_some_and(|after| key <= *after) {
                    return Err(error());
                }
                stream.after = Some(key.clone());
                let path = optional_text(&row, 13)?;
                let symbol = optional_text(&row, 14)?;
                let matches = path.as_deref().is_some_and(|path| {
                    diffs.contains(path)
                        || paths.iter().any(|pattern| recall_glob_match(pattern, path))
                }) || symbol
                    .as_deref()
                    .is_some_and(|symbol| symbols.contains(symbol));
                if !matches {
                    continue;
                }
                if matches!(row.get(0), Some(Value::Null)) {
                    denied.insert(key.0, "missing");
                    continue;
                }
                if text(&row, 0)? != key.0 {
                    return Err(error());
                }
                if let Some(reason) = super::admission::denial(&row, &key.0, workspace, at) {
                    denied.insert(key.0, reason);
                    continue;
                }
                if key.0.parse::<crate::models::MemoryId>().is_err()
                    || text(&row, 18)?
                        .parse::<crate::models::MemoryLevel>()
                        .is_err()
                    || text(&row, 19)?
                        .parse::<crate::models::MemoryKind>()
                        .is_err()
                {
                    denied.insert(key.0, "malformed");
                    continue;
                }
                let kind = MemoryAnchorKind::parse(&key.1).ok_or_else(error)?;
                let value = match (kind, path.as_deref(), symbol.as_deref()) {
                    (MemoryAnchorKind::Path, Some(path), None) => path,
                    (MemoryAnchorKind::Symbol, None, Some(symbol)) => symbol,
                    _ => {
                        invalid_anchors += 1;
                        continue;
                    }
                };
                // A missing canonical anchor or a substituted normalized value
                // is not merely drift. Never present it as guidance for code.
                if row.get(21).and_then(Value::as_str) != Some(key.0.as_str())
                    || memory_anchor_value_hash(kind, value) != key.2
                {
                    invalid_anchors += 1;
                    continue;
                }
                let freshness =
                    MemoryAnchorFreshnessState::parse(text(&row, 16)?).ok_or_else(error)?;
                // A private locator cannot become a redaction marker and
                // still be presented as a real file or symbol.
                if !projection::locator_is_public(value) {
                    denied.insert(key.0, "private_anchor");
                    continue;
                }
                let confidence = row
                    .get(20)
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                    .ok_or_else(error)? as f32;
                let candidate = RecallCandidateRow {
                    memory_id: key.0.clone(),
                    anchor_kind: kind,
                    normalized_path: path,
                    symbol,
                    freshness_state: freshness,
                    row_generation: row.get(17).and_then(Value::as_i64).ok_or_else(error)?,
                    level: text(&row, 18)?.to_owned(),
                    kind: text(&row, 19)?.to_owned(),
                    confidence,
                    // No content or provenance is fetched until after ranking.
                    content: String::new(),
                    tombstoned: false,
                    tags: Vec::new(),
                    provenance: Vec::new(),
                };
                surface_matches.insert(key.0.clone());
                if !query.kinds.is_empty() && !query.kinds.contains(&candidate.kind)
                    || !query.levels.is_empty() && !query.levels.contains(&candidate.level)
                {
                    continue;
                }
                best.entry(key.0)
                    .and_modify(|kept| {
                        if anchor_row_preference(&candidate) < anchor_row_preference(kept) {
                            *kept = candidate.clone();
                        }
                    })
                    .or_insert(candidate);
            }
        }
    }
    // A full last page need not imply truncation. Probe only metadata, never
    // claim a resumable source-tail cursor using a ranked budget offset.
    let mut incomplete = false;
    for stream in &streams {
        if !stream.done && !stream.page(db, workspace, 1)?.is_empty() {
            incomplete = true;
        }
    }
    let mut ranked = best
        .into_values()
        .filter(|row| {
            !query.stale_only || row.freshness_state != MemoryAnchorFreshnessState::Current
        })
        .map(|row| (score_row(&row, query.stale_anchor_penalty).score, row))
        .collect::<Vec<_>>();
    ranked.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .partial_cmp(a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.memory_id.cmp(&b.memory_id))
    });
    let overflow = ranked.len().saturating_sub(max_results);
    let filtered_empty = !surface_matches.is_empty() && ranked.is_empty();
    ranked.truncate(max_results);
    let mut rows = ranked.into_iter().map(|(_, row)| row).collect::<Vec<_>>();
    let mut redacted_memories = 0;
    for page in rows.chunks_mut(PAGE_SIZE) {
        let ids = page
            .iter()
            .map(|row| row.memory_id.as_str())
            .collect::<Vec<_>>();
        let sources = db.get_memories_batch(&ids).map_err(|_| error())?;
        let tags = db.get_memory_tags_batch(&ids).map_err(|_| error())?;
        for row in page {
            let source = sources.get(&row.memory_id).ok_or_else(error)?;
            if source.workspace_id != workspace {
                return Err(error());
            }
            let source_tags = tags.get(&row.memory_id).map_or(&[][..], Vec::as_slice);
            redacted_memories += usize::from(projection::apply(row, source, source_tags));
        }
    }
    let mut counts = BTreeMap::new();
    for reason in denied.into_values() {
        *counts.entry(reason).or_insert(0) += 1;
    }
    let mut degraded = super::admission::degradations(counts);
    if redacted_memories > 0 {
        degraded.push(RecallDegradation {
            code: "recall_egress_redacted",
            severity: "info",
            message: format!("Applied public-egress redaction to {redacted_memories} recalled memories before previews and token budgeting; private origins use an explicitly labeled memory identity, not fabricated source provenance."),
            repair: None,
        });
    }
    if filtered_empty {
        degraded.push(RecallDegradation {
            code: super::RECALL_FILTERED_EMPTY_CODE,
            severity: "info",
            message: format!("{} anchored memorie(s) matched the surface but kind/level/stale filters removed them all", surface_matches.len()),
            repair: None,
        });
    }
    if invalid_anchors > 0 {
        degraded.push(RecallDegradation { code: "recall_anchor_filtered", severity: "medium", message: format!("Withheld {invalid_anchors} reverse-index locators without matching canonical anchors; a stale index cannot invent code provenance."), repair: Some(super::ANCHOR_INDEX_REPAIR.to_owned()) });
    }
    if incomplete || overflow > 0 {
        degraded.push(RecallDegradation { code: "recall_scan_incomplete", severity: "medium", message: format!("Recall examined {scanned} anchor rows; source scan incomplete={incomplete}, ranked matches outside the retained candidate limit={overflow}. totalMatched and budget cursors describe only the retained admitted candidates, not exhaustive workspace recall."), repair: Some("Narrow --path, --symbol, --kind or --level and retry recall.".to_owned()) });
    }
    Ok(Scan { rows, degraded })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

    const WORKSPACE: &str = "wsp_00000000000000000000000701";
    const BASE: &str = "2026-01-01T00:00:00Z";

    fn fixture() -> DbConnection {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: "/recall-scan-test".to_owned(),
                name: None,
            },
        )
        .unwrap();
        db
    }

    fn seed(db: &DbConnection, number: u32, content: &str, confidence: f32) -> String {
        let id = format!("mem_{number:026}");
        db.insert_memory_with_timestamps(
            &id,
            &CreateMemoryInput {
                workspace_id: WORKSPACE.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("manual://code-anchors".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: vec!["scan-proof".to_owned()],
                valid_from: Some(BASE.to_owned()),
                valid_to: None,
            },
            BASE,
            BASE,
            &id,
        )
        .unwrap();
        id
    }

    fn scan(db: &DbConnection, query: &RecallQuery, source_rows: usize, results: usize) -> Scan {
        let snapshot = super::super::RecallReadSnapshot::begin_db(db).unwrap();
        let scan = load_bounded(
            db,
            WORKSPACE,
            query,
            DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            source_rows,
            results,
        )
        .unwrap();
        snapshot.finish_db().unwrap();
        scan
    }

    #[test]
    fn public_recall_finds_path_and_symbol_matches_beyond_a_full_nonmatching_prefix() {
        let db = fixture();
        // The old path reader consumes its complete 4096-row budget on one
        // memory's unrelated anchors, then the evaluator drops the symbol arm.
        db.with_transaction(|| {
            // Respect the durable 65536-character body bound while creating
            // more unrelated rows than the former global admission prefix.
            for (index, start) in (0..RECALL_CANDIDATE_SCAN_CAP + 3).step_by(1100).enumerate() {
                let end = (start + 1100).min(RECALL_CANDIDATE_SCAN_CAP + 3);
                let content = (start..end)
                    .map(|n| format!("anchor:path:src/other-{n:05}.rs"))
                    .collect::<Vec<_>>()
                    .join(" ");
                seed(&db, u32::try_from(index + 1).unwrap(), &content, 0.8);
            }
            seed(
                &db,
                9998,
                "Use the target implementation. anchor:path:src/wanted.rs",
                0.9,
            );
            seed(
                &db,
                9999,
                "Use the target entry point. anchor:symbol:Wanted::call",
                0.9,
            );
            Ok(())
        })
        .unwrap();
        let query = RecallQuery {
            paths: vec!["*wanted.rs".to_owned()],
            symbols: vec!["Wanted::call".to_owned()],
            ..RecallQuery::default()
        };
        let report = super::super::run_recall(&db, WORKSPACE, &query).unwrap();
        assert_eq!(report.items.len(), 2);
        assert_eq!(
            report
                .items
                .iter()
                .map(|row| row.memory_id.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([format!("mem_{:026}", 9998), format!("mem_{:026}", 9999)])
        );
        assert!(
            !report
                .degraded
                .iter()
                .any(|d| d.code == "recall_scan_incomplete")
        );
        assert!(
            report
                .items
                .iter()
                .all(|row| row.tags == ["scan-proof"] && !row.provenance.is_empty())
        );
    }

    #[test]
    fn obsolete_pages_do_not_consume_the_admitted_candidate_budget() {
        let db = fixture();
        db.with_transaction(|| {
            for number in 1..=PAGE_SIZE + 2 {
                seed(
                    &db,
                    number as u32,
                    "Former instruction. anchor:path:src/worker.rs",
                    0.9,
                );
            }
            db.execute_raw("UPDATE memories SET superseded_at = '2027-01-01T00:00:00Z'")?;
            seed(
                &db,
                9999,
                "Current instruction. anchor:path:src/worker.rs",
                0.6,
            );
            Ok(())
        })
        .unwrap();
        let result = scan(
            &db,
            &RecallQuery {
                paths: vec!["src/worker.rs".to_owned()],
                ..RecallQuery::default()
            },
            1024,
            1,
        );
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].memory_id, format!("mem_{:026}", 9999));
        assert!(result.degraded.iter().any(|d| d.code == "recall_source_filtered" && d.message.contains("superseded=258")));
        assert!(
            !result
                .degraded
                .iter()
                .any(|d| d.code == "recall_scan_incomplete")
        );
    }

    #[test]
    fn result_limit_keeps_the_highest_ranked_memories_not_the_first_ids() {
        let db = fixture();
        for number in 1..=8 {
            seed(
                &db,
                number,
                "Applicable guidance. anchor:path:src/all.rs",
                number as f32 / 10.0,
            );
        }
        let query = RecallQuery {
            paths: vec!["src/*".to_owned()],
            ..RecallQuery::default()
        };
        let result = scan(&db, &query, 100, 2);
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row.memory_id.clone())
                .collect::<Vec<_>>(),
            vec![format!("mem_{:026}", 8), format!("mem_{:026}", 7)]
        );
        assert!(
            result
                .degraded
                .iter()
                .any(|d| d.code == "recall_scan_incomplete"
                    && d.message.contains("candidate limit=6"))
        );
        assert_eq!(
            scan(&db, &query, 8, 8).degraded,
            Vec::new(),
            "an exactly full final page is not truncation"
        );
    }

    #[test]
    fn hard_scan_budget_is_explicit_and_does_not_starve_the_symbol_arm() {
        let db = fixture();
        for number in 1..=8 {
            seed(&db, number, "Generic guidance. anchor:path:src/all.rs", 0.1);
        }
        let symbol = seed(
            &db,
            9999,
            "Critical guidance. anchor:symbol:Last::call",
            0.99,
        );
        let result = scan(
            &db,
            &RecallQuery {
                paths: vec!["*".to_owned()],
                symbols: vec!["Last::call".to_owned()],
                ..RecallQuery::default()
            },
            4,
            1,
        );
        assert_eq!(result.rows[0].memory_id, symbol);
        assert!(
            result
                .degraded
                .iter()
                .any(|d| d.code == "recall_scan_incomplete"
                    && d.message.contains("source scan incomplete=true"))
        );
    }

    #[test]
    fn sql_narrowing_preserves_rust_glob_unicode_case_and_large_selector_semantics() {
        let db = fixture();
        let lower = seed(&db, 1, "Lowercase guidance. anchor:path:src/café.rs", 0.9);
        seed(&db, 2, "Uppercase guidance. anchor:path:Src/café.rs", 0.9);
        seed(&db, 3, "Other guidance. anchor:path:src/apple.rs", 0.9);
        let sym = seed(&db, 4, "Symbol guidance. anchor:symbol:Exact::call", 0.9);
        for pattern in ["src/caf*.rs", "src/[!a]*.rs", "./src/café.rs"] {
            let result = scan(
                &db,
                &RecallQuery {
                    paths: vec![pattern.to_owned()],
                    ..RecallQuery::default()
                },
                100,
                10,
            );
            assert_eq!(result.rows.len(), 1, "{pattern}");
            assert_eq!(result.rows[0].memory_id, lower);
        }
        let mut symbols = (0..SQL_SELECTOR_LIMIT + 2)
            .map(|n| format!("Unused{n}::call"))
            .collect::<Vec<_>>();
        symbols.push("Exact::call".to_owned());
        let result = scan(
            &db,
            &RecallQuery {
                symbols,
                ..RecallQuery::default()
            },
            100,
            10,
        );
        assert_eq!(result.rows[0].memory_id, sym);
        assert_eq!(result.rows.len(), 1);
        assert!(
            scan(
                &db,
                &RecallQuery {
                    paths: vec!["src/' OR 1=1 --".to_owned()],
                    ..RecallQuery::default()
                },
                100,
                10
            )
            .rows
            .is_empty()
        );
    }

    #[test]
    fn canonical_anchor_hash_prevents_index_locator_substitution() {
        let db = fixture();
        seed(
            &db,
            1,
            "Guidance for original code. anchor:path:src/original.rs",
            0.9,
        );
        db.execute_raw("UPDATE memory_anchor_index SET normalized_path = 'src/substituted.rs'")
            .unwrap();
        let result = scan(
            &db,
            &RecallQuery {
                paths: vec!["src/substituted.rs".to_owned()],
                ..RecallQuery::default()
            },
            100,
            10,
        );
        assert!(result.rows.is_empty());
        assert!(
            result
                .degraded
                .iter()
                .any(|d| d.code == "recall_anchor_filtered")
        );
        assert!(!format!("{:?}", result.degraded).contains("Guidance for original code"));
        // The original canonical anchor is retained but the index points to
        // another hash: a missing join cannot fall back to guessed freshness.
        db.execute_raw(&format!("UPDATE memory_anchor_index SET normalized_path = 'src/original.rs', anchor_value_hash = 'blake3:{}'", "f".repeat(64))).unwrap();
        let result = scan(
            &db,
            &RecallQuery {
                paths: vec!["src/original.rs".to_owned()],
                ..RecallQuery::default()
            },
            100,
            10,
        );
        assert!(result.rows.is_empty());
        assert!(
            result
                .degraded
                .iter()
                .any(|d| d.code == "recall_anchor_filtered")
        );
    }

    #[test]
    fn private_legacy_fields_are_redacted_before_public_rendering_and_budgeting() {
        let db = fixture();
        let id = seed(&db, 1, "Release guidance. anchor:path:src/release.rs", 0.9);
        let token = ["AKIA", "ABCDEFGHIJKLMNOP"].concat();
        let raw = format!(
            "{} /custom-private-root/session {token} anchor:path:src/release.rs",
            "Release guidance. ".repeat(100)
        );
        // Simulate durable legacy/configured-allow data, not a new ingress
        // bypass. Recall must neither publish it raw nor rewrite its source.
        db.execute_raw(&format!("UPDATE memories SET content = '{raw}', provenance_uri = 'file:///custom-private-root/notes.txt#L1' WHERE id = '{id}'")).unwrap();
        db.execute_raw(&format!(
            "UPDATE memory_tags SET tag = 'source-{token}' WHERE memory_id = '{id}'"
        ))
        .unwrap();
        let before = db.get_memory(&id).unwrap();
        let original_tags = db.get_memory_tags(&id).unwrap();
        let audits = db.count_table_rows("audit_log").unwrap();
        let query = RecallQuery {
            paths: vec!["src/release.rs".to_owned()],
            ..RecallQuery::default()
        };
        let report = super::super::run_recall(&db, WORKSPACE, &query).unwrap();
        assert_eq!(report.items.len(), 1);
        let item = &report.items[0];
        assert_eq!(item.memory_id, id);
        assert_eq!(item.provenance[0].uri, format!("ee-mem://{id}"));
        assert_eq!(item.provenance[0].source_type, "memory_identity");
        assert!(
            report
                .degraded
                .iter()
                .any(|d| d.code == "recall_egress_redacted")
        );
        let echo = super::super::RecallQueryEcho {
            paths: query.paths.clone(),
            ..Default::default()
        };
        for output in [
            super::super::recall_data_json(&report, &echo).to_string(),
            super::super::render_recall_markdown(&report, &[]),
            format!("{report:?}"),
        ] {
            assert!(!output.contains(&token));
            assert!(!output.contains("/custom-private-root"));
            assert!(output.contains("[REDACTED"));
        }
        let cost = super::super::recall_item_token_estimate(item);
        assert!(raw.split_whitespace().count() > cost);
        let budgeted = super::super::run_recall(
            &db,
            WORKSPACE,
            &RecallQuery {
                max_tokens: Some(u32::try_from(cost).unwrap()),
                ..query
            },
        )
        .unwrap();
        assert_eq!(budgeted.items, report.items);
        assert!(!budgeted.truncated);
        assert_eq!(db.get_memory(&id).unwrap(), before);
        assert_eq!(db.get_memory_tags(&id).unwrap(), original_tags);
        assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);
    }

    #[test]
    fn live_drift_is_a_flag_and_conjunctive_filters_remain_observable() {
        let db = fixture();
        seed(
            &db,
            1,
            "Guidance. anchor:path:src/current.rs anchor:path:src/stale.rs",
            0.9,
        );
        let hash = memory_anchor_value_hash(MemoryAnchorKind::Path, "src/stale.rs");
        db.execute_raw(&format!(
            "UPDATE memory_anchors SET freshness_state = 'stale' WHERE anchor_value_hash = '{hash}'"
        ))
        .unwrap();
        let stale = scan(
            &db,
            &RecallQuery {
                paths: vec!["src/stale.rs".to_owned()],
                ..RecallQuery::default()
            },
            100,
            10,
        );
        assert_eq!(stale.rows.len(), 1, "ordinary drift remains visible");
        assert_eq!(
            stale.rows[0].freshness_state,
            MemoryAnchorFreshnessState::Stale
        );
        for query in [
            RecallQuery {
                paths: vec!["src/*".to_owned()],
                stale_only: true,
                ..RecallQuery::default()
            },
            RecallQuery {
                paths: vec!["src/*".to_owned()],
                kinds: vec!["failure".to_owned()],
                ..RecallQuery::default()
            },
            RecallQuery {
                paths: vec!["src/*".to_owned()],
                levels: vec!["episodic".to_owned()],
                ..RecallQuery::default()
            },
        ] {
            let result = scan(&db, &query, 100, 10);
            assert!(result.rows.is_empty());
            assert!(
                result
                    .degraded
                    .iter()
                    .any(|d| d.code == super::super::RECALL_FILTERED_EMPTY_CODE)
            );
        }
    }
}
