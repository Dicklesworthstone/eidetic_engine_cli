//! Realistic event volume must reach diagnosis and proof without losing bounds.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::cass_error_recall::{
    ToolOutput, failure_diagnostics, structured_error_diagnostics,
    test_coverage::{Proof, Requirement},
};
use serde_json::{Value, json};

const PACKAGE: &str = "example.org/scale";

fn events(count: usize, failure: Option<usize>, skipped: Option<usize>) -> Vec<Value> {
    let mut events = vec![json!({"Action":"start","Package":PACKAGE})];
    for index in 0..count {
        let name = format!("TestCase{index:03}");
        let action = if failure == Some(index) {
            "fail"
        } else if skipped == Some(index) {
            "skip"
        } else {
            "pass"
        };
        events.extend([
            json!({"Action":"run","Package":PACKAGE,"Test":name}),
            json!({"Action":"output","Package":PACKAGE,"Test":name,"Output":format!("=== RUN   {name}\n")}),
            json!({"Action":"output","Package":PACKAGE,"Test":name,"Output":format!("--- {}: {name} (0.00s)\n", action.to_ascii_uppercase())}),
            json!({"Action":action,"Package":PACKAGE,"Test":name,"Elapsed":0.0}),
        ]);
    }
    events.push(json!({
        "Action":if failure.is_some() { "fail" } else { "pass" },
        "Package":PACKAGE,"Elapsed":0.01,
    }));
    events
}

fn output(events: &[Value]) -> ToolOutput {
    ToolOutput {
        text: events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

fn decoded(output: &ToolOutput) -> CompilerOutput {
    super::super::inspect(&output.text).unwrap().unwrap()
}

#[test]
fn hundred_test_reports_feed_diagnosis_and_coverage_preserving_growth() {
    let original = events(100, Some(99), None);
    assert_eq!(original.len(), 402);
    assert!(original.len() > super::super::MAX_COMPILER_RECORDS);
    let failed = output(&original);
    assert!(failed.text.len() < MAX_SCANNED_OUTPUT_BYTES);
    let parsed = decoded(&failed);
    assert!(parsed.failed);
    assert!(!parsed.allows_completion);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(failure_diagnostics(&failed), parsed.diagnostics);
    assert_eq!(
        structured_error_diagnostics(&failed.text),
        parsed.diagnostics
    );
    let coverage = parsed.go_test_coverage.unwrap();
    assert!(coverage.complete);
    assert_eq!(coverage.passed_tests.len(), 99);
    assert_eq!(coverage.failed_tests.len(), 1);
    let required = Requirement::capture(&failed, true);
    assert!(required.is_guarded());
    let retry = output(&events(101, None, None));
    assert!(retry.succeeded());
    assert!(required.is_verified_by(&Proof::capture(&retry)));
}

#[test]
fn late_failure_and_skipped_control_cannot_hide_beyond_the_old_record_cap() {
    let failed = output(&events(100, Some(99), None));
    let required = Requirement::capture(&failed, true);
    let skipped = output(&events(101, None, Some(98)));
    assert!(skipped.succeeded());
    assert!(!required.is_verified_by(&Proof::capture(&skipped)));
    let new_failure = output(&events(101, Some(100), None));
    assert!(new_failure.failed());
    assert!(!new_failure.succeeded());
    assert!(!required.is_verified_by(&Proof::capture(&new_failure)));
    let mut dropped = events(101, None, None);
    dropped.retain(|event| event.get("Test").and_then(Value::as_str) != Some("TestCase000"));
    assert!(output(&dropped).succeeded());
    assert!(!required.is_verified_by(&Proof::capture(&output(&dropped))));
}

#[test]
fn damaged_late_records_never_salvage_an_already_complete_prefix() {
    let valid = output(&events(100, None, None));
    assert!(valid.succeeded());
    for tail in [
        "{",
        r#"{"Action":"pass","Package":"example.org/scale"}"#,
        r#"{"Action":"start","Action":"pass","Package":"other"}"#,
        r#"{"reason":"build-finished","success":true}"#,
        r#"{"Action":"unknown","Package":"other"}"#,
    ] {
        let text = format!("{}\n{tail}", valid.text);
        assert!(text.len() < MAX_SCANNED_OUTPUT_BYTES);
        assert!(super::super::inspect(&text).is_err());
        let bad = ToolOutput {
            text,
            ..ToolOutput::default()
        };
        assert!(!bad.succeeded());
    }
}

#[test]
fn exact_go_record_boundary_counts_build_and_opaque_output_events() {
    let mut records = events(1, None, None);
    records.insert(
        0,
        json!({"Action":"build-output","ImportPath":"dep","Output":""}),
    );
    while records.len() < MAX_GO_RECORDS {
        records.insert(2, json!({"Action":"output","Package":PACKAGE,"Output":""}));
    }
    let at_limit = output(&records);
    assert!(at_limit.text.len() < MAX_SCANNED_OUTPUT_BYTES);
    assert!(decoded(&at_limit).allows_completion);
    records.insert(2, json!({"Action":"output","Package":PACKAGE,"Output":""}));
    let over = output(&records);
    assert!(over.text.len() < MAX_SCANNED_OUTPUT_BYTES);
    assert!(inspect(&over.text).is_err());
    assert!(!over.succeeded());
}

#[test]
fn go_byte_limit_is_unchanged_and_counts_unicode_and_trailing_whitespace() {
    let valid = output(&events(1, None, None));
    let exact = format!(
        "{}{}",
        valid.text,
        " ".repeat(MAX_SCANNED_OUTPUT_BYTES - valid.text.len())
    );
    assert_eq!(exact.len(), MAX_SCANNED_OUTPUT_BYTES);
    assert!(inspect(&exact).unwrap().unwrap().allows_completion);
    assert!(inspect(&(exact + " ")).is_err());
    let huge_first = json!({
        "Action":"output","Package":PACKAGE,"Output":"界".repeat(MAX_SCANNED_OUTPUT_BYTES / 3),
    })
    .to_string();
    assert!(huge_first.len() > MAX_SCANNED_OUTPUT_BYTES);
    assert!(inspect(&huge_first).is_err());
    assert!(super::super::inspect(&huge_first).is_err());
}

#[test]
fn expanded_go_intake_does_not_enlarge_the_cargo_compiler_budget() {
    let mut records =
        vec![json!({"reason":"compiler-artifact"}); super::super::MAX_COMPILER_RECORDS - 1];
    records.push(json!({"reason":"build-finished","success":true}));
    assert!(super::super::inspect(&output(&records).text).is_ok());
    records.insert(0, json!({"reason":"compiler-artifact"}));
    assert!(super::super::inspect(&output(&records).text).is_err());
}

#[test]
fn large_reports_still_require_complete_lifecycles_and_process_observation() {
    let failed = output(&events(100, Some(99), None));
    let retry = output(&events(101, None, None));
    let required = Requirement::capture(&failed, true);
    assert!(required.is_verified_by(&Proof::capture(&retry)));
    assert!(!Requirement::capture(&failed, false).is_verified_by(&Proof::capture(&retry)));
    let unknown_process = ToolOutput {
        text: retry.text.clone(),
        ..ToolOutput::default()
    };
    assert!(!unknown_process.succeeded());
    assert!(!required.is_verified_by(&Proof::capture(&unknown_process)));
    let mut partial = events(100, Some(99), None);
    partial.pop();
    let incomplete = output(&partial);
    assert!(decoded(&incomplete).failed);
    assert!(!decoded(&incomplete).go_test_coverage.unwrap().complete);
    assert!(!Requirement::capture(&incomplete, true).is_verified_by(&Proof::capture(&retry)));
}

#[test]
fn accumulated_large_polls_keep_every_executed_control() {
    let partial_scope = output(&events(1, Some(0), None));
    let mut required = Requirement::capture(&partial_scope, true);
    let narrow_retry = output(&events(1, None, None));
    assert!(required.is_verified_by(&Proof::capture(&narrow_retry)));
    let complete_failure = output(&events(100, Some(0), None));
    required.refresh(&complete_failure, true);
    assert!(!required.is_verified_by(&Proof::capture(&narrow_retry)));
    let complete_retry = output(&events(101, None, None));
    assert!(required.is_verified_by(&Proof::capture(&complete_retry)));
    let damaged = ToolOutput {
        text: complete_failure.text + "\n{",
        ..ToolOutput::default()
    };
    required.refresh(&damaged, true);
    assert!(!required.is_verified_by(&Proof::capture(&complete_retry)));
}

#[test]
fn large_store_repair_rejects_dropped_controls_then_records_cited_full_retry()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::core::cass_error_recall::record_session_error_recall;
    use crate::core::error_diagnosis::error_recall_report;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
        EvidenceProducerKind,
    };

    const WS: &str = "wsp_01234567890123456789012345";
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    db.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/cass-go-intake-recall".to_owned(),
            name: None,
        },
    )?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x60_5000)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "/sessions/go-intake.jsonl".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 0,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"go-intake").to_hex()),
            metadata_json: None,
        },
    )?;
    let append = |number: u32,
                  kind: &str,
                  role: &str,
                  record: Value|
     -> Result<String, crate::db::DbError> {
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x60_5000 + u128::from(number),
        ))
        .to_string();
        let excerpt = record.to_string();
        db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("go-intake:{number}"),
                span_kind: kind.to_owned(),
                start_line: number,
                end_line: number,
                start_byte: None,
                end_byte: None,
                role: Some(role.to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        Ok(id)
    };
    let call = |number, id| {
        append(
            number,
            "tool_call",
            "assistant",
            json!({
                "type":"assistant","message":{"role":"assistant","content":[{
                    "type":"tool_use","id":id,"name":"Bash","input":{"command":"go test -json ./..."}
                }]}
            }),
        )
    };
    let result = |number, id, text: String, is_error| {
        append(
            number,
            "tool_result",
            "user",
            json!({
                "type":"user","message":{"role":"user","content":[{
                    "type":"tool_result","tool_use_id":id,"content":text,"is_error":is_error
                }]}
            }),
        )
    };
    call(1, "failure")?;
    let failure = output(&events(100, Some(99), None));
    let diagnostic = decoded(&failure).diagnostics.remove(0);
    let failure_id = result(2, "failure", failure.text, true)?;
    let repair_id = append(
        3,
        "message",
        "assistant",
        json!({
            "type":"assistant","message":{"role":"assistant","content":[{
                "type":"text","text":"Fixed the Go test failure by flushing pending cache writes in cache.go."
            }]}
        }),
    )?;
    call(4, "dropped-control")?;
    let mut narrowed = events(101, None, None);
    narrowed.retain(|event| event.get("Test").and_then(Value::as_str) != Some("TestCase000"));
    let wrong = result(5, "dropped-control", output(&narrowed).text, false)?;
    let report = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 0);
    assert_eq!(report.incident_cards_recorded, 0);
    assert!(
        error_recall_report(&db, WS, &diagnostic)?
            .helpful_repairs
            .is_empty()
    );
    call(6, "full-retry")?;
    let proof = result(
        7,
        "full-retry",
        output(&events(101, None, None)).text,
        false,
    )?;
    let report = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    let recall = error_recall_report(&db, WS, &diagnostic)?;
    assert!(recall.helpful_repairs.contains(&repair_id));
    assert_eq!(recall.proof_links, vec![proof]);
    assert!(!recall.proof_links.contains(&wrong));
    let card = crate::core::incident_card::incident_card_id(WS, &failure_id);
    assert!(db.get_search_admitted_evidence_span(&card, WS)?.is_some());
    assert_eq!(
        record_session_error_recall(&db, WS, &session)?.incident_cards_recorded,
        0
    );
    Ok(())
}
