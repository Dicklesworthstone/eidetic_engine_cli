//! Integration coverage for persisted recorder tail/follow readers.

use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

use ee::core::recorder::{
    RecorderEventFilter, RecorderTailOptions, TailFollowResult, poll_follow_events_from_store,
    tail_recording_from_store,
};
use ee::db::{CreateRecorderEventInput, CreateRecorderRunInput, DbConnection};

type TestResult = Result<(), String>;

fn connect() -> Result<DbConnection, String> {
    let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
    conn.migrate().map_err(|error| error.to_string())?;
    Ok(conn)
}

fn insert_run(conn: &DbConnection, run_id: &str, status: &str) -> TestResult {
    conn.insert_recorder_run(
        run_id,
        &CreateRecorderRunInput {
            workspace_id: None,
            agent_id: "agent_tail".to_owned(),
            session_id: Some(format!("sess_{run_id}")),
            source_type: "live".to_owned(),
            source_id: None,
            status: status.to_owned(),
            started_at: "2026-05-06T10:00:00Z".to_owned(),
            ended_at: None,
            event_count: 0,
            redacted_count: 0,
            payload_bytes: 0,
            chain_complete: true,
        },
    )
    .map_err(|error| error.to_string())
}

fn insert_event(
    conn: &DbConnection,
    run_id: &str,
    sequence: u64,
    event_type: &str,
    timestamp: &str,
    redaction_status: &str,
) -> TestResult {
    let event_id = format!("evt_{run_id}_{sequence:04}");
    conn.insert_recorder_event(
        &event_id,
        &CreateRecorderEventInput {
            run_id: run_id.to_owned(),
            sequence,
            event_type: event_type.to_owned(),
            timestamp: timestamp.to_owned(),
            payload_hash: Some(format!("blake3:payload{sequence:064x}")),
            payload_bytes: 10 + sequence,
            redaction_status: redaction_status.to_owned(),
            redacted_bytes: if redaction_status == "clean" { 0 } else { 5 },
            previous_event_hash: (sequence > 1)
                .then(|| format!("blake3:event{:064x}", sequence - 1)),
            event_hash: format!("blake3:event{sequence:064x}"),
            chain_status: if sequence == 1 { "root" } else { "linked" }.to_owned(),
            source_span_id: None,
            source_line_start: None,
            source_line_end: None,
        },
    )
    .map_err(|error| error.to_string())
}

#[test]
fn tail_empty_store_returns_successful_empty_snapshot() -> TestResult {
    let conn = connect()?;
    let report = tail_recording_from_store(
        &conn,
        &RecorderTailOptions {
            run_id: None,
            since: None,
            limit: 10,
            from_sequence: None,
            follow: false,
            filter: None,
        },
    )
    .map_err(|error| error.message())?;

    assert!(report.events.is_empty());
    assert_eq!(report.total_events, 0);
    assert!(!report.has_more);
    Ok(())
}

#[test]
fn tail_returns_last_n_events_in_chronological_order() -> TestResult {
    let conn = connect()?;
    insert_run(&conn, "run_tail_n", "active")?;
    insert_event(
        &conn,
        "run_tail_n",
        1,
        "user_message",
        "2026-05-06T10:00:01Z",
        "clean",
    )?;
    insert_event(
        &conn,
        "run_tail_n",
        2,
        "tool_call",
        "2026-05-06T10:00:02Z",
        "clean",
    )?;
    insert_event(
        &conn,
        "run_tail_n",
        3,
        "tool_result",
        "2026-05-06T10:00:03Z",
        "redacted",
    )?;

    let report = tail_recording_from_store(
        &conn,
        &RecorderTailOptions {
            run_id: Some("run_tail_n".to_owned()),
            since: None,
            limit: 2,
            from_sequence: None,
            follow: false,
            filter: None,
        },
    )
    .map_err(|error| error.message())?;

    assert_eq!(report.total_events, 3);
    assert!(report.has_more);
    assert_eq!(report.events.len(), 2);
    assert_eq!(report.events[0].sequence, 2);
    assert_eq!(report.events[1].sequence, 3);
    assert_eq!(report.events[1].redaction_status, "redacted");
    Ok(())
}

#[test]
fn tail_applies_since_and_filter_expression() -> TestResult {
    let conn = connect()?;
    insert_run(&conn, "run_filter", "active")?;
    insert_event(
        &conn,
        "run_filter",
        1,
        "tool_call",
        "2026-05-06T10:00:01Z",
        "redacted",
    )?;
    insert_event(
        &conn,
        "run_filter",
        2,
        "tool_call",
        "2026-05-06T10:00:02Z",
        "clean",
    )?;
    insert_event(
        &conn,
        "run_filter",
        3,
        "tool_result",
        "2026-05-06T10:00:03Z",
        "clean",
    )?;
    let filter = RecorderEventFilter::parse_expression("event_type=tool_call AND redacted=false")
        .map_err(|error| error.message())?;

    let report = tail_recording_from_store(
        &conn,
        &RecorderTailOptions {
            run_id: Some("run_filter".to_owned()),
            since: Some("2026-05-06T10:00:02Z".to_owned()),
            limit: 10,
            from_sequence: None,
            follow: false,
            filter: Some(filter),
        },
    )
    .map_err(|error| error.message())?;

    assert_eq!(report.events.len(), 1);
    assert_eq!(report.events[0].sequence, 2);
    assert_eq!(report.events[0].event_type.as_str(), "tool_call");
    assert!(!report.events[0].redacted);
    Ok(())
}

#[test]
fn follow_poll_emits_multiple_new_events_once() -> TestResult {
    let conn = connect()?;
    insert_run(&conn, "run_follow_multi", "active")?;
    insert_event(
        &conn,
        "run_follow_multi",
        1,
        "user_message",
        "2026-05-06T10:00:01Z",
        "clean",
    )?;
    insert_event(
        &conn,
        "run_follow_multi",
        2,
        "tool_result",
        "2026-05-06T10:00:02Z",
        "clean",
    )?;

    let options = RecorderTailOptions {
        run_id: Some("run_follow_multi".to_owned()),
        since: None,
        limit: 10,
        from_sequence: None,
        follow: true,
        filter: None,
    };
    let seen = BTreeSet::new();
    let result =
        poll_follow_events_from_store(&conn, &options, &seen).map_err(|error| error.message())?;

    let TailFollowResult::Events(events) = result else {
        return Err("expected follow poll to emit events".to_owned());
    };
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].run_id, "run_follow_multi");
    assert_eq!(events[1].sequence, 2);
    assert!(serde_json::from_str::<serde_json::Value>(&events[0].to_jsonl()).is_ok());

    let seen = events
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<BTreeSet<_>>();
    let waiting =
        poll_follow_events_from_store(&conn, &options, &seen).map_err(|error| error.message())?;
    assert!(matches!(waiting, TailFollowResult::Waiting { .. }));
    Ok(())
}

#[test]
fn tail_caps_sql_fetch_to_caller_limit_when_no_client_filters() -> TestResult {
    // Regression for eidetic_engine_cli-yv29: tail_recording_from_store used to pass
    // u32::MAX to the SQL LIMIT, materialising every row in recorder_events even
    // when the caller asked for `--limit 5`. After the fix the SQL LIMIT tracks
    // `options.limit + 1` (no client-side filter terms, so no headroom is needed),
    // bounding both DB IO and the in-memory `total_events` count.
    let conn = connect()?;
    insert_run(&conn, "run_huge", "active")?;
    let total_inserted: u64 = 200;
    for sequence in 1..=total_inserted {
        insert_event(
            &conn,
            "run_huge",
            sequence,
            "tool_call",
            &format!("2026-05-06T10:{:02}:{:02}Z", sequence / 60, sequence % 60),
            "clean",
        )?;
    }

    let options = RecorderTailOptions {
        run_id: Some("run_huge".to_owned()),
        since: None,
        limit: 5,
        from_sequence: None,
        follow: false,
        filter: None,
    };

    let report = tail_recording_from_store(&conn, &options).map_err(|error| error.message())?;

    assert_eq!(report.events.len(), 5);
    assert!(report.has_more);
    // Without the fix `total_events` would equal 200 (every row materialised).
    // With the fix the SQL fetched at most `limit + 1 = 6` rows.
    assert!(
        report.total_events <= u64::from(options.limit) + 1,
        "expected SQL fetch bounded by caller limit, got total_events={} for limit={}",
        report.total_events,
        options.limit,
    );
    // The events returned must still be the most recent slice in chronological order.
    assert_eq!(report.events[0].sequence, total_inserted - 4);
    assert_eq!(
        report.events[report.events.len() - 1].sequence,
        total_inserted
    );
    Ok(())
}

#[test]
fn tail_applies_headroom_when_client_only_filter_terms_present() -> TestResult {
    // With a client-only filter term (`event_type`), the SQL fetch must include
    // headroom so the post-filter limit can still be satisfied — but it must
    // remain bounded (4 * limit, capped at 10_000), not u32::MAX.
    let conn = connect()?;
    insert_run(&conn, "run_filter_headroom", "active")?;
    // 20 events alternating between tool_call and tool_result.
    for sequence in 1..=20u64 {
        let event_type = if sequence % 2 == 0 {
            "tool_result"
        } else {
            "tool_call"
        };
        insert_event(
            &conn,
            "run_filter_headroom",
            sequence,
            event_type,
            &format!("2026-05-06T10:00:{:02}Z", sequence),
            "clean",
        )?;
    }

    let filter = RecorderEventFilter::parse_expression("event_type=tool_call")
        .map_err(|error| error.message())?;
    let options = RecorderTailOptions {
        run_id: Some("run_filter_headroom".to_owned()),
        since: None,
        limit: 3,
        from_sequence: None,
        follow: false,
        filter: Some(filter),
    };

    let report = tail_recording_from_store(&conn, &options).map_err(|error| error.message())?;

    // SQL was bounded at 4*3 + 1 = 13, of which roughly half match the filter,
    // so total_events stays well under the 20 inserted rows.
    assert!(
        report.total_events <= 13,
        "expected SQL bounded by headroom, got total_events={}",
        report.total_events,
    );
    assert!(
        report
            .events
            .iter()
            .all(|event| event.event_type.as_str() == "tool_call")
    );
    assert_eq!(report.events.len(), 3);
    Ok(())
}

#[test]
fn follow_idle_timeout_exits_without_hanging() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    fs::create_dir_all(tempdir.path().join(".ee")).map_err(|error| error.to_string())?;
    let database = tempdir.path().join(".ee").join("ee.db");
    let conn = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    conn.migrate().map_err(|error| error.to_string())?;
    conn.close().map_err(|error| error.to_string())?;

    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(tempdir.path())
        .arg("recorder")
        .arg("follow")
        .arg("--json-lines")
        .arg("--idle-timeout-ms")
        .arg("0")
        .output()
        .map_err(|error| format!("failed to run recorder follow: {error}"))?;

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    Ok(())
}
