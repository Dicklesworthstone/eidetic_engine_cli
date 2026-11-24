//! Integration tests for the recorder event store persistence helpers
//! (eidetic_engine_cli-ibw4).
//!
//! These exercise `start_and_persist_recording`, `record_and_persist_event`,
//! and `finish_and_persist_recording` against an in-memory FrankenSQLite
//! database with the full migration set applied.

// These persistence tests use unwrap/expect as direct assertions on fixed fixtures.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use ee::core::recorder::{
    RecordPersistedEventError, RecorderEventOptions, RecorderEventsListOptions,
    RecorderFinishOptions, RecorderStartOptions,
    finish_and_persist_recording, list_recorder_events, record_and_persist_event,
    start_and_persist_recording,
};
use ee::db::{CreateRecorderEventInput, CreateRecorderRunInput, DbConnection};
use ee::models::{RecorderEventType, RecorderRunStatus};
use std::collections::HashSet;
use std::sync::{Arc, Barrier};
use std::thread;

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

fn stored_event_input(run_id: &str, sequence: u64) -> CreateRecorderEventInput {
    CreateRecorderEventInput {
        run_id: run_id.to_owned(),
        sequence,
        event_type: "state_change".to_owned(),
        timestamp: "2026-01-01T00:00:00Z".to_owned(),
        payload_hash: None,
        payload_bytes: 0,
        redaction_status: "clean".to_owned(),
        redacted_bytes: 0,
        previous_event_hash: (sequence > 1).then(|| format!("blake3:fixture-{}", sequence - 1)),
        event_hash: format!("blake3:fixture-{sequence}"),
        chain_status: if sequence == 1 { "root" } else { "linked" }.to_owned(),
        source_span_id: None,
        source_line_start: None,
        source_line_end: None,
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
fn recorder_store_list_empty_store_returns_empty() {
    let conn = connect();
    let entries = list_recorder_events(
        &conn,
        &RecorderEventsListOptions {
            limit: 100,
            ..RecorderEventsListOptions::default()
        },
    )
    .expect("list empty store");
    assert!(entries.is_empty());
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
fn record_and_persist_event_serializes_parallel_writers_for_one_run() {
    const WRITER_COUNT: usize = 12;

    let dir = tempfile::tempdir().expect("temp db dir");
    let db_path = dir.path().join("recorder-parallel.db");
    let conn = DbConnection::open_file(db_path.clone()).expect("open file db");
    conn.migrate().expect("apply migrations");
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");

    let barrier = Arc::new(Barrier::new(WRITER_COUNT));
    let mut handles = Vec::with_capacity(WRITER_COUNT);
    for writer_index in 0..WRITER_COUNT {
        let db_path = db_path.clone();
        let run_id = start.run_id.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let conn = DbConnection::open_file(db_path)
                .unwrap_or_else(|error| panic!("writer {writer_index} open db: {error}"));
            barrier.wait();
            let payload = format!("parallel-event-{writer_index}");
            record_and_persist_event(&conn, &event_options(&run_id, Some(&payload)))
                .map_err(|error| format!("writer {writer_index} failed: {error}"))
        }));
    }

    let mut reports = Vec::with_capacity(WRITER_COUNT);
    for handle in handles {
        reports.push(
            handle
                .join()
                .expect("writer thread joins")
                .expect("writer records event without UNIQUE race"),
        );
    }

    let unique_sequences: HashSet<u64> = reports.iter().map(|report| report.sequence).collect();
    assert_eq!(
        unique_sequences.len(),
        WRITER_COUNT,
        "parallel writers must receive distinct sequences"
    );

    let stored = conn.list_recorder_events(&start.run_id).expect("list");
    assert_eq!(stored.len(), WRITER_COUNT, "all parallel events persisted");
    for (index, event) in stored.iter().enumerate() {
        let expected_sequence = u64::try_from(index + 1).expect("test sequence fits in u64");
        assert_eq!(event.sequence, expected_sequence);
        if index == 0 {
            assert!(
                event.previous_event_hash.is_none(),
                "first event starts the chain"
            );
        } else {
            assert_eq!(
                event.previous_event_hash.as_deref(),
                Some(stored[index - 1].event_hash.as_str()),
                "event {} chains from previous row",
                event.sequence
            );
        }
    }
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
fn record_and_persist_event_rejects_predecessor_for_empty_chain() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    let mut options = event_options(&start.run_id, Some("first"));
    options.previous_event_hash = Some("blake3:unexpected-predecessor".to_owned());
    assert!(matches!(
        record_and_persist_event(&conn, &options),
        Err(RecordPersistedEventError::ChainMismatch { .. })
    ));
    assert!(
        conn.list_recorder_events(&start.run_id)
            .expect("events")
            .is_empty()
    );
}

#[test]
fn recorder_terminal_runs_refuse_core_and_direct_database_appends() {
    let conn = connect();
    for status in ["completed", "abandoned", "imported"] {
        let run_id = format!("run_terminal_{status}");
        conn.insert_recorder_run(
            &run_id,
            &CreateRecorderRunInput {
                workspace_id: None,
                agent_id: "agent_terminal".to_owned(),
                session_id: None,
                source_type: if status == "imported" { "cass" } else { "live" }.to_owned(),
                source_id: None,
                status: status.to_owned(),
                started_at: "2026-01-01T00:00:00Z".to_owned(),
                ended_at: Some("2026-01-01T00:01:00Z".to_owned()),
                event_count: 0,
                redacted_count: 0,
                payload_bytes: 0,
                chain_complete: true,
            },
        )
        .expect("terminal fixture");
        let before = conn.get_recorder_run(&run_id).expect("run");
        assert!(matches!(
            record_and_persist_event(&conn, &event_options(&run_id, Some("late"))),
            Err(RecordPersistedEventError::RunNotActive { .. })
        ));
        assert!(
            conn.insert_recorder_event(
                &format!("evt_terminal_{status}"),
                &stored_event_input(&run_id, 1),
            )
            .is_err(),
            "direct writes must also refuse {status} runs"
        );
        assert_eq!(conn.get_recorder_run(&run_id).expect("unchanged run"), before);
        assert!(conn.list_recorder_events(&run_id).expect("events").is_empty());
    }
}

#[test]
fn recorder_append_refuses_a_gap_before_the_tail() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    for sequence in [1, 3] {
        conn.insert_recorder_event(
            &format!("evt_gap_{sequence}"),
            &stored_event_input(&start.run_id, sequence),
        )
        .expect("sparse history fixture");
    }
    let error = record_and_persist_event(&conn, &event_options(&start.run_id, Some("next")))
        .expect_err("a tail lookup must not hide a gap");
    assert!(error.to_string().contains("non-contiguous"), "{error}");
    assert_eq!(
        conn.list_recorder_events(&start.run_id)
            .expect("events")
            .len(),
        2
    );
}

#[test]
fn recorder_append_refuses_malformed_earlier_rows_without_hydrating_them() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    record_and_persist_event(&conn, &event_options(&start.run_id, Some("first"))).expect("first");
    let last = record_and_persist_event(&conn, &event_options(&start.run_id, Some("second")))
        .expect("second");
    // The isolated fixture models damaged historical metadata. The source-line
    // CHECK accepts this positive integer, but the existing u32 decoder rejects it.
    conn.execute_raw("DROP TRIGGER recorder_events_no_update")
        .expect("fixture trigger");
    conn.execute_raw("UPDATE recorder_events SET source_line_start = 4294967296 WHERE sequence = 1")
        .expect("corrupt earlier metadata");
    assert!(
        conn.list_recorder_events(&start.run_id).is_err(),
        "old full-row reader rejects it"
    );
    let error = record_and_persist_event(&conn, &event_options(&start.run_id, Some("third")))
        .expect_err("aggregate guard must retain the decoder's rejection");
    assert!(error.to_string().contains("malformed event rows"), "{error}");
    let tail = conn
        .list_recorder_events_filtered(Some(&start.run_id), None, None, 1)
        .expect("last row remains valid");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].event_id, last.event_id, "no later event was inserted");
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
fn recorder_finish_replay_preserves_timestamp_and_terminal_identity() {
    let conn = connect();
    for status in [RecorderRunStatus::Completed, RecorderRunStatus::Abandoned] {
        let start = start_and_persist_recording(&conn, &start_options()).expect("start");
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("recorded")))
            .expect("event");
        let options = RecorderFinishOptions {
            run_id: start.run_id.clone(),
            status,
            dry_run: false,
        };
        let first = finish_and_persist_recording(&conn, &options).expect("finish");
        let before = conn.get_recorder_run(&start.run_id).expect("terminal row");
        let replay = finish_and_persist_recording(&conn, &options).expect("idempotent finish");
        assert_eq!(replay.data_json(), first.data_json());
        assert_eq!(
            conn.get_recorder_run(&start.run_id).expect("unchanged row"),
            before
        );

        for requested in [
            RecorderRunStatus::Active,
            RecorderRunStatus::Imported,
            if status == RecorderRunStatus::Completed {
                RecorderRunStatus::Abandoned
            } else {
                RecorderRunStatus::Completed
            },
        ] {
            let attempt = RecorderFinishOptions {
                status: requested,
                ..options.clone()
            };
            assert!(
                finish_and_persist_recording(&conn, &attempt).is_err(),
                "cannot relabel {status} as {requested}"
            );
            assert_eq!(
                conn.get_recorder_run(&start.run_id)
                    .expect("unchanged terminal"),
                before
            );
        }
    }
}

#[test]
fn recorder_finish_validates_status_and_dry_run_keeps_run_open() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    record_and_persist_event(&conn, &event_options(&start.run_id, Some("first"))).expect("event");
    let before = conn.get_recorder_run(&start.run_id).expect("active row");
    for status in [RecorderRunStatus::Active, RecorderRunStatus::Imported] {
        assert!(
            finish_and_persist_recording(
                &conn,
                &RecorderFinishOptions {
                    run_id: start.run_id.clone(),
                    status,
                    dry_run: false,
                },
            )
            .is_err()
        );
    }
    let preview = finish_and_persist_recording(
        &conn,
        &RecorderFinishOptions {
            run_id: start.run_id.clone(),
            status: RecorderRunStatus::Completed,
            dry_run: true,
        },
    )
    .expect("preview");
    assert!(preview.dry_run);
    assert_eq!(preview.event_count, 1);
    assert_eq!(
        conn.get_recorder_run(&start.run_id).expect("still open"),
        before
    );
    assert_eq!(
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("second")))
            .expect("append after preview")
            .sequence,
        2
    );
}

#[test]
fn recorder_finish_preview_works_on_read_only_connection_and_releases_snapshot() {
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("recorder-preview.db");
    let writer = DbConnection::open_file(&database).expect("database");
    writer.migrate().expect("schema");
    let start = start_and_persist_recording(&writer, &start_options()).expect("start");
    record_and_persist_event(&writer, &event_options(&start.run_id, Some("first")))
        .expect("event");
    let before = writer.get_recorder_run(&start.run_id).expect("active row");
    let reader = DbConnection::open_file_read_only(&database).expect("read-only database");
    let options = RecorderFinishOptions {
        run_id: start.run_id.clone(),
        status: RecorderRunStatus::Completed,
        dry_run: true,
    };
    let preview = finish_and_persist_recording(&reader, &options).expect("read-only preview");
    assert!(preview.dry_run);
    assert_eq!(preview.event_count, 1);
    assert_eq!(
        writer.get_recorder_run(&start.run_id).expect("unchanged"),
        before
    );

    record_and_persist_event(&writer, &event_options(&start.run_id, Some("second")))
        .expect("writer continues after preview");
    let second = finish_and_persist_recording(&reader, &options).expect("new preview snapshot");
    assert_eq!(second.event_count, 2, "the prior read snapshot was released");
}

#[test]
fn recorder_finish_preview_preserves_a_caller_owned_transaction() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    let result: ee::db::Result<()> = conn.with_transaction(|| {
        conn.insert_recorder_event(
            "evt_preview_uncommitted",
            &stored_event_input(&start.run_id, 1),
        )?;
        let preview = finish_and_persist_recording(
            &conn,
            &RecorderFinishOptions {
                run_id: start.run_id.clone(),
                status: RecorderRunStatus::Completed,
                dry_run: true,
            },
        )
        .expect("preview inside the caller's snapshot");
        assert_eq!(preview.event_count, 1);
        assert_eq!(
            conn.list_recorder_events(&start.run_id)?.len(),
            1,
            "preview must not roll back its caller's pending event"
        );
        Err(ee::db::DbError::MalformedRow {
            operation: ee::db::DbOperation::Execute,
            message: "deliberate caller rollback".to_owned(),
        })
    });
    assert!(result.is_err());
    assert!(
        conn.list_recorder_events(&start.run_id)
            .expect("after rollback")
            .is_empty(),
        "preview must not commit its caller's pending event"
    );
}

#[test]
fn recorder_recovered_abandoned_run_keeps_missing_end_timestamp() {
    let conn = connect();
    let run_id = "run_recovered_abandoned";
    conn.insert_recorder_run(
        run_id,
        &CreateRecorderRunInput {
            workspace_id: None,
            agent_id: "agent_recovered".to_owned(),
            session_id: None,
            source_type: "live".to_owned(),
            source_id: None,
            status: "abandoned".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            ended_at: None,
            event_count: 0,
            redacted_count: 0,
            payload_bytes: 0,
            chain_complete: true,
        },
    )
    .expect("interrupted run restored as abandoned");
    let before = conn.get_recorder_run(run_id).expect("history");
    let error = finish_and_persist_recording(
        &conn,
        &RecorderFinishOptions {
            run_id: run_id.to_owned(),
            status: RecorderRunStatus::Abandoned,
            dry_run: false,
        },
    )
    .expect_err("recovery did not attest a live finish timestamp");
    assert!(matches!(
        error,
        ee::models::DomainError::UsageCodeWithDetails {
            code: "recorder_run_already_finished",
            ..
        }
    ));
    assert_eq!(
        conn.get_recorder_run(run_id).expect("unchanged history"),
        before
    );
}

#[test]
fn recorder_finish_and_parallel_appends_share_one_terminal_snapshot() {
    const WRITERS: usize = 4;
    let directory = tempfile::tempdir().expect("directory");
    let database = directory.path().join("recorder-finish-race.db");
    let conn = DbConnection::open_file(&database).expect("database");
    conn.migrate().expect("schema");
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    record_and_persist_event(&conn, &event_options(&start.run_id, Some("initial")))
        .expect("initial event");
    let barrier = Arc::new(Barrier::new(WRITERS + 1));
    let mut writers = Vec::new();
    for index in 0..WRITERS {
        let path = database.clone();
        let run_id = start.run_id.clone();
        let barrier = Arc::clone(&barrier);
        writers.push(thread::spawn(move || {
            let writer = DbConnection::open_file(path).expect("writer");
            barrier.wait();
            record_and_persist_event(
                &writer,
                &event_options(&run_id, Some(&format!("event-{index}"))),
            )
        }));
    }
    barrier.wait();
    let finished = finish_and_persist_recording(
        &conn,
        &RecorderFinishOptions {
            run_id: start.run_id.clone(),
            status: RecorderRunStatus::Completed,
            dry_run: false,
        },
    )
    .expect("finish while appends contend");
    let mut accepted = 1_u64;
    for writer in writers {
        match writer.join().expect("writer joins") {
            Ok(_) => accepted += 1,
            Err(RecordPersistedEventError::RunNotActive { .. }) => {}
            Err(error) => panic!("unexpected append failure: {error}"),
        }
    }
    let rows = conn.list_recorder_events(&start.run_id).expect("final events");
    let stored = conn
        .get_recorder_run(&start.run_id)
        .expect("run")
        .expect("present");
    assert_eq!(u64::try_from(rows.len()).expect("bounded fixture"), accepted);
    assert_eq!(finished.event_count, accepted);
    assert_eq!(stored.event_count, accepted);
    assert_eq!(
        stored.payload_bytes,
        rows.iter().map(|row| row.payload_bytes).sum::<u64>()
    );
    assert_eq!(stored.ended_at.as_deref(), Some(finished.ended_at.as_str()));
    assert!(matches!(
        record_and_persist_event(&conn, &event_options(&start.run_id, Some("too late"))),
        Err(RecordPersistedEventError::RunNotActive { .. })
    ));
}

#[test]
fn recorder_store_appending_many_events_yields_matching_row_count() {
    // Bead acceptance: "Appending 1000 events through hook API yields 1000 rows."
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");

    const N: usize = 1000;
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
fn recorder_store_list_filters_across_multiple_sources() {
    let conn = connect();
    let live = start_and_persist_recording(&conn, &start_options()).expect("live start");
    let _ = record_and_persist_event(&conn, &event_options(&live.run_id, Some("live event")))
        .expect("live event");

    conn.insert_recorder_run(
        "run_synthetic_001",
        &CreateRecorderRunInput {
            workspace_id: None,
            agent_id: "agent_synthetic".to_owned(),
            session_id: None,
            source_type: "synthetic".to_owned(),
            source_id: Some("fixture://synthetic".to_owned()),
            status: "active".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            ended_at: None,
            event_count: 1,
            redacted_count: 0,
            payload_bytes: 0,
            chain_complete: true,
        },
    )
    .expect("insert synthetic run");
    conn.insert_recorder_event(
        "evt_synthetic_001",
        &CreateRecorderEventInput {
            run_id: "run_synthetic_001".to_owned(),
            sequence: 1,
            event_type: "state_change".to_owned(),
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            payload_hash: None,
            payload_bytes: 0,
            redaction_status: "clean".to_owned(),
            redacted_bytes: 0,
            previous_event_hash: None,
            event_hash: "blake3:syntheticeventhash".to_owned(),
            chain_status: "root".to_owned(),
            source_span_id: None,
            source_line_start: None,
            source_line_end: None,
        },
    )
    .expect("insert synthetic event");

    let live_entries = list_recorder_events(
        &conn,
        &RecorderEventsListOptions {
            source: Some("live".to_owned()),
            limit: 100,
            ..RecorderEventsListOptions::default()
        },
    )
    .expect("list live entries");
    assert_eq!(live_entries.len(), 1);
    assert_eq!(live_entries[0].run_id, live.run_id);

    let synthetic_entries = list_recorder_events(
        &conn,
        &RecorderEventsListOptions {
            source: Some("synthetic".to_owned()),
            limit: 100,
            ..RecorderEventsListOptions::default()
        },
    )
    .expect("list synthetic entries");
    assert_eq!(synthetic_entries.len(), 1);
    assert_eq!(synthetic_entries[0].run_id, "run_synthetic_001");
}

#[test]
fn recorder_store_list_filters_by_run_and_time_window() {
    let conn = connect();
    let run_id = "run_window_001";
    conn.insert_recorder_run(
        run_id,
        &CreateRecorderRunInput {
            workspace_id: None,
            agent_id: "agent_window".to_owned(),
            session_id: None,
            source_type: "synthetic".to_owned(),
            source_id: Some("fixture://window".to_owned()),
            status: "active".to_owned(),
            started_at: "2026-01-01T00:00:00Z".to_owned(),
            ended_at: None,
            event_count: 3,
            redacted_count: 0,
            payload_bytes: 0,
            chain_complete: true,
        },
    )
    .expect("insert window run");

    for (event_id, sequence, timestamp, event_hash, previous_event_hash) in [
        (
            "evt_window_001",
            1,
            "2026-01-01T00:00:00Z",
            "blake3:window001",
            None,
        ),
        (
            "evt_window_002",
            2,
            "2026-01-01T00:01:00Z",
            "blake3:window002",
            Some("blake3:window001"),
        ),
        (
            "evt_window_003",
            3,
            "2026-01-01T00:02:00Z",
            "blake3:window003",
            Some("blake3:window002"),
        ),
    ] {
        conn.insert_recorder_event(
            event_id,
            &CreateRecorderEventInput {
                run_id: run_id.to_owned(),
                sequence,
                event_type: "tool_call".to_owned(),
                timestamp: timestamp.to_owned(),
                payload_hash: None,
                payload_bytes: 0,
                redaction_status: "clean".to_owned(),
                redacted_bytes: 0,
                previous_event_hash: previous_event_hash.map(str::to_owned),
                event_hash: event_hash.to_owned(),
                chain_status: if previous_event_hash.is_some() {
                    "linked".to_owned()
                } else {
                    "root".to_owned()
                },
                source_span_id: None,
                source_line_start: None,
                source_line_end: None,
            },
        )
        .expect("insert window event");
    }

    let entries = list_recorder_events(
        &conn,
        &RecorderEventsListOptions {
            run_id: Some(run_id.to_owned()),
            since: Some("2026-01-01T00:01:00Z".to_owned()),
            source: Some("synthetic".to_owned()),
            limit: 100,
        },
    )
    .expect("list filtered");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].event_id, "evt_window_003");
    assert_eq!(entries[1].event_id, "evt_window_002");
    assert!(entries.iter().all(|e| e.run_id == run_id));
    assert!(entries.iter().all(|e| e.event_hash.starts_with("blake3:")));
}

#[test]
fn recorder_store_rejects_malformed_payload_without_persisting() {
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    let mut options = event_options(&start.run_id, Some("too-large"));
    options.max_payload_bytes = 3;

    let err = record_and_persist_event(&conn, &options).expect_err("payload rejected");
    let message = err.to_string();
    assert!(
        message.contains("exceeding the 3 byte limit"),
        "expected payload size validation, got {message}"
    );
    let stored = conn.list_recorder_events(&start.run_id).expect("list");
    assert!(stored.is_empty(), "rejected payload must not persist");
}

#[test]
fn list_recorder_events_with_limit_zero_returns_every_match() {
    // Regression for eidetic_engine_cli-2nkb. Previously
    // `RecorderEventsListOptions::default()` (limit=0) was relayed straight
    // into `LIMIT 0`, silently returning zero rows. The fix treats limit=0
    // as "no limit" so a defaulted caller still observes the data.
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    for tag in ["alpha", "beta", "gamma", "delta", "epsilon"] {
        let _ = record_and_persist_event(&conn, &event_options(&start.run_id, Some(tag)))
            .unwrap_or_else(|err| panic!("event {tag} failed: {err}"));
    }

    let entries = list_recorder_events(
        &conn,
        &RecorderEventsListOptions {
            run_id: Some(start.run_id.clone()),
            limit: 0,
            ..RecorderEventsListOptions::default()
        },
    )
    .expect("list with limit=0");

    assert_eq!(entries.len(), 5, "limit=0 must surface every matching row");
    assert!(entries.iter().all(|e| e.run_id == start.run_id));
}

#[test]
fn list_recorder_events_filtered_with_limit_zero_returns_every_match() {
    // DB-level mirror of the regression: hit the underlying
    // `list_recorder_events_filtered` directly to prove the SQL builder
    // skips the LIMIT clause when limit=0.
    let conn = connect();
    let start = start_and_persist_recording(&conn, &start_options()).expect("start");
    for tag in ["a", "b", "c", "d"] {
        let _ = record_and_persist_event(&conn, &event_options(&start.run_id, Some(tag)))
            .unwrap_or_else(|err| panic!("event {tag} failed: {err}"));
    }

    let stored = conn
        .list_recorder_events_filtered(Some(&start.run_id), None, None, 0)
        .expect("list filtered with limit=0");
    assert_eq!(stored.len(), 4);

    // Sanity: a positive limit still bounds the result.
    let bounded = conn
        .list_recorder_events_filtered(Some(&start.run_id), None, None, 2)
        .expect("list filtered with limit=2");
    assert_eq!(bounded.len(), 2);
}
