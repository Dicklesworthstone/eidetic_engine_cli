//! Completed failure -> later invocation -> completed retry, through real rows.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::error_diagnosis::{
    error_recall_report, pack_error_recall_query_seed, recalled_repair_evidence,
};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
};
use serde_json::json;

const WS: &str = "wsp_01234567890123456789012345";
const ERROR: &str = "error[E0308]: mismatched types\n --> src/widget.rs:3:5\n";
const FIX: &str =
    "I fixed the mismatched types by converting the widget return value in src/widget.rs.";

struct Fixture {
    db: DbConnection,
    session: String,
    line: u32,
}

impl Fixture {
    fn new() -> Self {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            WS,
            &CreateWorkspaceInput {
                path: "/tmp/cass-stream-repair".to_owned(),
                name: None,
            },
        )
        .unwrap();
        let session =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x60_5100)).to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "/sessions/stream-repair.jsonl".to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 0,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"stream-repair").to_hex()),
                metadata_json: None,
            },
        )
        .unwrap();
        Self {
            db,
            session,
            line: 0,
        }
    }

    fn add(&mut self, kind: &str, role: &str, value: Value) -> String {
        self.line += 1;
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x60_5200 + u128::from(self.line),
        ))
        .to_string();
        let excerpt = value.to_string();
        self.db
            .insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: WS.to_owned(),
                    session_id: self.session.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("stream:{}", self.line),
                    span_kind: kind.to_owned(),
                    start_line: self.line,
                    end_line: self.line,
                    start_byte: None,
                    end_byte: None,
                    role: Some(role.to_owned()),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                    excerpt,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .unwrap();
        id
    }

    fn call(&mut self, id: &str, command: &str) {
        let _ = self.add(
            "tool_call",
            "assistant",
            json!({
                "type":"response_item","payload":{
                    "type":"function_call","name":"exec_command","call_id":id,
                    "arguments":json!({"cmd":command}).to_string()
                }
            }),
        );
    }

    fn poll(&mut self, id: &str, chars: &str) {
        let _ = self.add(
            "tool_call",
            "assistant",
            json!({
                "type":"response_item","payload":{
                    "type":"function_call","name":"write_stdin","call_id":id,
                    "arguments":json!({"session_id":42,"chars":chars}).to_string()
                }
            }),
        );
    }

    fn raw_result(&mut self, id: &str, output: &str) -> String {
        self.add(
            "tool_result",
            "tool",
            json!({
                "type":"response_item","payload":{
                    "type":"function_call_output","call_id":id,"output":output
                }
            }),
        )
    }

    fn result(&mut self, id: &str, body: &str, code: Option<i32>) -> String {
        let status = code.map_or_else(
            || "Process running with session ID 42".to_owned(),
            |code| format!("Process exited with code {code}"),
        );
        self.raw_result(
            id,
            &format!("Wall time: 0.01 seconds\n{status}\nOutput:\n{body}"),
        )
    }

    fn repair(&mut self) -> String {
        self.add(
            "message",
            "assistant",
            json!({
                "type":"assistant","message":{"role":"assistant","content":FIX}
            }),
        )
    }

    fn derive(&self) -> CassErrorRecallReport {
        record_session_error_recall(&self.db, WS, &self.session).unwrap()
    }

    fn start_failure(&mut self, complete: bool) -> String {
        self.call("failure", "cargo check");
        self.result("failure", ERROR, complete.then_some(101))
    }

    fn no_repair(&self) {
        let report = self.derive();
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        let recall = error_recall_report(&self.db, WS, &diagnostic()).unwrap();
        assert!(recall.exact);
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
    }
}

fn diagnostic() -> CanonicalDiagnostic {
    from_rustc(Some("E0308"), "mismatched types")
}

#[test]
fn a_running_compiler_failure_remains_searchable_but_cannot_be_claimed_repaired() {
    let mut f = Fixture::new();
    let _ = f.start_failure(false);
    let _ = f.repair();
    f.call("overlap", "cargo check");
    let _ = f.result("overlap", "Finished dev profile\n", Some(0));
    f.no_repair();
    // Repeating an incomplete session import cannot create a positive link.
    f.no_repair();
}

#[test]
fn a_retry_launched_before_failure_completion_is_not_retroactively_a_valid_proof() {
    let mut f = Fixture::new();
    let _ = f.start_failure(false);
    let _ = f.repair();
    f.call("overlap", "cargo check");
    f.poll("failure-end", "");
    let _ = f.result("failure-end", "", Some(101));
    let _ = f.result("overlap", "Finished dev profile\n", Some(0));
    f.no_repair();
    // A genuinely later launch can resolve the same preserved failure.
    let fix = f.repair();
    f.call("later", "cargo check");
    let proof = f.result("later", "Finished dev profile\n", Some(0));
    assert_eq!(f.derive().resolved_failures, 1);
    let recall = error_recall_report(&f.db, WS, &diagnostic()).unwrap();
    assert!(recall.helpful_repairs.contains(&fix));
    assert_eq!(recall.proof_links, vec![proof]);
}

#[test]
fn streamed_retry_closes_the_durable_recall_and_pack_loop_with_original_provenance() {
    let mut f = Fixture::new();
    let failure = f.start_failure(true);
    let fix = f.repair();
    f.call("retry", "cargo check");
    let _ = f.result("retry", "Exit code ", None);
    f.no_repair();
    f.poll("retry-end", "");
    let proof = f.result("retry-end", "0\n", Some(0));
    let originals = f.db.list_evidence_spans_for_session(&f.session).unwrap();
    let report = f.derive();
    assert_eq!(report.failures_seen, 1);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    let card = crate::core::incident_card::incident_card_id(WS, &failure);
    let recall = error_recall_report(&f.db, WS, &diagnostic()).unwrap();
    assert!(recall.helpful_repairs.contains(&fix));
    assert!(recall.helpful_repairs.contains(&card));
    assert_eq!(recall.proof_links, vec![proof.clone()]);
    let links =
        f.db.list_error_repair_links(WS, &diagnostic().layered_key().key)
            .unwrap();
    assert!(
        links
            .iter()
            .all(|link| link.evidence_ref.as_deref() == Some(failure.as_str()))
    );
    let evidence = recalled_repair_evidence(&f.db, WS, &recall).unwrap();
    assert!(
        evidence
            .iter()
            .any(|item| item.evidence_id == fix && item.text.is_some())
    );
    assert!(
        evidence
            .iter()
            .any(|item| item.evidence_id == proof && item.text.is_none())
    );
    let seed = pack_error_recall_query_seed(&f.db, WS, &diagnostic()).unwrap();
    assert!(seed.contains("prior fix:"));
    assert!(seed.contains("converting"));
    for source in originals {
        let after = f.db.get_evidence_span(&source.id).unwrap().unwrap();
        assert_eq!(after.excerpt, source.excerpt);
        assert_eq!(after.content_hash, source.content_hash);
        assert_eq!(
            after.canonical_provenance_uri(),
            source.canonical_provenance_uri()
        );
    }
    assert_eq!(f.derive().incident_cards_recorded, 0);
    assert_eq!(
        f.db.list_error_repair_links(WS, &diagnostic().layered_key().key)
            .unwrap()
            .len(),
        links.len()
    );
}

#[test]
fn a_false_status_inside_captured_json_cannot_poison_the_verified_retry() {
    let mut f = Fixture::new();
    let _ = f.start_failure(true);
    let _ = f.repair();
    f.call("retry", "cargo check");
    let prefix = "{\"reason\":\"compiler-artifact\",\"filenames\":[\"";
    let tail =
        "error: captured artifact label\"]}\n{\"reason\":\"build-finished\",\"success\":true}\n";
    let _ = f.result("retry", prefix, None);
    f.no_repair();
    f.poll("end", "");
    let proof = f.result("end", tail, Some(0));
    assert_eq!(f.derive().resolved_failures, 1);
    assert_eq!(
        error_recall_report(&f.db, WS, &diagnostic())
            .unwrap()
            .proof_links,
        vec![proof]
    );
}

#[test]
fn invalidated_failed_streams_do_not_gain_authority_from_an_unrelated_completion() {
    for nonempty_stdin in [false, true] {
        let mut f = Fixture::new();
        let _ = f.start_failure(false);
        f.poll("bad", if nonempty_stdin { "continue\n" } else { "" });
        if nonempty_stdin {
            let _ = f.result("bad", "", Some(101));
        } else {
            let _ = f.raw_result(
                "bad",
                "Wall time: 0.1 seconds\nProcess exited with code unknown\nOutput:\n",
            );
        }
        let _ = f.repair();
        f.call("retry", "cargo check");
        let _ = f.result("retry", "Finished dev profile\n", Some(0));
        f.no_repair();
    }
}

#[test]
fn narrative_before_the_failed_invocation_finishes_is_not_credited_to_a_later_retry() {
    let mut f = Fixture::new();
    let _ = f.start_failure(false);
    let premature_fix = f.repair();
    f.poll("failure-end", "");
    let _ = f.result("failure-end", "", Some(101));
    f.call("retry", "cargo check");
    let proof = f.result("retry", "Finished dev profile\n", Some(0));
    let report = f.derive();
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 0);
    let recall = error_recall_report(&f.db, WS, &diagnostic()).unwrap();
    assert!(!recall.helpful_repairs.contains(&premature_fix));
    assert!(recall.helpful_repairs.is_empty());
    assert_eq!(recall.proof_links, vec![proof]);
}

#[test]
fn a_different_streamed_invocation_keeps_the_original_failure_unresolved() {
    let mut f = Fixture::new();
    let _ = f.start_failure(true);
    let _ = f.repair();
    f.call("wrong", "cargo check --all-targets");
    let _ = f.result("wrong", "Exit code ", None);
    f.poll("wrong-end", "");
    let _ = f.result("wrong-end", "0\n", Some(0));
    f.no_repair();
}

#[test]
fn unresolved_failures_do_not_block_an_independent_completed_failure() {
    let mut f = Fixture::new();
    let _ = f.start_failure(false);
    f.call("independent", "cargo check");
    let completed_failure = f.result("independent", ERROR, Some(101));
    let fix = f.repair();
    f.call("retry", "cargo check");
    let proof = f.result("retry", "Finished dev profile\n", Some(0));
    let report = f.derive();
    assert_eq!(report.failures_seen, 2);
    assert_eq!(report.resolved_failures, 1);
    assert_eq!(report.incident_cards_recorded, 1);
    let recall = error_recall_report(&f.db, WS, &diagnostic()).unwrap();
    assert!(recall.helpful_repairs.contains(&fix));
    assert_eq!(recall.proof_links, vec![proof]);
    let links =
        f.db.list_error_repair_links(WS, &diagnostic().layered_key().key)
            .unwrap();
    assert!(
        links
            .iter()
            .all(|link| link.evidence_ref.as_deref() == Some(completed_failure.as_str()))
    );
}
