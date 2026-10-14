use super::*;
use super::super::{Proof, Requirement};
use crate::core::cass_error_recall::{ToolOutput, record_session_error_recall};
use serde_json::{Value, json};

fn failure() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/cass/pytest-json-report-collection-failure.json"
    ))
    .expect("native collection failure")
}

fn repaired() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/cass/pytest-json-report-collection-repaired.json"
    ))
    .expect("native repaired collection and execution")
}

fn output(report: &Value) -> ToolOutput {
    ToolOutput {
        text: report.to_string(),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

fn verifies(failed: &Value, passed: &Value) -> bool {
    Requirement::capture(&output(failed), true).is_verified_by(&Proof::capture(&output(passed)))
}

fn collector<'a>(report: &'a mut Value, id: &str) -> &'a mut Value {
    report["collectors"]
        .as_array_mut()
        .expect("collectors")
        .iter_mut()
        .find(|row| row["nodeid"] == id)
        .expect("collector identity")
}

#[test]
fn native_collection_abort_is_repaired_only_after_a_descendant_actually_passes() {
    let failure = failure();
    let repaired = repaired();
    assert_eq!(failure["exitcode"], 2);
    assert_eq!(failure["summary"]["total"], 0);
    assert_eq!(failure["summary"]["collected"], 1);
    let decoded = decode(&failure.to_string()).expect("valid").expect("pytest");
    assert!(decoded.output.failed);
    assert!(decoded.execution.has_known_failures());
    assert_eq!(decoded.output.diagnostics.len(), 1);
    assert!(!output(&failure).succeeded());
    assert!(output(&repaired).succeeded());
    assert!(verifies(&failure, &repaired));
    let mut reordered = repaired.clone();
    reordered["tests"].as_array_mut().expect("tests").reverse();
    reordered["collectors"].as_array_mut().expect("collectors").reverse();
    assert!(verifies(&failure, &reordered));
}

#[test]
fn clean_counts_and_matching_collector_names_are_not_execution_coverage() {
    let mut report = repaired();
    report["tests"] = json!([]);
    report["summary"] = json!({"collected":3,"total":0});
    assert!(!verifies(&failure(), &report));
    report = repaired();
    for collector in report["collectors"].as_array_mut().expect("collectors") {
        collector.as_object_mut().expect("collector").remove("result");
    }
    assert!(output(&report).succeeded(), "individual tests really passed");
    assert!(!verifies(&failure(), &report), "no observed collector relation");
    report = repaired();
    collector(&mut report, "test_collect_repair.py")["result"] = json!([]);
    assert!(output(&report).succeeded());
    assert!(!verifies(&failure(), &report), "a missing class edge is not a prefix match");
}

#[test]
fn an_unrelated_module_or_different_root_cannot_cover_the_failed_collector() {
    let renamed = repaired().to_string().replace("test_collect_repair.py", "test_other.py");
    let renamed: Value = serde_json::from_str(&renamed).expect("renamed fixture");
    assert!(output(&renamed).succeeded());
    assert!(!verifies(&failure(), &renamed));
    let mut changed = repaired();
    changed["root"] = json!("/fixture/different-project");
    assert!(output(&changed).succeeded());
    assert!(!verifies(&failure(), &changed));
}

#[test]
fn skipping_or_xfailing_every_descendant_cannot_inherit_a_control_tests_pass() {
    for status in ["skipped", "xfailed", "xpassed"] {
        let mut report = repaired();
        for test in report["tests"].as_array_mut().expect("tests") {
            if test["nodeid"].as_str().expect("node").starts_with("test_collect_repair.py::") {
                test["outcome"] = json!(status);
                test["call"] = json!({
                    "duration":0.0,
                    "outcome": if status == "xpassed" { "passed" } else { "skipped" }
                });
            }
        }
        report["summary"] = json!({"total":3,"collected":3,"passed":1});
        report["summary"][status] = json!(2);
        assert!(output(&report).succeeded(), "independent control still passed");
        assert!(!verifies(&failure(), &report), "{status}");
    }
}

#[test]
fn every_failed_collector_needs_its_own_executed_descendant() {
    let mut failed = failure();
    let control = collector(&mut failed, "test_collect_control.py");
    control["outcome"] = json!("failed");
    control.as_object_mut().expect("collector").remove("result");
    failed["summary"]["collected"] = json!(0);
    assert!(verifies(&failed, &repaired()));
    let mut partial = repaired();
    partial["tests"].as_array_mut().expect("tests").retain(|test| {
        !test["nodeid"].as_str().expect("node").starts_with("test_collect_control.py::")
    });
    partial["summary"] = json!({"total":2,"collected":2,"passed":2});
    assert!(output(&partial).succeeded());
    assert!(!verifies(&failed, &partial));
}

#[test]
fn summary_only_root_errors_and_mixed_collection_runs_remain_unproven() {
    let mut summary_only = failure();
    summary_only.as_object_mut().expect("report").remove("tests");
    let mut root_failure = failure();
    root_failure["collectors"] = json!([{"nodeid":"","outcome":"failed"}]);
    let mut mixed = failure();
    let passed = repaired();
    mixed["tests"] = json!([passed["tests"][2].clone()]);
    mixed["summary"] = json!({"collected":1,"total":1,"passed":1});
    for value in [summary_only, root_failure, mixed] {
        assert!(decode(&value.to_string()).expect("valid incomplete report").is_some());
        assert!(!verifies(&value, &passed));
    }
    for code in [0, 3, 4, 5] {
        let mut failed = failure();
        failed["exitcode"] = json!(code);
        assert!(!verifies(&failed, &passed));
    }
}

#[test]
fn duplicate_owners_cycles_and_invalid_child_records_cannot_authorize_a_repair() {
    let mut duplicate = repaired();
    let children = collector(&mut duplicate, "test_collect_repair.py::TestCache")["result"]
        .as_array_mut().expect("children");
    children.push(children[0].clone());
    let mut multiple_owners = repaired();
    let child = collector(&mut multiple_owners, "test_collect_repair.py::TestCache")["result"][0].clone();
    collector(&mut multiple_owners, "test_collect_control.py")["result"]
        .as_array_mut().expect("children").push(child);
    let mut cycle = repaired();
    collector(&mut cycle, "")["result"].as_array_mut().expect("children")
        .retain(|child| child["nodeid"] != "test_collect_repair.py");
    collector(&mut cycle, "test_collect_repair.py::TestCache")["result"]
        .as_array_mut().expect("children")
        .push(json!({"nodeid":"test_collect_repair.py"}));
    let mut invalid = repaired();
    collector(&mut invalid, "test_collect_repair.py")["result"] = json!([{"nodeid":"\n"}]);
    for report in [duplicate, multiple_owners, cycle, invalid] {
        assert!(decode(&report.to_string()).is_err());
        assert!(!verifies(&failure(), &report));
    }
}

#[test]
fn collection_edges_are_bounded_independently_of_small_test_inventories() {
    let mut report = repaired();
    let nodes: Vec<_> = (0..=MAX_RESULTS).map(|i| json!({"nodeid":format!("n{i}")})).collect();
    collector(&mut report, "")["result"] = json!(nodes);
    assert!(report.to_string().len() < MAX_SCANNED_OUTPUT_BYTES);
    assert!(decode(&report.to_string()).is_err());
}

#[test]
fn incomplete_poll_and_failed_process_cannot_certify_collection_repair() {
    let failed = output(&failure());
    let good = output(&repaired());
    let proof = Proof::capture(&good);
    let mut required = Requirement::capture(&failed, false);
    assert!(!required.is_verified_by(&proof));
    required.refresh(&failed, true);
    assert!(required.is_verified_by(&proof));
    assert!(!required.is_verified_by(&Proof::capture(&ToolOutput {
        exit_code: Some(1), ..good.clone()
    })));
    for text in ["{}", "Finished successfully", "{"] {
        required.refresh(&ToolOutput { text: text.to_owned(), ..ToolOutput::default() }, true);
        assert!(required.is_guarded());
        assert!(!required.is_verified_by(&proof));
    }
}

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";

fn store_span(
    db: &crate::db::DbConnection,
    session: &str,
    line: u32,
    kind: &str,
    role: &str,
    excerpt: Value,
) -> TestResult<String> {
    let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(8000 + u128::from(line)))
        .to_string();
    let excerpt = excerpt.to_string();
    db.insert_evidence_span(&id, &crate::db::CreateEvidenceSpanInput {
        workspace_id: WS.to_owned(), session_id: session.to_owned(), memory_id: None,
        producer_kind: crate::db::EvidenceProducerKind::CassImport,
        cass_span_id: format!("{session}:{line}"), span_kind: kind.to_owned(),
        start_line: line, end_line: line, start_byte: None, end_byte: None,
        role: Some(role.to_owned()),
        content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
        excerpt, metadata_json: None, inherited_redaction_classes: Vec::new(),
    })?;
    Ok(id)
}

fn invocation(db: &crate::db::DbConnection, session: &str, line: u32, id: &str) -> TestResult {
    store_span(db, session, line, "tool_call", "assistant", json!({
        "type":"response_item", "payload":{
            "type":"function_call", "name":"exec_command", "call_id":id,
            "arguments":json!({"cmd":"pytest --json-report"}).to_string()
        }
    }))?;
    Ok(())
}

fn result(db: &crate::db::DbConnection, session: &str, line: u32, id: &str, report: Value) -> TestResult<String> {
    store_span(db, session, line, "tool_result", "tool", json!({
        "type":"response_item", "payload":{
            "type":"function_call_output", "call_id":id,
            "output":{"output":report.to_string(), "metadata":{"exit_code":report["exitcode"]}}
        }
    }))
}

#[test]
fn stored_collection_failure_needs_matching_execution_before_a_cited_incident_is_created() -> TestResult {
    let db = crate::db::DbConnection::open_memory()?;
    db.migrate()?;
    db.insert_workspace(WS, &crate::db::CreateWorkspaceInput {
        path: "/tmp/pytest-collection-repair".to_owned(), name: None,
    })?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(8000)).to_string();
    db.insert_session(&session, &crate::db::CreateSessionInput {
        workspace_id: WS.to_owned(), cass_session_id: "/sessions/pytest-collection.jsonl".to_owned(),
        source_path: None, agent_name: Some("codex".to_owned()), model: None,
        started_at: None, ended_at: None, message_count: 0, token_count: None,
        content_hash: format!("blake3:{}", blake3::hash(b"collection-session").to_hex()),
        metadata_json: None,
    })?;
    invocation(&db, &session, 1, "failed")?;
    let failing_span = result(&db, &session, 2, "failed", failure())?;
    let repair = store_span(&db, &session, 3, "message", "assistant", json!({
        "type":"assistant", "message":{"role":"assistant", "content":
        "I fixed the cache contract import in test_collect_repair.py by defining the missing initialization before collecting the cache test class."}
    }))?;
    invocation(&db, &session, 4, "unrelated")?;
    let other: Value = serde_json::from_str(&repaired().to_string().replace("test_collect_repair.py", "test_other.py"))?;
    result(&db, &session, 5, "unrelated", other)?;
    let first = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(first.failures_seen, 1);
    assert_eq!(first.resolved_failures, 0);
    assert_eq!(first.incident_cards_recorded, 0);
    let source = db.get_evidence_span(&failing_span)?.ok_or("missing failure source")?;
    invocation(&db, &session, 6, "matching")?;
    let proof = result(&db, &session, 7, "matching", repaired())?;
    let second = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(second.resolved_failures, 1);
    assert_eq!(second.incident_cards_recorded, 1);
    assert_eq!(db.get_evidence_span(&failing_span)?, Some(source));
    let canonical = decode(&failure().to_string()).map_err(|_| "decoder")?
        .ok_or("missing pytest report")?.output.diagnostics.remove(0);
    let recall = crate::core::error_diagnosis::error_recall_report(&db, WS, &canonical)?;
    assert!(recall.helpful_repairs.contains(&repair));
    assert_eq!(recall.proof_links, vec![proof]);
    let card = crate::core::incident_card::incident_card_id(WS, &failing_span);
    assert!(db.get_search_admitted_evidence_span(&card, WS)?.is_some());
    assert_eq!(record_session_error_recall(&db, WS, &session)?.incident_cards_recorded, 0);
    Ok(())
}
