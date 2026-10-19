//! Repair attribution through migrated storage and current-source readers.
//!
//! All transcript rows are ingested normally. Negative controls do not edit
//! hashes or break admission to manufacture a refusal. Successful command
//! proof remains available even when no explanation deserves repair credit.

use crate::core::cass_error_recall::{CASS_ERROR_RECALL_ACTOR, record_session_error_recall};
use crate::core::error_diagnosis::{
    ErrorRepairLinkRecording, error_recall_report, pack_error_recall_query_seed,
    recalled_repair_evidence, record_error_repair_links,
};
use crate::core::error_recall::from_rustc;
use crate::core::incident_card::{incident_card_id, proof::CardChecks};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "cargo check";
const ERROR: &str = "error[E0277]: the trait bound `Widget: Serialize` is not satisfied\n --> src/widget.rs:3:5\n";
const FIX: &str = "I fixed E0277 by deriving Serialize for Widget before writing it to the serialized store in src/widget.rs.";

struct Fixture {
    db: DbConnection,
    session: String,
    failure: String,
    repairs: Vec<String>,
    proof: String,
    line: u32,
}

impl Fixture {
    fn new(repairs: &[&str]) -> TestResult<Self> {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        db.insert_workspace(
            WS,
            &CreateWorkspaceInput {
                path: "/tmp/repair-attribution".to_owned(),
                name: None,
            },
        )?;
        let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x760100))
            .to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "/sessions/repair-attribution.jsonl".to_owned(),
                source_path: None,
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 0,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"repair-attribution").to_hex()),
                metadata_json: None,
            },
        )?;
        let mut fixture = Self {
            db,
            session,
            failure: String::new(),
            repairs: Vec::new(),
            proof: String::new(),
            line: 0,
        };
        fixture.add("tool_call", "assistant", json!({
            "type":"tool_use","id":"failed","name":"Bash","input":{"command":COMMAND}
        }))?;
        fixture.failure = fixture.add("tool_result", "user", json!({
            "type":"tool_result","tool_use_id":"failed","content":ERROR,"is_error":true
        }))?;
        for text in repairs {
            let id = fixture.add("message", "assistant", json!({"type":"text","text":text}))?;
            fixture.repairs.push(id);
        }
        fixture.add("tool_call", "assistant", json!({
            "type":"tool_use","id":"verified","name":"Bash","input":{"command":COMMAND}
        }))?;
        fixture.proof = fixture.add("tool_result", "user", json!({
            "type":"tool_result","tool_use_id":"verified","content":"","is_error":false
        }))?;
        Ok(fixture)
    }

    fn add(&mut self, kind: &str, role: &str, block: Value) -> TestResult<String> {
        self.line += 1;
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x760200 + u128::from(self.line),
        ))
        .to_string();
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        self.db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: self.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{}:{}", self.session, self.line),
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
        )?;
        Ok(id)
    }
}

#[test]
fn a_successful_retry_does_not_turn_admitted_plans_into_helpful_repairs() -> TestResult {
    for text in [
        "I will fix E0277 by adding Serialize for Widget.",
        "I have not fixed E0277 in src/widget.rs.",
        "Adding Serialize may fix E0277 for Widget.",
        "I think I fixed E0277 by changing Widget.",
        "> I fixed E0277 by deriving Serialize for Widget.",
        "~~~text\nI fixed E0277 by deriving Serialize for Widget.\n~~~",
    ] {
        let fixture = Fixture::new(&[text])?;
        assert!(fixture.db.get_search_admitted_evidence_span(&fixture.repairs[0], WS)?.is_some());
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1, "{text}");
        assert_eq!(report.resolved_failures, 1, "the command itself succeeded");
        assert_eq!(report.repair_links_recorded, 1, "only the real proof is recorded");
        assert_eq!(report.incident_cards_recorded, 0, "{text}");
        let recall = error_recall_report(&fixture.db, WS, &from_rustc(Some("E0277"), "trait bound"))?;
        assert!(recall.exact);
        assert!(recall.helpful_repairs.is_empty(), "{text}");
        assert_eq!(recall.proof_links, vec![fixture.proof]);
    }
    Ok(())
}

#[test]
fn recent_plans_do_not_crowd_a_real_explanation_out_of_the_two_turn_budget() -> TestResult {
    let fixture = Fixture::new(&[
        FIX,
        "Let me change Widget again to fix a different E0277 tomorrow.",
        "I will add another Serialize implementation next.",
        "Maybe I should update Widget in src/widget.rs.",
    ])?;
    let before = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
    let first = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(first.resolved_failures, 1);
    assert_eq!(first.repair_links_recorded, 2);
    assert_eq!(first.incident_cards_recorded, 1);
    let diagnostic = from_rustc(Some("E0277"), "trait bound");
    let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
    assert!(recall.helpful_repairs.contains(&fixture.repairs[0]));
    for excluded in &fixture.repairs[1..] {
        assert!(!recall.helpful_repairs.contains(excluded));
    }
    let card_id = incident_card_id(WS, &fixture.failure);
    let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
    assert!(card.reader_body().contains("deriving Serialize"));
    assert!(!card.reader_body().contains("tomorrow"));
    assert!(!card.reader_body().contains("Maybe"));
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&fixture.db, WS)?.is_empty());
    assert!(pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?.contains("prior fix:"));
    for original in before {
        let current = fixture.db.get_evidence_span(&original.id)?.ok_or("original")?;
        assert_eq!(current.excerpt, original.excerpt);
        assert_eq!(current.content_hash, original.content_hash);
        assert_eq!(current.metadata_json, original.metadata_json);
        assert_eq!(current.canonical_provenance_uri(), original.canonical_provenance_uri());
    }
    let second = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(second.incident_cards_recorded, 0);
    assert_eq!(fixture.db.list_error_repair_links(WS, &diagnostic.layered_key().key)?.len(), 3);
    Ok(())
}

#[test]
fn historical_cass_helpful_links_are_withheld_live_without_erasing_history() -> TestResult {
    let fixture = Fixture::new(&["I will fix E0277 by adding Serialize for Widget."])?;
    let diagnostic = from_rustc(Some("E0277"), "trait bound");
    let stored = record_error_repair_links(
        &fixture.db,
        WS,
        &diagnostic,
        &ErrorRepairLinkRecording {
            helpful_repairs: fixture.repairs.clone(),
            proof_links: vec![fixture.proof.clone()],
            created_by: Some(CASS_ERROR_RECALL_ACTOR.to_owned()),
            evidence_ref: Some(fixture.failure.clone()),
            ..ErrorRepairLinkRecording::default()
        },
    )?;
    assert_eq!(stored.len(), 2);
    let report = error_recall_report(&fixture.db, WS, &diagnostic)?;
    assert!(report.helpful_repairs.contains(&fixture.repairs[0]), "history remains inspectable");
    let exposed = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert!(exposed.iter().all(|item| item.role == "proof"));
    assert_eq!(exposed.len(), 1);
    assert_eq!(exposed[0].evidence_id, fixture.proof);
    assert!(exposed[0].text.is_none());
    let query = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
    assert!(!query.contains("prior fix:"));
    assert!(!query.contains("I will fix"));
    assert_eq!(fixture.db.list_error_repair_links(WS, &diagnostic.layered_key().key)?.len(), 2);
    assert!(fixture.db.get_evidence_span(&fixture.repairs[0])?.is_some());
    Ok(())
}

#[test]
fn explicit_manual_feedback_is_not_reclassified_as_an_automatic_cass_claim() -> TestResult {
    let fixture = Fixture::new(&["Adding Serialize may fix E0277 for Widget."])?;
    let diagnostic = from_rustc(Some("E0277"), "trait bound");
    record_error_repair_links(
        &fixture.db,
        WS,
        &diagnostic,
        &ErrorRepairLinkRecording {
            helpful_repairs: fixture.repairs.clone(),
            created_by: Some("explicit-human-review".to_owned()),
            ..ErrorRepairLinkRecording::default()
        },
    )?;
    let report = error_recall_report(&fixture.db, WS, &diagnostic)?;
    let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert!(evidence.iter().any(|item| {
        item.role == "repair" && item.evidence_id == fixture.repairs[0]
    }));
    Ok(())
}
