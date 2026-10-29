//! Repair attribution through migrated storage and current-source readers.
//!
//! All transcript rows are ingested normally. Negative controls do not edit
//! hashes or break admission to manufacture a refusal. Successful command
//! proof remains available even when no explanation deserves repair credit.

use crate::core::cass_error_recall::{
    CASS_ERROR_RECALL_ACTOR, record_session_error_recall, session_failure_arcs,
};
use crate::core::error_diagnosis::{
    ErrorRepairLinkRecording, error_recall_report, pack_error_recall_query_seed,
    recalled_repair_evidence, record_error_repair_links,
};
use crate::core::error_recall::from_rustc;
use crate::core::incident_card::{
    INCIDENT_CARD_ACTOR, IncidentCardDraft, belongs_to_incident, draft_incident_card,
    incident_card_id, proof::CardChecks,
};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind, StoredEvidenceSpan,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const COMMAND: &str = "cargo check";
const ERROR: &str =
    "error[E0277]: the trait bound `Widget: Serialize` is not satisfied\n --> src/widget.rs:3:5\n";
const FIX: &str = "I fixed E0277 by deriving Serialize for Widget before writing it to the serialized store in src/widget.rs.";

struct Fixture {
    db: DbConnection,
    session: String,
    attempt: String,
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
        let session =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x760100)).to_string();
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
            attempt: String::new(),
            failure: String::new(),
            repairs: Vec::new(),
            proof: String::new(),
            line: 0,
        };
        fixture.attempt = fixture.add(
            "tool_call",
            "assistant",
            json!({
                "type":"tool_use","id":"failed","name":"Bash","input":{"command":COMMAND}
            }),
        )?;
        fixture.failure = fixture.add(
            "tool_result",
            "user",
            json!({
                "type":"tool_result","tool_use_id":"failed","content":ERROR,"is_error":true
            }),
        )?;
        for text in repairs {
            let id = fixture.add("message", "assistant", json!({"type":"text","text":text}))?;
            fixture.repairs.push(id);
        }
        fixture.add(
            "tool_call",
            "assistant",
            json!({
                "type":"tool_use","id":"verified","name":"Bash","input":{"command":COMMAND}
            }),
        )?;
        fixture.proof = fixture.add(
            "tool_result",
            "user",
            json!({
                "type":"tool_result","tool_use_id":"verified","content":"","is_error":false
            }),
        )?;
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

    fn draft(&self) -> TestResult<IncidentCardDraft> {
        let session = self.db.get_session(&self.session)?.ok_or("session")?;
        let spans = self
            .db
            .list_evidence_spans_for_session(&self.session)?
            .into_iter()
            .filter(|span| !crate::core::incident_card::proof::requires_proof(span))
            .collect::<Vec<_>>();
        let arc = session_failure_arcs(WS, &session, &spans)
            .into_iter()
            .next()
            .ok_or("arc")?;
        draft_incident_card(WS, &self.session, &spans, &arc).ok_or("draft".into())
    }

    fn older_card(&self, provenance_only: bool) -> TestResult<StoredEvidenceSpan> {
        let mut old = self.draft()?;
        if provenance_only {
            let mut metadata: Value =
                serde_json::from_str(old.input.metadata_json.as_deref().ok_or("metadata")?)?;
            metadata["previousRenderer"] = true.into();
            old.input.metadata_json = Some(metadata.to_string());
        } else {
            old.input.excerpt.push_str("\nEarlier rendering.");
            old.input.content_hash = format!(
                "blake3:{}",
                blake3::hash(old.input.excerpt.as_bytes()).to_hex()
            );
        }
        self.db.insert_evidence_span(&old.id, &old.input)?;
        record_error_repair_links(
            &self.db,
            WS,
            &from_rustc(Some("E0277"), "trait bound"),
            &ErrorRepairLinkRecording {
                helpful_repairs: vec![old.id.clone()],
                created_by: Some(INCIDENT_CARD_ACTOR.to_owned()),
                evidence_ref: Some(self.failure.clone()),
                ..ErrorRepairLinkRecording::default()
            },
        )?;
        self.db
            .get_evidence_span(&old.id)?
            .ok_or("older card".into())
    }

    fn older_revision(&self) -> TestResult<StoredEvidenceSpan> {
        let mut old = self.draft()?;
        old.input.excerpt.push_str("\nIntermediate rendering.");
        old.input.content_hash = format!(
            "blake3:{}",
            blake3::hash(old.input.excerpt.as_bytes()).to_hex()
        );
        let old = old.revision_draft();
        self.db.insert_evidence_span(&old.id, &old.input)?;
        self.db
            .get_evidence_span(&old.id)?
            .ok_or("older revision".into())
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
        assert!(
            fixture
                .db
                .get_search_admitted_evidence_span(&fixture.repairs[0], WS)?
                .is_some()
        );
        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.failures_seen, 1, "{text}");
        assert_eq!(report.resolved_failures, 1, "the command itself succeeded");
        assert_eq!(
            report.repair_links_recorded, 1,
            "only the real proof is recorded"
        );
        assert_eq!(report.incident_cards_recorded, 0, "{text}");
        let recall =
            error_recall_report(&fixture.db, WS, &from_rustc(Some("E0277"), "trait bound"))?;
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
    let before = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
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
        let current = fixture
            .db
            .get_evidence_span(&original.id)?
            .ok_or("original")?;
        assert_eq!(current.excerpt, original.excerpt);
        assert_eq!(current.content_hash, original.content_hash);
        assert_eq!(current.metadata_json, original.metadata_json);
        assert_eq!(
            current.canonical_provenance_uri(),
            original.canonical_provenance_uri()
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
    assert!(
        report.helpful_repairs.contains(&fixture.repairs[0]),
        "history remains inspectable"
    );
    let exposed = recalled_repair_evidence(&fixture.db, WS, &report)?;
    assert!(exposed.iter().all(|item| item.role == "proof"));
    assert_eq!(exposed.len(), 1);
    assert_eq!(exposed[0].evidence_id, fixture.proof);
    assert!(exposed[0].text.is_none());
    let query = pack_error_recall_query_seed(&fixture.db, WS, &diagnostic)?;
    assert!(!query.contains("prior fix:"));
    assert!(!query.contains("I will fix"));
    assert_eq!(
        fixture
            .db
            .list_error_repair_links(WS, &diagnostic.layered_key().key)?
            .len(),
        2
    );
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
    assert!(
        evidence
            .iter()
            .any(|item| { item.role == "repair" && item.evidence_id == fixture.repairs[0] })
    );
    Ok(())
}

#[test]
fn stale_cards_gain_one_immutable_current_revision_with_exact_source_membership() -> TestResult {
    for provenance_only in [false, true] {
        let fixture = Fixture::new(&[FIX])?;
        let old = fixture.older_card(provenance_only)?;
        let original_rows = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        let expected = fixture.draft()?.revision_draft();
        let mut checks = CardChecks::default();
        checks.observe(&old)?;
        assert_eq!(
            checks.withheld(&fixture.db, WS)?,
            BTreeSet::from([old.id.clone()])
        );

        let report = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(report.incident_cards_recorded, 1);
        let current = fixture
            .db
            .get_evidence_span(&expected.id)?
            .ok_or("current card")?;
        assert_ne!(current.id, old.id);
        assert_ne!(current.pack_entity_revision(), old.pack_entity_revision());
        assert!(current.matches_screened_input(&expected.input)?);
        assert!(belongs_to_incident(
            &current,
            WS,
            &fixture.session,
            &fixture.failure
        ));
        assert!(!belongs_to_incident(
            &current,
            WS,
            &fixture.session,
            &fixture.proof
        ));
        assert!(
            fixture
                .db
                .get_search_admitted_evidence_span(&current.id, WS)?
                .is_some()
        );
        for original in &original_rows {
            assert_eq!(
                fixture.db.get_evidence_span(&original.id)?.as_ref(),
                Some(original)
            );
        }
        let mut checks = CardChecks::default();
        checks.observe(&old)?;
        checks.observe(&current)?;
        assert_eq!(
            checks.withheld(&fixture.db, WS)?,
            BTreeSet::from([old.id.clone()])
        );
        let mut checks = CardChecks::default();
        checks.observe(&old)?;
        checks.observe(&current)?;
        let sources = checks.verified_source_ids(&fixture.db, WS)?;
        assert_eq!(sources.len(), 1);
        let expected_sources = [&fixture.attempt, &fixture.failure, &fixture.proof]
            .into_iter()
            .chain(&fixture.repairs)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(sources.get(&current.id), Some(&expected_sources));
        let diagnostic = from_rustc(Some("E0277"), "trait bound");
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(
            recall.helpful_repairs.contains(&old.id),
            "immutable link history"
        );
        assert!(recall.helpful_repairs.contains(&current.id));
        let exposed = recalled_repair_evidence(&fixture.db, WS, &recall)?;
        assert!(exposed.iter().any(|item| item.evidence_id == current.id));
        assert!(exposed.iter().all(|item| item.evidence_id != old.id));
        assert_eq!(
            record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded,
            0
        );
        assert_eq!(
            fixture
                .db
                .list_evidence_spans_for_session(&fixture.session)?
                .len(),
            original_rows.len() + 1
        );
        assert_eq!(
            fixture.db.get_evidence_span(&current.id)?.as_ref(),
            Some(&current)
        );
    }
    Ok(())
}

#[test]
fn a_withdrawn_original_or_prior_revision_never_materializes_a_replacement() -> TestResult {
    for withdraw_revision in [false, true] {
        let fixture = Fixture::new(&[FIX])?;
        let original = fixture.older_card(false)?;
        let target = if withdraw_revision {
            fixture.older_revision()?
        } else {
            original
        };
        fixture
            .db
            .with_transaction_error(|| {
                fixture.db.withdraw_cass_evidence_in_current_snapshot(
                    WS,
                    &target.id,
                    "Withdraw this incident's guidance.",
                    None,
                    false,
                )
            })
            .map_err(|error| format!("withdrawal: {error:?}"))?;
        let before = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        let expected = fixture.draft()?.revision_draft();
        assert_eq!(
            record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded,
            0
        );
        assert!(fixture.db.get_evidence_span(&expected.id)?.is_none());
        assert_eq!(
            fixture
                .db
                .list_evidence_spans_for_session(&fixture.session)?,
            before
        );
        // The refusal also applies to a current revision already present when
        // a family member is withdrawn; direct readers cannot bypass import.
        fixture
            .db
            .insert_evidence_span(&expected.id, &expected.input)?;
        let current = fixture
            .db
            .get_evidence_span(&expected.id)?
            .ok_or("current")?;
        let mut checks = CardChecks::default();
        checks.observe(&current)?;
        assert_eq!(
            checks.withheld(&fixture.db, WS)?,
            BTreeSet::from([current.id])
        );
    }
    Ok(())
}

#[test]
fn harmful_feedback_on_any_retained_revision_withholds_the_current_card() -> TestResult {
    for harmful_revision in [false, true] {
        let fixture = Fixture::new(&[FIX])?;
        let original = fixture.older_card(false)?;
        let target = if harmful_revision {
            fixture.older_revision()?
        } else {
            original
        };
        let diagnostic = from_rustc(Some("E0277"), "trait bound");
        record_error_repair_links(
            &fixture.db,
            WS,
            &diagnostic,
            &ErrorRepairLinkRecording {
                harmful_repairs: vec![target.id.clone()],
                created_by: Some("explicit-human-review".to_owned()),
                ..ErrorRepairLinkRecording::default()
            },
        )?;
        let expected = fixture.draft()?.revision_draft();
        assert_eq!(
            record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded,
            1
        );
        let current = fixture
            .db
            .get_evidence_span(&expected.id)?
            .ok_or("current")?;
        let mut checks = CardChecks::default();
        checks.observe(&current)?;
        assert!(checks.verified_source_ids(&fixture.db, WS)?.is_empty());
        let recall = error_recall_report(&fixture.db, WS, &diagnostic)?;
        assert!(recall.harmful_repairs.contains(&target.id));
        assert!(
            recalled_repair_evidence(&fixture.db, WS, &recall)?
                .iter()
                .all(|item| item.evidence_id != current.id)
        );
        assert_eq!(
            fixture.db.get_evidence_span(&target.id)?.as_ref(),
            Some(&target)
        );
    }
    Ok(())
}

#[test]
fn failed_revision_insert_rolls_back_fingerprints_links_and_source_changes() -> TestResult {
    let fixture = Fixture::new(&[FIX])?;
    fixture.older_card(false)?;
    let expected = fixture.draft()?.revision_draft();
    let before = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    let key = from_rustc(Some("E0277"), "trait bound").layered_key().key;
    let links = fixture.db.list_error_repair_links(WS, &key)?;
    fixture.db.execute_raw(&format!(
        "CREATE TRIGGER reject_incident_revision BEFORE INSERT ON evidence_spans WHEN NEW.id = '{}' BEGIN SELECT RAISE(ABORT, 'revision insertion failed'); END;",
        expected.id,
    ))?;
    assert!(record_session_error_recall(&fixture.db, WS, &fixture.session).is_err());
    assert_eq!(
        fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?,
        before
    );
    assert_eq!(fixture.db.list_error_repair_links(WS, &key)?, links);
    Ok(())
}

#[test]
fn a_conflicting_revision_row_is_never_overwritten_or_approved() -> TestResult {
    let fixture = Fixture::new(&[FIX])?;
    fixture.older_card(false)?;
    let expected = fixture.draft()?.revision_draft();
    let mut collision = expected.input.clone();
    collision
        .excerpt
        .push_str("\nA different stored rendering.");
    collision.content_hash = format!(
        "blake3:{}",
        blake3::hash(collision.excerpt.as_bytes()).to_hex()
    );
    fixture.db.insert_evidence_span(&expected.id, &collision)?;
    let before = fixture
        .db
        .get_evidence_span(&expected.id)?
        .ok_or("collision")?;
    assert_eq!(
        record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded,
        0
    );
    assert_eq!(
        fixture.db.get_evidence_span(&expected.id)?.as_ref(),
        Some(&before)
    );
    let mut checks = CardChecks::default();
    checks.observe(&before)?;
    assert_eq!(
        checks.withheld(&fixture.db, WS)?,
        BTreeSet::from([before.id])
    );
    Ok(())
}

#[test]
fn reserved_card_identities_never_become_tool_events_or_repair_sources() -> TestResult {
    let fixture = Fixture::new(&[FIX])?;
    let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
    let sources = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    let expected = session_failure_arcs(WS, &session, &sources);
    assert_eq!(expected.len(), 1);
    assert_eq!(
        expected[0]
            .resolution
            .as_ref()
            .ok_or("resolution")?
            .repair_ids,
        fixture.repairs
    );
    for (index, source) in sources.iter().enumerate() {
        let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
            0x760300 + u128::try_from(index)?,
        ))
        .to_string();
        // A reserved native locator survives normal security screening even
        // if the row's kind, role and JSON body mimic a real transcript turn.
        fixture.db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: fixture.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("ee-incident-card:incident_card.v1:{id}"),
                span_kind: source.span_kind.clone(),
                start_line: source.start_line,
                end_line: source.end_line,
                start_byte: None,
                end_byte: None,
                role: source.role.clone(),
                excerpt: source.excerpt.clone(),
                content_hash: source.content_hash.clone(),
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        let reserved = fixture.db.get_evidence_span(&id)?.ok_or("reserved")?;
        assert!(!reserved.is_derived_incident_card());
        assert!(crate::core::incident_card::proof::requires_proof(&reserved));
    }
    let mixed = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    assert_eq!(session_failure_arcs(WS, &session, &mixed), expected);
    assert_eq!(
        record_session_error_recall(&fixture.db, WS, &fixture.session)?.incident_cards_recorded,
        1
    );
    let current = fixture
        .db
        .get_evidence_span(&fixture.draft()?.id)?
        .ok_or("current")?;
    let mut checks = CardChecks::default();
    checks.observe(&current)?;
    assert!(checks.withheld(&fixture.db, WS)?.is_empty());
    Ok(())
}
