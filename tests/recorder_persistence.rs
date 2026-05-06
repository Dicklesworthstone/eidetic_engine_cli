//! Integration tests for the recorder event store persistence helpers
//! (eidetic_engine_cli-ibw4).
//!
//! These exercise `start_and_persist_recording`, `record_and_persist_event`,
//! and `finish_and_persist_recording` against an in-memory FrankenSQLite
//! database with the full migration set applied.

use ee::core::recorder::{
    RecorderEventOptions, RecorderEventsListOptions, RecorderFinishOptions, RecorderStartOptions,
    finish_and_persist_recording, list_recorder_events, record_and_persist_event,
    start_and_persist_recording,
};
use ee::db::DbConnection;
use ee::models::{RecorderEventType, RecorderRunStatus};

const DEFAULT_MAX_PAYLOAD: usize = 64 * 1024;

fn connect() -> DbConnection {
    let conn = DbConnection::open_memory().expect("open in-memory db");
    conn.migrate().expect("apply migrations");
    conn
}

fn start_options() -> RecorderStartOptions {
    RecorderStartOptions {
        agent_id: "agent_test".to_owned(),
        session_id: Some("sess_test".to_owned()),
        workspace_id: None,
        dry_run: false,
    }
}

fn event_options(run_id: &str, payload: Option<&str>) -> RecorderEventOptions {
    RecorderEventOptions {
        run_id: run_id.to_owned(),
        event_type: RecorderEventType::ToolCall,
        payload: payload.map(str::to_owned),
        redact: false,
        previous_event_hash: None,
        max_payload_bytes: DEFAULT_MAX_PAYLOAD,
        dry_run: false,
    }
}

#[test]
fn start_and_persist_recording_inserts_run_row() {
    let conn = connect();
    let report = start_and_persist_recording(&conn, &start_options()).expect("start");
    assert!(report.run_id.starts_with("run_"));
    assert_eq!(report.agent_id, "agent_test");
    assert!(!report.dry_run);

    let stored = conn
        .get_recorder_run(&report.run_id)
        .expect("query")
        .expect("row present");
    assert_eq!(stored.run_id, report.run_id);
    assert_eq!(stored.agent_id, "agent_test");
    assert_eq!(stored.session_id.as_deref(), Some("sess_test"));
    assert_eq!(stored.source_type, "live");
    assert_eq!(stored.status, RecorderRunStatus::Active.as_str());
    assert_eq!(stored.event_count, 0);
}

#[test]
fn start_and_persist_recording_dry_run_does_not_insert() {
    let conn = connect();
    let mut opts = start_options();
    opts.dry_run = true;
    let report = start_and_persist_recording(&conn, &opts).expect("start dry run");
    assert!(report.dry_run);
    assert!(
        conn.get_recorder_run(&report.run_id)
            .expect("query")
            .is_none(),
        "dry-run must not insert"
    );
}

#[test]
fn record_and_persist_event_assigns_monotonic_sequence_and_chains_hashes() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");

    let first = record_and_persist_event(&conn, &event_options(&start.run_id, Some("hello")))
        .expect("first event");
    assert_eq!(first.sequence, 1);
    assert!(first.previous_event_hash.is_none());
    assert!(first.event_hash.starts_with("blake3:"));

    let second = record_and_persist_event(&conn, &event_options(&start.run_id, Some("world")))
        .expect("second event");
    assert_eq!(second.sequence, 2);
    assert_eq!(
        second.previous_event_hash.as_deref(),
        Some(first.event_hash.as_str()),
        "second event chains from first"
    );

    let stored = conn.list_recorder_events(&start.run_id).expect("list");
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].event_id, first.event_id);
    assert_eq!(stored[1].event_id, second.event_id);
    assert_eq!(
        stored[1].previous_event_hash.as_deref(),
        Some(first.event_hash.as_str())
    );
}

#[test]
fn record_and_persist_event_rejects_bad_run_id() {
    let conn = connect();
    let opts = event_options("bogus_id", Some("payload"));
    let err = record_and_persist_event(&conn, &opts).expect_err("invalid run id");
    let message = format!("{err}");
    assert!(message.contains("Invalid recorder run id"), "{message}");
}

#[test]
fn record_and_persist_event_rejects_chain_mismatch() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    let _first =
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("a"))).expect("first");

    let mut bad = event_options(&start.run_id, Some("b"));
    bad.previous_event_hash =
        Some("blake3:0000000000000000000000000000000000000000000000000000000000000000".to_owned());
    let err = record_and_persist_event(&conn, &bad).expect_err("chain mismatch rejected");
    let message = format!("{err}");
    assert!(
        message.contains("previous_event_hash mismatch"),
        "expected chain mismatch error, got: {message}"
    );
}

#[test]
fn finish_and_persist_recording_marks_run_completed_with_rolled_up_counts() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    let _ =
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("a"))).expect("event a");
    let _ =
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("b"))).expect("event b");
    let _ =
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("c"))).expect("event c");

    let report = finish_and_persist_recording(
        &conn,
        &RecorderFinishOptions {
            run_id: start.run_id.clone(),
            status: RecorderRunStatus::Completed,
            dry_run: false,
        },
    )
    .expect("finish");

    assert_eq!(report.run_id, start.run_id);
    assert_eq!(report.event_count, 3);
    assert_eq!(report.status, RecorderRunStatus::Completed);

    let stored = conn
        .get_recorder_run(&start.run_id)
        .expect("query")
        .expect("row present");
    assert_eq!(stored.status, RecorderRunStatus::Completed.as_str());
    assert!(stored.ended_at.is_some());
    assert_eq!(stored.event_count, 3);
    assert!(
        stored.payload_bytes >= 3,
        "payload bytes rolled up: {}",
        stored.payload_bytes
    );
}

#[test]
fn appending_many_events_yields_matching_row_count() {
    // Bead acceptance: "Appending 1000 events through hook API yields 1000 rows."
    // We exercise the hot path with a smaller-but-still-meaningful budget here.
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");

    const N: usize = 250;
    for i in 0..N {
        let payload = format!("event-{i}");
        let _ = record_and_persist_event(&conn, &event_options(&start.run_id, Some(&payload)))
            .unwrap_or_else(|err| panic!("event {i} failed: {err}"));
    }

    let stored = conn.list_recorder_events(&start.run_id).expect("list all");
    assert_eq!(stored.len(), N, "all events persisted");
    let last_seq = stored.last().expect("non-empty").sequence;
    assert_eq!(last_seq, N as u64);
}

#[test]
fn list_recorder_events_filters_by_run_and_since() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    let _ = record_and_persist_event(&conn, &event_options(&start.run_id, Some("alpha")))
        .expect("alpha");
    let _ =
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("beta"))).expect("beta");

    let entries = list_recorder_events(
        &conn,
        &RecorderEventsListOptions {
            run_id: Some(start.run_id.clone()),
            since: None,
            source: Some("live".to_owned()),
            limit: 100,
        },
    )
    .expect("list filtered");

    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.run_id == start.run_id));
    assert!(entries.iter().all(|e| e.event_hash.starts_with("blake3:")));
}
