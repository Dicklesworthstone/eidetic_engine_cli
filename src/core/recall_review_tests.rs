//! Live pending review cannot be bypassed by code-anchored retrieval.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::recall::{RecallReadSnapshot, run_recall, run_recall_in_snapshot};
use crate::db::{CreateFeedbackQuarantineInput, CreateMemoryInput, CreateWorkspaceInput};

const WORKSPACE: &str = "wsp_00000000000000000000000921";
const OTHER: &str = "wsp_00000000000000000000000922";
const BASE: &str = "2026-01-01T00:00:00Z";
const BODY: &str = "Reviewed guidance. anchor:path:src/release.rs anchor:symbol:Release::publish";

fn at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn initialize(db: &DbConnection) {
    db.migrate().unwrap();
    for id in [WORKSPACE, OTHER] {
        db.insert_workspace(
            id,
            &CreateWorkspaceInput {
                path: format!("/recall-review/{id}"),
                name: None,
            },
        )
        .unwrap();
    }
}

fn fixture() -> DbConnection {
    let db = DbConnection::open_memory().unwrap();
    initialize(&db);
    db
}

fn seed(db: &DbConnection, number: u32, confidence: f32) -> String {
    let id = format!("mem_{number:026}");
    db.insert_memory_with_timestamps(
        &id,
        &CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: BODY.to_owned(),
            workflow_id: None,
            confidence,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some("manual://reviewed-release".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: vec!["release".to_owned()],
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

fn hold(db: &DbConnection, number: u32, workspace: &str, kind: &str, target: &str) -> String {
    let id = format!("fq_{number:026}");
    db.insert_feedback_quarantine(
        &id,
        &CreateFeedbackQuarantineInput {
            workspace_id: workspace.to_owned(),
            source_id: "PRIVATE-RECALL-REVIEW-SOURCE".to_owned(),
            target_type: kind.to_owned(),
            target_id: target.to_owned(),
            signal: "harmful".to_owned(),
            weight: 1.0,
            source_type: "outcome_observed".to_owned(),
            proposed_event_id: None,
            recorded_at: BASE.to_owned(),
            reason: "PRIVATE-RECALL-REVIEW-REASON".to_owned(),
            event_reason: None,
            evidence_json: None,
            session_id: None,
            raw_event_hash: format!("blake3:{}", "a".repeat(64)),
        },
    )
    .unwrap();
    id
}

fn review(db: &DbConnection, id: &str, status: &str) {
    assert!(
        db.update_feedback_quarantine_status(id, status, Some("operator"), None)
            .unwrap()
    );
}

fn query() -> RecallQuery {
    RecallQuery {
        paths: vec!["src/*".to_owned()],
        symbols: vec!["Release::publish".to_owned()],
        ..RecallQuery::default()
    }
}

#[test]
fn pending_review_precedes_every_selector_preview_count_and_cursor() {
    let db = fixture();
    let memory = seed(&db, 1, 0.9);
    assert_eq!(run_recall(&db, WORKSPACE, &query()).unwrap().items.len(), 1);
    hold(&db, 1, WORKSPACE, "memory", &memory);
    // Neither a current nor a deliberately requested stale anchor overrides
    // source review. Keep the underlying anchor inventory unchanged by reads.
    db.execute_raw("UPDATE memory_anchors SET freshness_state = 'stale'")
        .unwrap();
    let before = db.get_memory(&memory).unwrap();
    let tags = db.get_memory_tags(&memory).unwrap();
    let anchors = db.count_table_rows("memory_anchors").unwrap();
    let generation = db.get_workspace_generation(WORKSPACE).unwrap();
    let audits = db.count_table_rows("audit_log").unwrap();
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
        RecallQuery {
            stale_only: true,
            kinds: vec!["rule".to_owned()],
            levels: vec!["procedural".to_owned()],
            max_tokens: Some(1),
            ..query()
        },
    ] {
        let report = run_recall(&db, WORKSPACE, &request).unwrap();
        assert!(report.items.is_empty());
        assert_eq!(report.total_matched, 0);
        assert_eq!(report.dropped_count, 0);
        assert!(!report.truncated && report.continuation_cursor.is_none());
        assert!(report.degraded.iter().any(|entry| {
            entry.code == "recall_source_filtered" && entry.message.contains("pending_review=1")
        }));
        assert!(!report.degraded.iter().any(|entry| {
            entry.code == "recall_egress_redacted"
                || entry.code == crate::core::recall::RECALL_FILTERED_EMPTY_CODE
        }));
        let output = format!("{report:?}");
        for private in [memory.as_str(), BODY, "PRIVATE-RECALL-REVIEW"] {
            assert!(!output.contains(private));
        }
    }
    assert_eq!(db.get_memory(&memory).unwrap(), before);
    assert_eq!(db.get_memory_tags(&memory).unwrap(), tags);
    assert_eq!(db.count_table_rows("memory_anchors").unwrap(), anchors);
    assert_eq!(db.get_workspace_generation(WORKSPACE).unwrap(), generation);
    assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);
}

#[test]
fn held_high_ranked_pages_cannot_starve_a_later_admitted_memory() {
    let db = fixture();
    let mut live = String::new();
    db.with_transaction(|| {
        for number in 1..=513 {
            let id = seed(&db, number, 1.0);
            hold(&db, number, WORKSPACE, "memory", &id);
        }
        live = seed(&db, 9999, 0.2);
        Ok(())
    })
    .unwrap();
    let snapshot = RecallReadSnapshot::begin_db(&db).unwrap();
    let result = load_bounded(&db, WORKSPACE, &query(), at(), 2048, 1).unwrap();
    snapshot.finish_db().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].memory_id, live);
    assert!(result.degraded.iter().any(|entry| {
        entry.code == "recall_source_filtered" && entry.message.contains("pending_review=513")
    }));
    assert!(
        !result
            .degraded
            .iter()
            .any(|entry| entry.code == "recall_scan_incomplete")
    );
    // Every held memory has both a path and symbol, but it is counted once.
    assert_eq!(db.count_table_rows("memory_anchor_index").unwrap(), 1028);
}

#[test]
fn exact_native_review_ownership_and_all_pending_events_control_readmission() {
    let db = fixture();
    let memory = seed(&db, 2, 0.9);
    hold(&db, 1, OTHER, "memory", &memory);
    hold(&db, 2, WORKSPACE, "rule", &memory);
    assert_eq!(run_recall(&db, WORKSPACE, &query()).unwrap().items.len(), 1);
    let first = hold(&db, 3, WORKSPACE, "memory", &memory);
    let second = hold(&db, 4, WORKSPACE, "memory", &memory);
    assert!(
        run_recall(&db, WORKSPACE, &query())
            .unwrap()
            .items
            .is_empty()
    );
    review(&db, &first, "released");
    assert!(
        run_recall(&db, WORKSPACE, &query())
            .unwrap()
            .items
            .is_empty()
    );
    db.insert_memory_seal(&memory, &format!("blake3:{}", "b".repeat(64)), BASE)
        .unwrap();
    review(&db, &second, "rejected");
    let sealed = run_recall(&db, WORKSPACE, &query()).unwrap();
    assert!(sealed.items.is_empty());
    assert!(
        sealed
            .degraded
            .iter()
            .any(|entry| entry.message.contains("sealed=1"))
    );
    assert!(db.mark_memory_seal_revealed(&memory, BASE).unwrap());
    let restored = run_recall(&db, WORKSPACE, &query()).unwrap();
    assert_eq!(restored.items[0].memory_id, memory);
    assert!(
        !restored
            .degraded
            .iter()
            .any(|entry| entry.message.contains("pending_review"))
    );
}

#[test]
fn concurrent_hold_and_release_never_split_a_borrowed_recall_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("recall.db");
    let writer = DbConnection::open_file(&path).unwrap();
    initialize(&writer);
    let memory = seed(&writer, 3, 0.9);
    let reader = DbConnection::open_file_read_only(&path).unwrap();
    let snapshot = RecallReadSnapshot::begin_db(&reader).unwrap();
    let first = run_recall_in_snapshot(&reader, WORKSPACE, &query(), at()).unwrap();
    let hold_id = hold(&writer, 1, WORKSPACE, "memory", &memory);
    assert_eq!(
        run_recall_in_snapshot(&reader, WORKSPACE, &query(), at()).unwrap(),
        first
    );
    snapshot.finish_db().unwrap();
    let snapshot = RecallReadSnapshot::begin_db(&reader).unwrap();
    let held = run_recall_in_snapshot(&reader, WORKSPACE, &query(), at()).unwrap();
    assert!(held.items.is_empty());
    review(&writer, &hold_id, "released");
    assert_eq!(
        run_recall_in_snapshot(&reader, WORKSPACE, &query(), at()).unwrap(),
        held
    );
    snapshot.finish_db().unwrap();
    assert_eq!(
        run_recall(&reader, WORKSPACE, &query()).unwrap().items[0].memory_id,
        memory
    );
    assert!(!root.path().join("index").exists());
}

#[test]
fn unreadable_review_authority_withholds_the_result_and_preserves_snapshot_ownership() {
    let db = fixture();
    seed(&db, 4, 0.9);
    db.execute_raw("ALTER TABLE feedback_quarantine RENAME TO private_unavailable_review")
        .unwrap();
    let error = run_recall(&db, WORKSPACE, &query()).unwrap_err();
    let diagnostic = format!("{error:?}");
    for private in [
        "private_unavailable_review",
        "feedback_quarantine",
        "SELECT",
        BODY,
    ] {
        assert!(!diagnostic.contains(private));
    }
    db.begin_read_snapshot().unwrap();
    assert!(run_recall_in_snapshot(&db, WORKSPACE, &query(), at()).is_err());
    assert!(
        db.begin_read_snapshot().is_err(),
        "borrowed snapshot must remain owned"
    );
    db.rollback_read_snapshot().unwrap();
    // A later repair is observed without rebuilding or touching source rows.
    db.execute_raw("ALTER TABLE private_unavailable_review RENAME TO feedback_quarantine")
        .unwrap();
    assert_eq!(run_recall(&db, WORKSPACE, &query()).unwrap().items.len(), 1);
}

#[test]
fn a_source_scan_bound_stays_explicit_even_when_the_prefix_is_held() {
    let db = fixture();
    for number in 1..=5 {
        let memory = seed(&db, number, 0.9);
        hold(&db, number, WORKSPACE, "memory", &memory);
    }
    seed(&db, 9999, 0.2);
    let snapshot = RecallReadSnapshot::begin_db(&db).unwrap();
    let result = load_bounded(&db, WORKSPACE, &query(), at(), 4, 1).unwrap();
    snapshot.finish_db().unwrap();
    assert!(result.rows.is_empty());
    assert!(result.degraded.iter().any(|entry| {
        entry.code == "recall_scan_incomplete"
            && entry.message.contains("source scan incomplete=true")
    }));
    assert!(
        !result
            .degraded
            .iter()
            .any(|entry| { entry.code == crate::core::recall::RECALL_FILTERED_EMPTY_CODE })
    );
}
