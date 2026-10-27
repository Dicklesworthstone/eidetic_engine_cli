//! Exercise historical claims against the current interpreter and real store.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::cass_error_recall::{
    CommandFamily, FailureArc, Resolution, record_session_error_recall,
    structured_error_diagnostics,
};
use crate::core::error_diagnosis::{
    ErrorRepairLinkRecording, error_recall_report, pack_error_recall_query_seed,
    recalled_repair_evidence, record_error_repair_links,
};
use crate::core::error_recall::CanonicalDiagnostic;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbError, DbOperation,
    EvidenceProducerKind,
};
use serde_json::{Value as Json, json};

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const OTHER: &str = "wsp_01234567890123456789012346";
const COMMAND: &str = "go test -json ./...";
const REPAIR: &str = "The cache test failed because writes were not flushed. I fixed the cache lookup by flushing pending writes before reads in src/cache.go.";

struct Fixture {
    db: DbConnection,
    session: String,
    attempt: String,
    failure: String,
    repair: String,
    proof: String,
    diagnostic: CanonicalDiagnostic,
}

fn go(results: &[(&str, &str)]) -> String {
    let mut rows = vec![json!({"Action":"start","Package":"example.org/cache"})];
    for (name, status) in results {
        rows.push(json!({"Action":"run","Package":"example.org/cache","Test":name}));
        rows.push(json!({"Action":status,"Package":"example.org/cache","Test":name}));
    }
    rows.push(json!({"Action":if results.iter().any(|(_, status)| *status == "fail") {"fail"} else {"pass"},"Package":"example.org/cache"}));
    rows.iter()
        .map(Json::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

impl Fixture {
    fn new(valid_retry: bool) -> TestResult<Self> {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        for (workspace, path) in [
            (WS, "/tmp/revalidate-recall"),
            (OTHER, "/tmp/revalidate-other"),
        ] {
            db.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )?;
        }
        let session =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x7600)).to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "/sessions/revalidate.jsonl".to_owned(),
                source_path: None,
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 5,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"revalidate").to_hex()),
                metadata_json: None,
            },
        )?;
        let failed = go(&[("TestCache", "fail"), ("TestControl", "pass")]);
        let passed = if valid_retry {
            go(&[
                ("TestCache", "pass"),
                ("TestControl", "pass"),
                ("TestRegression", "pass"),
            ])
        } else {
            go(&[
                ("TestCache", "pass"),
                ("TestControl", "skip"),
                ("TestRegression", "pass"),
            ])
        };
        let diagnostic = structured_error_diagnostics(&failed)
            .into_iter()
            .next()
            .ok_or("diagnostic")?;
        let id = |line: u32| {
            EvidenceId::from_uuid(uuid::Uuid::from_u128(0x7700 + u128::from(line))).to_string()
        };
        let rows = [
            (
                "tool_call",
                "assistant",
                json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":COMMAND}}),
            ),
            (
                "tool_result",
                "user",
                json!({"type":"tool_result","tool_use_id":"fail","content":failed,"is_error":true}),
            ),
            ("message", "assistant", json!({"type":"text","text":REPAIR})),
            (
                "tool_call",
                "assistant",
                json!({"type":"tool_use","id":"retry","name":"Bash","input":{"command":COMMAND}}),
            ),
            (
                "tool_result",
                "user",
                json!({"type":"tool_result","tool_use_id":"retry","content":passed,"is_error":false}),
            ),
        ];
        for (index, (kind, role, block)) in rows.into_iter().enumerate() {
            let line = u32::try_from(index + 1)?;
            let excerpt =
                json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
            db.insert_evidence_span(
                &id(line),
                &CreateEvidenceSpanInput {
                    workspace_id: WS.to_owned(),
                    session_id: session.clone(),
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
            )?;
        }
        Ok(Self {
            db,
            session,
            attempt: id(1),
            failure: id(2),
            repair: id(3),
            proof: id(5),
            diagnostic,
        })
    }

    fn historical(&self, actor: Option<&str>, failure: &str) -> TestResult {
        record_error_repair_links(
            &self.db,
            WS,
            &self.diagnostic,
            &ErrorRepairLinkRecording {
                helpful_repairs: vec![self.repair.clone()],
                proof_links: vec![self.proof.clone()],
                created_by: actor.map(str::to_owned),
                evidence_ref: Some(failure.to_owned()),
                ..ErrorRepairLinkRecording::default()
            },
        )?;
        Ok(())
    }

    fn old_card(&self) -> TestResult<String> {
        // Simulate a card written by a former policy that accepted a skipped
        // control. The new interpreter is never weakened to create this row.
        let arc = FailureArc {
            failure_id: self.failure.clone(),
            attempt_id: self.attempt.clone(),
            family: CommandFamily::parse(COMMAND).ok_or("family")?,
            symptom: Some("The cache test failed because writes were not flushed.".to_owned()),
            diagnostics: vec![self.diagnostic.clone()],
            resolution: Some(Resolution {
                proof_id: self.proof.clone(),
                proof_family: CommandFamily::parse(COMMAND).ok_or("family")?,
                repair_ids: vec![self.repair.clone()],
            }),
        };
        let spans = self.db.list_evidence_spans_for_session(&self.session)?;
        let card = draft_incident_card(WS, &self.session, &spans, &arc).ok_or("card")?;
        self.db.insert_evidence_span(&card.id, &card.input)?;
        record_error_repair_links(
            &self.db,
            WS,
            &self.diagnostic,
            &ErrorRepairLinkRecording {
                helpful_repairs: vec![card.id.clone()],
                created_by: Some(INCIDENT_CARD_ACTOR.to_owned()),
                evidence_ref: Some(self.failure.clone()),
                ..ErrorRepairLinkRecording::default()
            },
        )?;
        Ok(card.id)
    }

    fn history(&self) -> TestResult<Vec<(String, String, String, Option<String>, Option<String>)>> {
        Ok(self
            .db
            .list_error_repair_links(WS, &self.diagnostic.layered_key().key)?
            .into_iter()
            .map(|link| {
                (
                    link.link_id,
                    link.target_id,
                    link.outcome,
                    link.evidence_ref,
                    link.created_by,
                )
            })
            .collect())
    }
}

#[test]
fn historical_skipped_control_cannot_supply_repair_or_card_text_but_history_survives() -> TestResult
{
    let fixture = Fixture::new(false)?;
    fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &fixture.failure)?;
    let card = fixture.old_card()?;
    let history = fixture.history()?;
    let sources = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    let report = error_recall_report(&fixture.db, WS, &fixture.diagnostic)?;
    assert!(report.helpful_repairs.contains(&fixture.repair));
    assert!(report.helpful_repairs.contains(&card));
    let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].role, "proof");
    assert_eq!(evidence[0].text, None);
    assert!(
        !pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?.contains("prior fix:")
    );
    assert_eq!(fixture.history()?, history);
    for source in sources {
        let after = fixture
            .db
            .get_evidence_span(&source.id)?
            .ok_or("missing source")?;
        assert_eq!(after.excerpt, source.excerpt);
        assert_eq!(after.content_hash, source.content_hash);
        assert_eq!(after.metadata_json, source.metadata_json);
    }
    Ok(())
}

#[test]
fn current_valid_repair_keeps_cited_card_and_pack_context_without_rederivation() -> TestResult {
    let fixture = Fixture::new(true)?;
    let written = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(written.resolved_failures, 1);
    assert_eq!(written.incident_cards_recorded, 1);
    let history = fixture.history()?;
    let report = error_recall_report(&fixture.db, WS, &fixture.diagnostic)?;
    let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert_eq!(evidence.len(), 3);
    assert_eq!(evidence[0].role, "incident_card");
    assert!(
        evidence
            .iter()
            .any(|item| item.evidence_id == fixture.repair && item.text.is_some())
    );
    assert!(
        pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?.contains("prior fix:")
    );
    assert_eq!(fixture.history()?, history);
    Ok(())
}

#[test]
fn cached_positive_report_loses_authority_when_its_failure_source_is_withheld() -> TestResult {
    let fixture = Fixture::new(true)?;
    record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    let report = error_recall_report(&fixture.db, WS, &fixture.diagnostic)?;
    fixture.db.execute_raw(&format!(
        "UPDATE evidence_spans SET excerpt = 'unverifiable changed source' WHERE id = '{}'",
        fixture.failure
    ))?;
    assert!(
        recalled_repair_evidence(&fixture.db, WS, &report)?
            .iter()
            .all(|item| item.text.is_none())
    );
    assert!(
        !pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?.contains("prior fix:")
    );
    Ok(())
}

#[test]
fn wrong_fingerprint_missing_failure_and_aliases_do_not_borrow_another_valid_arc() -> TestResult {
    for failure in [
        "missing".to_owned(),
        format!(
            "{}\0alias",
            EvidenceId::from_uuid(uuid::Uuid::from_u128(0x7702))
        ),
    ] {
        let fixture = Fixture::new(true)?;
        fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &failure)?;
        assert!(
            !pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?
                .contains("prior fix:")
        );
    }
    let fixture = Fixture::new(true)?;
    let unrelated = crate::core::error_recall::from_rustc(Some("E0277"), "trait bound failed");
    record_error_repair_links(
        &fixture.db,
        WS,
        &unrelated,
        &ErrorRepairLinkRecording {
            helpful_repairs: vec![fixture.repair.clone()],
            created_by: Some(CASS_ERROR_RECALL_ACTOR.to_owned()),
            evidence_ref: Some(fixture.failure.clone()),
            ..ErrorRepairLinkRecording::default()
        },
    )?;
    assert!(!pack_error_recall_query_seed(&fixture.db, WS, &unrelated)?.contains("prior fix:"));
    Ok(())
}

#[test]
fn manual_feedback_remains_independent_and_harmful_observations_still_win() -> TestResult {
    let fixture = Fixture::new(false)?;
    fixture.historical(Some("human review"), &fixture.failure)?;
    assert!(
        pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?.contains("prior fix:")
    );
    record_error_repair_links(
        &fixture.db,
        WS,
        &fixture.diagnostic,
        &ErrorRepairLinkRecording {
            harmful_repairs: vec![fixture.repair.clone()],
            created_by: Some("human review".to_owned()),
            ..ErrorRepairLinkRecording::default()
        },
    )?;
    assert!(
        !pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?.contains("prior fix:")
    );
    Ok(())
}

#[test]
fn changed_card_metadata_does_not_replace_an_unchanged_verified_turn() -> TestResult {
    let fixture = Fixture::new(true)?;
    record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    let card = crate::core::incident_card::incident_card_id(WS, &fixture.failure);
    fixture.db.execute_raw(&format!(
        "UPDATE evidence_spans SET metadata_json = '{{}}' WHERE id = '{card}'"
    ))?;
    let report = error_recall_report(&fixture.db, WS, &fixture.diagnostic)?;
    let evidence = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert!(!evidence.iter().any(|item| item.evidence_id == card));
    assert!(
        evidence
            .iter()
            .any(|item| item.evidence_id == fixture.repair && item.text.is_some())
    );
    Ok(())
}

#[test]
fn foreign_session_ownership_and_over_budget_sources_withhold_auto_claims() -> TestResult {
    let fixture = Fixture::new(true)?;
    fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &fixture.failure)?;
    fixture.db.execute_raw(&format!(
        "UPDATE sessions SET workspace_id = '{OTHER}' WHERE id = '{}'",
        fixture.session
    ))?;
    assert!(
        !pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?.contains("prior fix:")
    );
    let fixture = Fixture::new(true)?;
    fixture.historical(Some(CASS_ERROR_RECALL_ACTOR), &fixture.failure)?;
    fixture.db.execute_raw(&format!(
        "UPDATE evidence_spans SET metadata_json = '{}' WHERE id = '{}'",
        "x".repeat(MAX_SOURCE_BYTES + 1),
        fixture.proof
    ))?;
    assert!(
        !pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?.contains("prior fix:")
    );
    Ok(())
}

#[test]
fn source_reads_borrow_the_callers_transaction_without_committing_or_rolling_it_back() -> TestResult
{
    let fixture = Fixture::new(true)?;
    record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    let report = error_recall_report(&fixture.db, WS, &fixture.diagnostic)?;
    let result: Result<()> = fixture.db.with_transaction(|| {
        fixture.db.execute_raw(&format!(
            "UPDATE sessions SET model = 'caller-owned' WHERE id = '{}'",
            fixture.session
        ))?;
        assert!(
            recalled_repair_evidence(&fixture.db, WS, &report)?
                .iter()
                .any(|item| item.text.is_some())
        );
        assert!(
            pack_error_recall_query_seed(&fixture.db, WS, &fixture.diagnostic)?
                .contains("prior fix:")
        );
        Err(DbError::MalformedRow {
            operation: DbOperation::Query,
            message: "rollback requested by caller".to_owned(),
        })
    });
    assert!(result.is_err());
    assert_eq!(
        fixture
            .db
            .get_session(&fixture.session)?
            .ok_or("session")?
            .model,
        None
    );
    fixture.db.begin_read_snapshot()?;
    assert!(
        recalled_repair_evidence(&fixture.db, WS, &report)?
            .iter()
            .any(|item| item.text.is_some())
    );
    fixture.db.rollback_read_snapshot()?;
    Ok(())
}

#[path = "error_recall_revalidate_paging_tests.rs"]
mod paging;

#[path = "error_recall_revalidate_batch_tests.rs"]
mod batching;
