//! Live source admission for code-anchored recall, before ranking or egress.
//!
//! An anchor is a locator, not lifecycle or workspace authority. Keep source,
//! seal, body, tags and cursor generation in the caller's one read snapshot.
//! No embedding model, index rebuild or durable write is needed for admission.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use super::RecallDegradation;
use crate::db::{DbConnection, DbError, DbOperation, StoredAnchorIndexCandidate};

const PAGE_SIZE: usize = 256;

fn malformed() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify anchored recall source authority; no partial result returned"
            .to_owned(),
    }
}

fn timestamp(value: Option<&Value>) -> Result<Option<DateTime<Utc>>, ()> {
    match value {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(raw)) => DateTime::parse_from_rfc3339(raw)
            .map(|at| Some(at.with_timezone(&Utc)))
            .map_err(|_| ()),
        _ => Err(()),
    }
}

/// The query projects a fixed binary-owned column layout. Invalid timestamps
/// withhold just their own memory; a failed read withholds the whole result.
fn denial(
    row: &sqlmodel_core::Row,
    id: &str,
    workspace: &str,
    at: DateTime<Utc>,
) -> Option<&'static str> {
    if row.get(1).and_then(Value::as_str) != Some(workspace) {
        return Some("workspace_mismatch");
    }
    if !matches!(row.get(7), Some(Value::Null)) {
        return Some("tombstoned");
    }
    match (row.get(8), row.get(9)) {
        (Some(Value::Null), Some(Value::Null)) => {}
        (Some(Value::Text(seal_id)), Some(Value::Text(_))) if seal_id == id => {
            if timestamp(row.get(9)).is_err() {
                return Some("malformed");
            }
        }
        (Some(Value::Text(seal_id)), Some(Value::Null)) if seal_id == id => {
            return Some("sealed");
        }
        _ => return Some("malformed"),
    }
    let (Ok(Some(created)), Ok(Some(updated)), Ok(start), Ok(end), Ok(superseded)) = (
        timestamp(row.get(2)),
        timestamp(row.get(3)),
        timestamp(row.get(4)),
        timestamp(row.get(5)),
        timestamp(row.get(6)),
    ) else {
        return Some("malformed");
    };
    if start.zip(end).is_some_and(|(start, end)| start > end) {
        return Some("malformed");
    }
    if superseded.is_some_and(|end| at >= end) {
        return Some("superseded");
    }
    if created > at || updated > at || start.is_some_and(|start| start > at) {
        return Some("future");
    }
    if end.is_some_and(|end| at > end) {
        return Some("expired");
    }
    None
}

pub(super) fn admit(
    db: &DbConnection,
    workspace: &str,
    candidates: Vec<StoredAnchorIndexCandidate>,
    at: DateTime<Utc>,
) -> crate::db::Result<(Vec<StoredAnchorIndexCandidate>, Vec<RecallDegradation>)> {
    let ids: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.memory_id.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut admitted = BTreeSet::new();
    let mut excluded = BTreeMap::<&str, usize>::new();
    for page in ids.chunks(PAGE_SIZE) {
        let placeholders = (1..=page.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let params = page
            .iter()
            .map(|id| Value::Text((*id).to_owned()))
            .collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        let sql = format!(
            "SELECT m.id, m.workspace_id, m.created_at, m.updated_at, m.valid_from, m.valid_to, m.superseded_at, m.tombstoned_at, s.memory_id, s.revealed_at FROM memories m LEFT JOIN memory_seals s ON s.memory_id = m.id WHERE m.id IN ({placeholders}) ORDER BY m.id ASC"
        );
        for row in db.query(&sql, &params).map_err(|_| malformed())? {
            let id = row.get(0).and_then(Value::as_str).ok_or_else(malformed)?;
            if !page.contains(&id) || !seen.insert(id.to_owned()) {
                return Err(malformed());
            }
            if let Some(reason) = denial(&row, id, workspace, at) {
                *excluded.entry(reason).or_default() += 1;
            } else {
                admitted.insert(id.to_owned());
            }
        }
        let missing = page.len() - seen.len();
        if missing > 0 {
            *excluded.entry("missing").or_default() += missing;
        }
    }
    let degraded = if excluded.is_empty() {
        Vec::new()
    } else {
        let counts = excluded
            .iter()
            .map(|(reason, count)| format!("{reason}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        vec![RecallDegradation {
            code: "recall_source_filtered",
            severity: if excluded.contains_key("malformed")
                || excluded.contains_key("missing")
                || excluded.contains_key("workspace_mismatch")
            {
                "medium"
            } else {
                "low"
            },
            message: format!(
                "Withheld anchored memories that are not current, visible sources in this workspace ({counts}); anchor freshness never overrides source authority."
            ),
            repair: None,
        }]
    };
    Ok((
        candidates
            .into_iter()
            .filter(|candidate| admitted.contains(&candidate.memory_id))
            .collect(),
        degraded,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::{RecallQuery, RecallReadSnapshot, run_recall, run_recall_in_snapshot};
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

    const WORKSPACE: &str = "wsp_00000000000000000000000601";
    const MEMORY: &str = "mem_00000000000000000000000601";
    const BASE: &str = "2026-01-01T00:00:00Z";
    const NOW: &str = "2030-01-01T00:00:00.500000000Z";
    const BODY: &str =
        "Release guidance. anchor:path:src/release.rs anchor:symbol:Release::publish";

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn seed(db: &DbConnection, workspace: &str, id: &str) {
        db.insert_memory_with_timestamps(
            id,
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: BODY.to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                provenance_uri: Some("manual://release-guidance".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: vec!["release".to_owned()],
                valid_from: Some(BASE.to_owned()),
                valid_to: None,
            },
            BASE,
            BASE,
            id,
        )
        .unwrap();
    }

    fn fixture() -> DbConnection {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: "/recall-authority-test".to_owned(),
                name: None,
            },
        )
        .unwrap();
        seed(&db, WORKSPACE, MEMORY);
        db
    }

    fn query() -> RecallQuery {
        RecallQuery {
            paths: vec!["src/**".to_owned()],
            symbols: vec!["Release::publish".to_owned()],
            ..RecallQuery::default()
        }
    }

    fn recall(db: &DbConnection) -> super::super::RecallReport {
        let snapshot = RecallReadSnapshot::begin_db(db).unwrap();
        let result = run_recall_in_snapshot(db, WORKSPACE, &query(), at(NOW)).unwrap();
        snapshot.finish_db().unwrap();
        result
    }

    fn set(db: &DbConnection, column: &str, raw: &str) {
        // Both column and raw are test-owned literals, never external input.
        db.execute_raw(&format!(
            "UPDATE memories SET {column} = '{raw}' WHERE id = '{MEMORY}'"
        ))
        .unwrap();
    }

    #[test]
    fn live_source_admission_checks_all_recall_selectors_without_an_index_rebuild() {
        let db = fixture();
        for request in [
            query(),
            RecallQuery {
                paths: vec!["src/release.rs".to_owned()],
                ..RecallQuery::default()
            },
            RecallQuery {
                symbols: vec!["Release::publish".to_owned()],
                ..RecallQuery::default()
            },
            RecallQuery {
                diff_paths: vec!["src/release.rs".to_owned()],
                ..RecallQuery::default()
            },
        ] {
            let before = run_recall_in_snapshot(&db, WORKSPACE, &request, at(NOW)).unwrap();
            assert_eq!(before.items.len(), 1, "{request:?}");
            assert_eq!(before.items[0].memory_id, MEMORY);
        }
        set(&db, "superseded_at", "2029-12-31T19:00:00.5-05:00");
        for request in [
            query(),
            RecallQuery {
                diff_paths: vec!["src/release.rs".to_owned()],
                ..RecallQuery::default()
            },
        ] {
            let after = run_recall_in_snapshot(&db, WORKSPACE, &request, at(NOW)).unwrap();
            assert!(after.items.is_empty());
            assert_eq!(after.total_matched, 0);
            assert!(after.continuation_cursor.is_none());
            assert!(
                after
                    .degraded
                    .iter()
                    .any(|d| d.code == "recall_source_filtered"
                        && d.message.contains("superseded=1"))
            );
        }
        // The stale locator remains present, proving the read boundary did the work.
        assert!(
            !db.query_anchor_index_path_candidates(WORKSPACE, None, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn live_validity_and_supersession_use_exact_inclusive_and_exclusive_endpoints() {
        let db = fixture();
        for (column, raw, visible) in [
            ("valid_from", "2030-01-01T00:00:00.500000000Z", true),
            ("valid_from", "2030-01-01T02:00:00.500000001+02:00", false),
            ("valid_from", "2030-01-01T00:00:00.499999999Z", true),
            ("valid_to", "2030-01-01T00:00:00.500000000Z", true),
            ("valid_to", "2029-12-31T19:00:00.499999999-05:00", false),
            ("superseded_at", "2030-01-01T00:00:00.500000001Z", true),
            ("superseded_at", "2030-01-01T00:00:00.500000000Z", false),
            ("created_at", "2030-01-01T00:00:00.500000001Z", false),
            ("updated_at", "2030-01-01T00:00:00.500000001Z", false),
        ] {
            db.execute_raw(&format!("UPDATE memories SET created_at = '{BASE}', updated_at = '{BASE}', valid_from = '{BASE}', valid_to = NULL, superseded_at = NULL WHERE id = '{MEMORY}'")).unwrap();
            set(&db, column, raw);
            assert_eq!(!recall(&db).items.is_empty(), visible, "{column} = {raw}");
        }
    }

    #[test]
    fn live_seal_not_placeholder_spelling_controls_disclosure() {
        let db = fixture();
        let commitment = format!("blake3:{}", "a".repeat(64));
        db.insert_memory_seal(MEMORY, &commitment, BASE).unwrap();
        let hidden = recall(&db);
        assert!(hidden.items.is_empty());
        assert!(
            hidden
                .degraded
                .iter()
                .any(|d| d.message.contains("sealed=1"))
        );
        assert!(!format!("{hidden:?}").contains(BODY));
        // Even unexpectedly retained plaintext cannot defeat a closed seal.
        assert_eq!(db.get_memory(MEMORY).unwrap().unwrap().content, BODY);
        db.mark_memory_seal_revealed(MEMORY, BASE).unwrap();
        assert_eq!(recall(&db).items.len(), 1);
    }

    #[test]
    fn foreign_live_ownership_is_not_authorized_by_a_stale_anchor_workspace() {
        let db = fixture();
        let other = "wsp_00000000000000000000000602";
        db.insert_workspace(
            other,
            &CreateWorkspaceInput {
                path: "/foreign-recall-test".to_owned(),
                name: None,
            },
        )
        .unwrap();
        set(&db, "workspace_id", other);
        assert!(
            !db.query_anchor_index_path_candidates(WORKSPACE, None, 10)
                .unwrap()
                .is_empty()
        );
        let report = recall(&db);
        assert!(report.items.is_empty());
        assert!(
            report
                .degraded
                .iter()
                .any(|d| d.message.contains("workspace_mismatch=1"))
        );
        assert!(!format!("{report:?}").contains(other));
        assert!(!format!("{report:?}").contains(MEMORY));
    }

    #[test]
    fn malformed_source_bounds_do_not_poison_other_memories_or_leak_raw_values() {
        let db = fixture();
        let public = "mem_00000000000000000000000602";
        seed(&db, WORKSPACE, public);
        for column in [
            "created_at",
            "updated_at",
            "valid_from",
            "valid_to",
            "superseded_at",
        ] {
            db.execute_raw(&format!("UPDATE memories SET created_at = '{BASE}', updated_at = '{BASE}', valid_from = '{BASE}', valid_to = NULL, superseded_at = NULL WHERE id = '{MEMORY}'")).unwrap();
            set(&db, column, "PRIVATE-BROKEN-TIMESTAMP");
            let report = recall(&db);
            assert_eq!(report.items.len(), 1);
            assert_eq!(report.items[0].memory_id, public);
            assert!(
                report
                    .degraded
                    .iter()
                    .any(|d| d.severity == "medium" && d.message.contains("malformed=1"))
            );
            assert!(!format!("{report:?}").contains("PRIVATE-BROKEN-TIMESTAMP"));
        }
    }

    #[test]
    fn direct_library_recall_is_read_only_and_preserves_a_callers_transaction() {
        let db = fixture();
        let before = db.get_memory(MEMORY).unwrap();
        let audits = db.count_table_rows("audit_log").unwrap();
        assert_eq!(run_recall(&db, WORKSPACE, &query()).unwrap().items.len(), 1);
        assert_eq!(db.get_memory(MEMORY).unwrap(), before);
        assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);
        db.begin_read_snapshot().unwrap();
        assert!(run_recall(&db, WORKSPACE, &query()).is_err());
        db.commit_read_snapshot()
            .expect("failed nested begin must not roll back the caller");
        assert!(run_recall(&db, WORKSPACE, &query()).is_ok());
    }

    #[test]
    fn failed_authority_read_returns_no_partial_report_and_releases_owned_snapshot() {
        let db = fixture();
        db.execute_raw("ALTER TABLE memory_seals RENAME TO unavailable_memory_seals")
            .unwrap();
        let error = run_recall(&db, WORKSPACE, &query()).unwrap_err();
        assert!(error.to_string().contains("no partial result"));
        assert!(!error.to_string().contains(BODY));
        db.begin_read_snapshot()
            .expect("no leaked source read snapshot");
        db.rollback_read_snapshot().unwrap();
    }

    #[test]
    fn concurrent_seal_and_revision_only_change_the_next_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("recall.db");
        let writer = DbConnection::open_file(&database).unwrap();
        writer.migrate().unwrap();
        writer
            .insert_workspace(
                WORKSPACE,
                &CreateWorkspaceInput {
                    path: root.path().to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        seed(&writer, WORKSPACE, MEMORY);
        let reader = DbConnection::open_file_read_only(&database).unwrap();
        let snapshot = RecallReadSnapshot::begin_db(&reader).unwrap();
        assert_eq!(
            run_recall_in_snapshot(&reader, WORKSPACE, &query(), at(NOW))
                .unwrap()
                .items
                .len(),
            1
        );
        writer
            .insert_memory_seal(MEMORY, &format!("blake3:{}", "b".repeat(64)), BASE)
            .unwrap();
        set(&writer, "superseded_at", BASE);
        assert_eq!(
            run_recall_in_snapshot(&reader, WORKSPACE, &query(), at(NOW))
                .unwrap()
                .items
                .len(),
            1
        );
        snapshot.finish_db().unwrap();
        assert!(
            run_recall(&reader, WORKSPACE, &query())
                .unwrap()
                .items
                .is_empty()
        );
    }
}
