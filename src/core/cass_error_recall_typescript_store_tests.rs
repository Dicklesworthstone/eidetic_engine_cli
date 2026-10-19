//! Migrated-store TypeScript recall, current-source proof and streamed intake.

use crate::core::cass_error_recall::{record_session_error_recall, structured_error_diagnostics};
use crate::core::error_diagnosis::{error_recall_report, pack_error_recall_query_seed};
use crate::core::incident_card::{incident_card_id, proof::CardChecks};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use serde_json::{Value, json};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;
type TestResult = Fallible<()>;
const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "tsc --noEmit -p tsconfig.json";
const CWD: &str = "/tmp/typescript-recall";
const PLAIN: &str = "src/widget.ts(7,9): error TS2322: Type 'string' is not assignable to type 'number'.";
const REPAIR: &str = "I fixed TS2322 by converting the widget input to a number before assigning it to the numeric field in src/widget.ts.";

fn colored() -> String {
    "\u{1b}[96msrc/widget.ts\u{1b}[0m:\u{1b}[93m7\u{1b}[0m:\u{1b}[93m9\u{1b}[0m - \u{1b}[91merror\u{1b}[0m\u{1b}[90m TS2322: \u{1b}[0mType 'string' is not assignable to type 'number'.\n\n\u{1b}[7m7\u{1b}[0m const value: number = 'wrong';\n          \u{1b}[91m~~~~~\u{1b}[0m\n\nFound 1 error in src/widget.ts\u{1b}[90m:7\u{1b}[0m\n".to_owned()
}

struct Fixture {
    db: DbConnection,
    session: String,
    line: u32,
}

impl Fixture {
    fn new() -> Fallible<Self> {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        db.insert_workspace(
            WS,
            &CreateWorkspaceInput {
                path: CWD.to_owned(),
                name: None,
            },
        )?;
        let session =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x740100)).to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "/sessions/typescript-recall.jsonl".to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 0,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"typescript-recall").to_hex()),
                metadata_json: None,
            },
        )?;
        Ok(Self {
            db,
            session,
            line: 0,
        })
    }

    fn add(&mut self, kind: &str, role: &str, value: Value) -> Fallible<String> {
        self.line += 1;
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x740200 + u128::from(self.line),
        ))
        .to_string();
        let excerpt = value.to_string();
        self.db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: self.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("typescript:{}", self.line),
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

    fn call(&mut self, id: &str, command: &str, cwd: &str) -> Fallible<String> {
        self.add(
            "tool_call",
            "assistant",
            json!({"type":"response_item","payload":{
                "type":"function_call","name":"exec_command","call_id":id,
                "arguments":json!({"cmd":command,"workdir":cwd}).to_string()
            }}),
        )
    }

    fn poll(&mut self, id: &str) -> Fallible<String> {
        self.add(
            "tool_call",
            "assistant",
            json!({"type":"response_item","payload":{
                "type":"function_call","name":"write_stdin","call_id":id,
                "arguments":json!({"session_id":42,"chars":""}).to_string()
            }}),
        )
    }

    fn result(&mut self, id: &str, body: &str, code: Option<i32>) -> Fallible<String> {
        let status = code.map_or_else(
            || "Process running with session ID 42".to_owned(),
            |code| format!("Process exited with code {code}"),
        );
        self.add(
            "tool_result",
            "tool",
            json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":id,
                "output":format!("Wall time: 0.01 seconds\n{status}\nOutput:\n{body}")
            }}),
        )
    }

    fn repair(&mut self) -> Fallible<String> {
        self.add(
            "message",
            "assistant",
            json!({
                "type":"assistant","message":{"role":"assistant","content":REPAIR}
            }),
        )
    }
}

#[test]
fn imported_plain_and_pretty_errors_have_cited_idempotent_packable_repairs() -> TestResult {
    for failure in [PLAIN.to_owned(), colored()] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        let failed_id = fixture.result("fail", &failure, Some(2))?;
        let repair_id = fixture.repair()?;
        fixture.call("verify", COMMAND, CWD)?;
        let proof_id = fixture.result("verify", "", Some(0))?;
        let before = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.fingerprints_recorded, 1);
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.repair_links_recorded, 2);
        assert_eq!(report.incident_cards_recorded, 1);
        let diagnostic = structured_error_diagnostics(PLAIN)
            .into_iter()
            .next()
            .ok_or("diagnostic")?;
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.exact);
        assert_eq!(recall.proof_links, vec![proof_id]);
        assert!(recall.helpful_repairs.contains(&repair_id));
        let card_id = incident_card_id(WS, &failed_id);
        assert!(recall.helpful_repairs.contains(&card_id));
        let card = fixture.db.get_evidence_span(&card_id)?.ok_or("card")?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        assert!(checks.withheld(&fixture.db, WS)?.is_empty());
        let seed = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
        assert!(seed.contains("prior fix:"));
        assert!(seed.contains("converting"));
        assert!(!seed.contains("const value"));
        assert!(!seed.contains('\u{1b}'));
        let links = fixture
            .db
            .list_error_repair_links(WS, &diagnostic.layered_key().key)?;
        assert_eq!(links.len(), 3);
        assert!(
            links
                .iter()
                .all(|link| link.evidence_ref.as_deref() == Some(failed_id.as_str()))
        );
        for original in before {
            let after = fixture
                .db
                .get_evidence_span(&original.id)?
                .ok_or("original")?;
            assert_eq!(original.excerpt, after.excerpt);
            assert_eq!(original.content_hash, after.content_hash);
            assert_eq!(original.metadata_json, after.metadata_json);
            assert_eq!(
                original.canonical_provenance_uri(),
                after.canonical_provenance_uri()
            );
        }
        let second = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(second.incident_cards_recorded, 0);
        assert_eq!(
            fixture
                .db
                .list_error_repair_links(WS, &diagnostic.layered_key().key)?
                .len(),
            3
        );
    }
    Ok(())
}

#[test]
fn a_different_project_or_working_directory_cannot_verify_a_typescript_repair() -> TestResult {
    for (command, cwd) in [
        ("tsc --noEmit -p other.json", CWD),
        (COMMAND, "/tmp/another-project"),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", PLAIN, Some(2))?;
        fixture.repair()?;
        fixture.call("verify", command, cwd)?;
        fixture.result("verify", "", Some(0))?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
    }
    Ok(())
}

#[test]
fn malformed_or_still_failing_optimistic_retries_never_write_helpful_repairs() -> TestResult {
    for retry in [
        colored(),
        colored().replace("Found 1 error in src/widget.ts", "Found 0 errors in src/widget.ts"),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", PLAIN, Some(2))?;
        fixture.repair()?;
        fixture.call("verify", COMMAND, CWD)?;
        fixture.result("verify", &retry, Some(0))?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.repair_links_recorded, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        let diagnostic = structured_error_diagnostics(PLAIN)
            .into_iter()
            .next()
            .ok_or("diagnostic")?;
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
    }
    Ok(())
}

#[test]
fn poll_boundaries_inside_color_coordinates_and_error_codes_preserve_repair_attribution()
-> TestResult {
    let failure = colored();
    let marker = failure.find("TS2322").ok_or("error code")?;
    for split in [1, 5, 28, marker + 3] {
        let mut fixture = Fixture::new()?;
        fixture.call("fail", COMMAND, CWD)?;
        fixture.result("fail", &failure[..split], None)?;
        fixture.poll("finish")?;
        fixture.result("finish", &failure[split..], Some(2))?;
        fixture.repair()?;
        fixture.call("verify", COMMAND, CWD)?;
        let proof = fixture.result("verify", "", Some(0))?;
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1, "split {split}");
        assert_eq!(report.resolved_failures, 1, "split {split}");
        assert_eq!(report.incident_cards_recorded, 1, "split {split}");
        let diagnostic = structured_error_diagnostics(PLAIN)
            .into_iter()
            .next()
            .ok_or("diagnostic")?;
        assert_eq!(
            error_recall_report(&fixture.db, WS, &diagnostic)?.proof_links,
            vec![proof]
        );
    }
    Ok(())
}

#[test]
fn a_stored_card_loses_current_source_proof_when_the_retry_changes_to_failure() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.call("fail", COMMAND, CWD)?;
    let failure = fixture.result("fail", PLAIN, Some(2))?;
    fixture.repair()?;
    fixture.call("verify", COMMAND, CWD)?;
    let proof = fixture.result("verify", "", Some(0))?;
    assert_eq!(
        record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded,
        1
    );
    let card = fixture
        .db
        .get_evidence_span(&incident_card_id(WS, &failure))?
        .ok_or("card")?;
    let source = fixture.db.get_evidence_span(&proof)?.ok_or("proof source")?;
    let mut record: Value = serde_json::from_str(&source.excerpt)?;
    record["payload"]["output"] = json!(format!(
        "Wall time: 0.01 seconds\nProcess exited with code 2\nOutput:\n{}",
        colored()
    ));
    let revised = record.to_string().replace('\'', "''");
    fixture.db.execute_raw(&format!(
        "UPDATE evidence_spans SET excerpt = '{revised}' WHERE id = '{proof}'"
    ))?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    assert!(checks.withheld(&fixture.db, WS)?.contains(&card.id));
    Ok(())
}
