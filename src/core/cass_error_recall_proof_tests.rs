//! Completion and attribution regressions, including the real durable store.

use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
};
use serde_json::json;

const WS: &str = "wsp_01234567890123456789012345";

fn store() -> DbConnection {
    let db = DbConnection::open_memory().expect("open database");
    db.migrate().expect("migrate");
    db.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/cass-repair-proof".to_owned(),
            name: None,
        },
    )
    .expect("workspace");
    db
}

fn session(db: &DbConnection, seed: u128) -> String {
    let id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
    db.insert_session(
        &id,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: format!("/sessions/proof-{seed}.jsonl"),
            source_path: None,
            agent_name: Some("codex".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 0,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(&seed.to_le_bytes()).to_hex()),
            metadata_json: None,
        },
    )
    .expect("session");
    id
}

fn span(db: &DbConnection, session: &str, line: u32, kind: &str, role: &str, value: Value) -> String {
    let excerpt = value.to_string();
    let hash = blake3::hash(format!("{session}:{line}").as_bytes());
    let mut seed = [0_u8; 16];
    seed.copy_from_slice(&hash.as_bytes()[..16]);
    let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_bytes(seed)).to_string();
    db.insert_evidence_span(
        &id,
        &CreateEvidenceSpanInput {
            workspace_id: WS.to_owned(),
            session_id: session.to_owned(),
            memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: format!("{session}:{line}"),
            span_kind: kind.to_owned(),
            start_line: line,
            end_line: line,
            start_byte: None,
            end_byte: None,
            role: Some(role.to_owned()),
            content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
            excerpt,
            metadata_json: None,
            inherited_redaction_classes: Vec::new(),
        },
    )
    .expect("evidence");
    id
}

fn call(db: &DbConnection, session: &str, line: u32, id: &str) {
    span(
        db,
        session,
        line,
        "tool_call",
        "assistant",
        json!({"type":"response_item", "payload":{
            "type":"function_call", "name":"exec_command", "call_id":id,
            "arguments":json!({"cmd":"cargo check"}).to_string()
        }}),
    );
}

fn result(db: &DbConnection, session: &str, line: u32, id: &str, output: Value) -> String {
    span(
        db,
        session,
        line,
        "tool_result",
        "tool",
        json!({"type":"response_item", "payload":{
            "type":"function_call_output", "call_id":id, "output":output
        }}),
    )
}

fn repair(db: &DbConnection, session: &str, line: u32) -> String {
    span(
        db,
        session,
        line,
        "message",
        "assistant",
        json!({"type":"assistant", "message":{"role":"assistant", "content":
            "I fixed the mismatched types by converting the widget return value in src/widget.rs."
        }}),
    )
}

#[test]
fn modern_codex_commands_and_execution_wrappers_decode_without_losing_status() {
    for arguments in [
        json!({"cmd":"cargo check"}),
        json!({"command":["bash", "-lc", "cargo check"]}),
    ] {
        for arguments in [arguments.clone(), Value::String(arguments.to_string())] {
            let event = codex_payload_event(&json!({
                "type":"function_call", "name":"exec_command", "call_id":"a",
                "arguments":arguments
            }));
            assert_eq!(
                event,
                Some(ToolEvent::Call {
                    id: "a".to_owned(),
                    command: Some("cargo check".to_owned()),
                })
            );
        }
    }
    assert!(command_argument(&json!({"cmd":"cargo check", "command":"cargo test"})).is_none());
    let event = codex_payload_event(&json!({
        "type":"function_call_output", "call_id":"a",
        "output":{"output":"error[E0308]: mismatched types", "metadata":{"exit_code":101}}
    }))
    .expect("structured result");
    let ToolEvent::Result { output, .. } = event else {
        panic!("expected result")
    };
    assert!(output.failed());
    assert_eq!(failure_diagnostics(&output).len(), 1);
}

#[test]
fn malformed_windows_and_unknown_content_blocks_are_not_partial_successes() {
    let good = json!({"type":"response_item", "payload":{
        "type":"function_call_output", "call_id":"a", "output":"Exit code 0"
    }})
    .to_string();
    assert_eq!(tool_events(&good).len(), 1);
    assert!(tool_events(&format!("{good}\n{{\"payload\":")).is_empty());
    for body in [json!({}), json!([{"type":"image", "text":"not a result"}])] {
        assert!(
            claude_block_event(&json!({
                "type":"tool_result", "tool_use_id":"a", "is_error":false, "content":body
            }))
            .is_none()
        );
    }
}

#[test]
fn modern_codex_completed_retry_persists_a_cited_repair_and_incident_card() {
    let db = store();
    let session = session(&db, 0x60_1000);
    call(&db, &session, 1, "a");
    let failure = result(
        &db,
        &session,
        2,
        "a",
        json!({
            "output":"error[E0308]: mismatched types\n --> src/widget.rs:3:5",
            "metadata":{"exit_code":101}
        }),
    );
    let fix = repair(&db, &session, 3);
    call(&db, &session, 4, "b");
    let proof = result(
        &db,
        &session,
        5,
        "b",
        json!(
            "Chunk ID: abc\nWall time: 0.4 seconds\nProcess exited with code 0\nFinal output:\nFinished dev profile"
        ),
    );
    let report = record_session_error_recall(&db, WS, &session).expect("derive");
    assert_eq!(report.failures_seen, 1, "{report:?}");
    assert_eq!(report.resolved_failures, 1, "{report:?}");
    assert_eq!(report.incident_cards_recorded, 1, "{report:?}");
    let recall = crate::core::error_diagnosis::error_recall_report(
        &db,
        WS,
        &from_rustc(Some("E0308"), "mismatched types"),
    )
    .expect("recall");
    assert!(recall.helpful_repairs.contains(&fix));
    assert_eq!(recall.proof_links, vec![proof]);
    let card = crate::core::incident_card::incident_card_id(WS, &failure);
    assert!(
        db.get_search_admitted_evidence_span(&card, WS)
            .expect("admission")
            .is_some()
    );
    assert_eq!(
        record_session_error_recall(&db, WS, &session)
            .expect("rerun")
            .incident_cards_recorded,
        0
    );
}

#[test]
fn unknown_or_incomplete_codex_results_cannot_persist_helpful_links() {
    let db = store();
    for (offset, output) in [
        json!("test result: ok. 21 passed; 0 failed"),
        json!("Process running with session ID 42"),
        json!("test result: ok. 21 passed; 0 failed\nProcess exited with code 101"),
        json!({"output":"Exit code 0", "metadata":{"exit_code":"unknown"}}),
        json!({"metadata":{"exit_code":0}}),
    ]
    .into_iter()
    .enumerate()
    {
        let session = session(&db, 0x60_1100 + offset as u128);
        call(&db, &session, 1, "a");
        result(
            &db,
            &session,
            2,
            "a",
            json!({
                "output":"error[E0308]: mismatched types", "metadata":{"exit_code":101}
            }),
        );
        repair(&db, &session, 3);
        call(&db, &session, 4, "b");
        result(&db, &session, 5, "b", output.clone());
        let report = record_session_error_recall(&db, WS, &session).expect("derive");
        assert_eq!(report.failures_seen, 1, "{output}: {report:?}");
        assert_eq!(report.resolved_failures, 0, "{output}: {report:?}");
        assert_eq!(report.repair_links_recorded, 0, "{output}: {report:?}");
        assert_eq!(report.incident_cards_recorded, 0, "{output}: {report:?}");
    }
    assert!(
        db.get_error_fingerprint(WS, "rustc:E0308")
            .expect("fingerprint")
            .is_some()
    );
    assert!(
        db.list_error_repair_links(WS, "rustc:E0308")
            .expect("links")
            .is_empty()
    );
}

fn failure(db: &DbConnection, session: &str, line: u32, id: &str) -> String {
    result(
        db,
        session,
        line,
        id,
        json!({
            "output":"error[E0308]: mismatched types", "metadata":{"exit_code":101}
        }),
    )
}

fn success(db: &DbConnection, session: &str, line: u32, id: &str) -> String {
    result(
        db,
        session,
        line,
        id,
        json!({"output":"Finished dev profile", "metadata":{"exit_code":0}}),
    )
}

#[test]
fn repair_intervals_require_failure_then_call_then_result() {
    for failure in 0..8 {
        for call in 0..8 {
            for result in 0..8 {
                let range = repair_span_range(failure, call, result);
                assert_eq!(range.is_some(), failure < call && call < result);
                if let Some(range) = range {
                    assert_eq!(range, failure + 1..call);
                    assert!(!range.contains(&call));
                    assert!(!range.contains(&result));
                }
            }
        }
    }
    assert_eq!(repair_span_range(usize::MAX, 0, usize::MAX), None);
    assert_eq!(
        repair_span_range(usize::MAX - 2, usize::MAX - 1, usize::MAX),
        Some(usize::MAX - 1..usize::MAX - 1)
    );
}

#[test]
fn a_result_from_a_command_started_before_the_failure_is_not_a_retry() {
    let db = store();
    let session = session(&db, 0x60_1200);
    call(&db, &session, 1, "failed");
    call(&db, &session, 2, "already-running");
    failure(&db, &session, 3, "failed");
    let fix = repair(&db, &session, 4);
    let stale = success(&db, &session, 5, "already-running");
    let report = record_session_error_recall(&db, WS, &session).expect("derive");
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 0);
    assert_eq!(report.repair_links_recorded, 0);
    assert_eq!(report.incident_cards_recorded, 0);

    call(&db, &session, 6, "actual-retry");
    let proof = success(&db, &session, 7, "actual-retry");
    let report = record_session_error_recall(&db, WS, &session).expect("derive retry");
    assert_eq!(report.resolved_failures, 1);
    let recall = crate::core::error_diagnosis::error_recall_report(
        &db,
        WS,
        &from_rustc(Some("E0308"), "mismatched types"),
    )
    .expect("recall");
    assert!(recall.helpful_repairs.contains(&fix));
    assert_eq!(recall.proof_links, vec![proof]);
    assert!(!recall.proof_links.contains(&stale));
}

#[test]
fn replayed_results_cannot_reopen_a_consumed_call_or_duplicate_a_failure() {
    let db = store();
    let session = session(&db, 0x60_1201);
    call(&db, &session, 1, "a");
    failure(&db, &session, 2, "a");
    repair(&db, &session, 3);
    success(&db, &session, 4, "a");
    failure(&db, &session, 5, "a");
    let report = record_session_error_recall(&db, WS, &session).expect("derive");
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 0);
    assert_eq!(report.incident_cards_recorded, 0);
    assert!(
        db.list_error_repair_links(WS, "rustc:E0308")
            .expect("links")
            .is_empty()
    );

    call(&db, &session, 6, "b");
    success(&db, &session, 7, "b");
    let report = record_session_error_recall(&db, WS, &session).expect("real retry");
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
}

#[test]
fn messages_after_launch_and_pure_narration_are_never_helpful_repairs() {
    let db = store();
    for late in [false, true] {
        let session = session(&db, 0x60_1210 + u128::from(late));
        call(&db, &session, 1, "a");
        failure(&db, &session, 2, "a");
        if late {
            call(&db, &session, 3, "b");
            repair(&db, &session, 4);
        } else {
            span(
                &db,
                &session,
                3,
                "message",
                "assistant",
                json!({"type":"assistant", "message":{
                    "role":"assistant", "content":"Let me look into it."
                }}),
            );
            call(&db, &session, 4, "b");
        }
        success(&db, &session, 5, "b");
        let report = record_session_error_recall(&db, WS, &session).expect("derive");
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.incident_cards_recorded, 0);
    }
    let recall = crate::core::error_diagnosis::error_recall_report(
        &db,
        WS,
        &from_rustc(Some("E0308"), "mismatched types"),
    )
    .expect("recall");
    assert!(recall.helpful_repairs.is_empty(), "{recall:?}");
    assert_eq!(recall.proof_links.len(), 2);
}

#[test]
fn bundled_failure_and_success_results_do_not_panic_or_invent_a_repair() {
    let db = store();
    let session = session(&db, 0x60_1220);
    call(&db, &session, 1, "a");
    call(&db, &session, 2, "b");
    span(
        &db,
        &session,
        3,
        "tool_result",
        "user",
        json!({"type":"user", "message":{"role":"user", "content":[
            {"type":"tool_result", "tool_use_id":"a", "is_error":true,
             "content":"error[E0308]: mismatched types"},
            {"type":"tool_result", "tool_use_id":"b", "is_error":false,
             "content":"Finished dev profile"}
        ]}}),
    );
    let report = record_session_error_recall(&db, WS, &session).expect("bundled results");
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 0);
    assert_eq!(report.incident_cards_recorded, 0);
    repair(&db, &session, 4);
    call(&db, &session, 5, "c");
    success(&db, &session, 6, "c");
    let report = record_session_error_recall(&db, WS, &session).expect("ordered retry");
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
}

#[test]
fn duplicate_call_ids_are_ambiguous_but_do_not_block_unrelated_valid_arcs() {
    let db = store();
    let session = session(&db, 0x60_1221);
    call(&db, &session, 1, "duplicate");
    call(&db, &session, 2, "duplicate");
    failure(&db, &session, 3, "duplicate");
    call(&db, &session, 4, "valid-failure");
    failure(&db, &session, 5, "valid-failure");
    repair(&db, &session, 6);
    call(&db, &session, 7, "valid-retry");
    success(&db, &session, 8, "valid-retry");
    let report = record_session_error_recall(&db, WS, &session).expect("derive");
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
}
