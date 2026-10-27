//! Real migrated sources: a safe stored summary still needs a current proof.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::cass_error_recall::{
    CommandFamily, FailureArc, Resolution, record_session_error_recall,
    structured_error_diagnostics,
};
use crate::core::error_recall::CanonicalDiagnostic;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
};
use serde_json::json;

pub(crate) type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
pub(crate) const WS: &str = "wsp_01234567890123456789012345";
pub(crate) const OTHER: &str = "wsp_01234567890123456789012346";
const COMMAND: &str = "go test -json ./...";
const REPAIR: &str = "The cache test failed because writes were not flushed. I fixed the cache lookup by flushing pending writes before reads in src/cache.go.";

pub(crate) struct Fixture {
    pub(crate) db: DbConnection,
    pub(crate) session: String,
    pub(crate) attempt: String,
    pub(crate) failure: String,
    pub(crate) repair: String,
    pub(crate) proof: String,
    pub(crate) diagnostic: CanonicalDiagnostic,
    command: &'static str,
}

fn go(results: &[(&str, &str)]) -> String {
    let mut rows = vec![json!({"Action":"start","Package":"example.org/cache"})];
    for (name, status) in results {
        rows.push(json!({"Action":"run","Package":"example.org/cache","Test":name}));
        rows.push(json!({"Action":status,"Package":"example.org/cache","Test":name}));
    }
    rows.push(json!({"Action":if results.iter().any(|(_, status)| *status == "fail") {"fail"} else {"pass"},"Package":"example.org/cache"}));
    // Keep these equivalent JSONL frames whitespace-separated. The public
    // resume boundary deliberately refuses long opaque atoms; a minified
    // diagnostic is not an appropriate positive public-excerpt fixture.
    rows.iter()
        .map(|row| format!("{row:#}").replace('\n', " "))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Fixture {
    pub(crate) fn new(valid_retry: bool) -> TestResult<Self> {
        Self::with_output(valid_retry, false)
    }

    /// Plain compiler diagnostics exercise the positive public replay path.
    /// Structured Go cards intentionally carry a fingerprint in their symptom;
    /// the public replay guard rejects that opaque atom, even with valid proof.
    pub(crate) fn new_public(valid_retry: bool) -> TestResult<Self> {
        Self::with_output(valid_retry, true)
    }

    fn with_output(valid_retry: bool, public: bool) -> TestResult<Self> {
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
        let command = if public { "cargo check" } else { COMMAND };
        let failed = if public {
            "error[E0308]: mismatched types\n --> src/cache.rs:4:2\n".to_owned()
        } else {
            go(&[("TestCache", "fail"), ("TestControl", "pass")])
        };
        let passed = if public {
            if valid_retry {
                "Checking cache v0.1.0\nFinished `dev` profile [unoptimized] target(s) in 0.01s\n"
                    .to_owned()
            } else {
                failed.clone()
            }
        } else if valid_retry {
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
        let diagnostic = if public {
            crate::core::error_recall::from_cargo(Some("E0308"), "mismatched types")
        } else {
            structured_error_diagnostics(&failed)
                .into_iter()
                .next()
                .ok_or("diagnostic")?
        };
        let id = |line: u32| {
            EvidenceId::from_uuid(uuid::Uuid::from_u128(0x7700 + u128::from(line))).to_string()
        };
        let rows = [
            (
                "tool_call",
                "assistant",
                json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":command}}),
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
                json!({"type":"tool_use","id":"retry","name":"Bash","input":{"command":command}}),
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
            command,
        })
    }

    pub(crate) fn old_card(&self) -> TestResult<String> {
        // Simulate a card written by a former policy that accepted a skipped
        // control. The new interpreter is never weakened to create this row.
        let arc = FailureArc {
            failure_id: self.failure.clone(),
            attempt_id: self.attempt.clone(),
            family: CommandFamily::parse(self.command).ok_or("family")?,
            symptom: Some("The cache test failed because writes were not flushed.".to_owned()),
            diagnostics: vec![self.diagnostic.clone()],
            resolution: Some(Resolution {
                proof_id: self.proof.clone(),
                proof_family: CommandFamily::parse(self.command).ok_or("family")?,
                repair_ids: vec![self.repair.clone()],
            }),
        };
        let spans = self.db.list_evidence_spans_for_session(&self.session)?;
        let card = draft_incident_card(WS, &self.session, &spans, &arc).ok_or("card")?;
        self.db.insert_evidence_span(&card.id, &card.input)?;
        Ok(card.id)
    }
}

impl Fixture {
    pub(crate) fn current_card(&self) -> TestResult<StoredEvidenceSpan> {
        let report = record_session_error_recall(&self.db, WS, &self.session)?;
        assert_eq!(report.resolved_failures, 1);
        let id = super::super::incident_card_id(WS, &self.failure);
        Ok(self.db.get_evidence_span(&id)?.ok_or("missing card")?)
    }
}

fn check(fixture: &Fixture, spans: &[StoredEvidenceSpan]) -> Result<BTreeSet<String>> {
    let mut checks = CardChecks::default();
    for span in spans {
        checks.observe(span)?;
    }
    checks.withheld(&fixture.db, WS)
}

#[test]
fn historical_unlinked_card_is_withheld_without_relabeling_or_changing_history() -> TestResult {
    let fixture = Fixture::new(false)?;
    let id = fixture.old_card()?;
    let card = fixture.db.get_evidence_span(&id)?.ok_or("card")?;
    let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
    assert!(card.is_direct_pack_admitted_for_session(WS, &session));
    let before = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    assert_eq!(
        check(&fixture, std::slice::from_ref(&card))?,
        BTreeSet::from([id])
    );
    assert!(
        fixture
            .db
            .list_error_repair_links(WS, &fixture.diagnostic.layered_key().key)?
            .is_empty()
    );
    for original in before {
        let after = fixture
            .db
            .get_evidence_span(&original.id)?
            .ok_or("source missing")?;
        assert_eq!(after.excerpt, original.excerpt);
        assert_eq!(after.content_hash, original.content_hash);
        assert_eq!(after.metadata_json, original.metadata_json);
        assert_eq!(
            after.canonical_provenance_uri(),
            original.canonical_provenance_uri()
        );
    }
    Ok(())
}

#[test]
fn currently_derived_cards_keep_exact_source_text_and_fingerprint_proof() -> TestResult {
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    let history = fixture
        .db
        .list_error_repair_links(WS, &fixture.diagnostic.layered_key().key)?;
    assert!(check(&fixture, std::slice::from_ref(&card))?.is_empty());
    let key = fixture.diagnostic.layered_key().key;
    let proofs = session_proofs(&fixture.db, WS, Some(&key), &fixture.session)?;
    assert!(proofs.claims.contains(&Claim {
        failure: fixture.failure.clone(),
        target: card.id.clone(),
        card: true,
    }));
    assert!(
        session_proofs(&fixture.db, WS, Some("unrelated"), &fixture.session)?
            .claims
            .is_empty()
    );
    assert_eq!(fixture.db.list_error_repair_links(WS, &key)?, history);
    assert_eq!(
        fixture
            .db
            .get_evidence_span(&card.id)?
            .ok_or("card")?
            .excerpt,
        card.excerpt
    );
    Ok(())
}

#[test]
fn supplied_card_must_match_every_committed_field_not_just_id_or_declared_hash() -> TestResult {
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    for variant in 0..12 {
        let mut changed = card.clone();
        match variant {
            0 => changed.excerpt.push_str(" Altered advice."),
            1 => changed.content_hash.push('0'),
            2 => changed.metadata_json = None,
            3 => changed.start_line += 1,
            4 => changed.end_line += 1,
            5 => changed.memory_id = Some("mem_unrelated".to_owned()),
            6 => changed.start_byte = Some(0),
            7 => changed.end_byte = Some(1),
            8 => changed.id.push_str("\0alias"),
            9 => changed.session_id.push_str("\0alias"),
            10 => changed.span_kind = "message".to_owned(),
            _ => changed.role = Some("assistant".to_owned()),
        }
        assert_eq!(
            check(&fixture, &[changed.clone()])?,
            BTreeSet::from([changed.id.clone()]),
            "{variant}"
        );
        // Input order cannot make a conflicting observation disappear.
        for pair in [
            [changed.clone(), card.clone()],
            [card.clone(), changed.clone()],
        ] {
            let result = check(&fixture, &pair)?;
            assert!(result.contains(&changed.id));
        }
    }
    Ok(())
}

#[test]
fn reserved_card_locator_cannot_reenter_as_plain_transcript_after_shape_changes() -> TestResult {
    let fixture = Fixture::new(true)?;
    let mut card = fixture.current_card()?;
    card.excerpt = "This altered summary must not inherit proof.".to_owned();
    assert!(!card.is_derived_incident_card());
    assert!(requires_proof(&card));
    assert_eq!(check(&fixture, &[card.clone()])?, BTreeSet::from([card.id]));
    Ok(())
}

#[test]
fn repeated_cards_share_one_real_source_read_and_disagreeing_twins_stay_refused() -> TestResult {
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    let mut checks = CardChecks::default();
    for _ in 0..300 {
        checks.observe(&card)?;
    }
    assert_eq!(checks.sessions.len(), 1);
    assert_eq!(checks.sessions.values().next().unwrap().len(), 1);
    let mut reads = 0;
    let refused = checks.withheld_with(|session| {
        reads += 1;
        session_proofs(&fixture.db, WS, None, session)
    })?;
    assert!(refused.is_empty());
    assert_eq!(reads, 1);
    Ok(())
}

#[test]
fn ordinary_transcript_sources_do_not_trigger_proof_queries() -> TestResult {
    let fixture = Fixture::new(true)?;
    let ordinary = fixture
        .db
        .get_evidence_span(&fixture.repair)?
        .ok_or("repair")?;
    let mut checks = CardChecks::default();
    checks.observe(&ordinary)?;
    let mut called = false;
    assert!(
        checks
            .withheld_with(|_| {
                called = true;
                Err(unavailable())
            })?
            .is_empty()
    );
    assert!(!called);
    Ok(())
}

#[test]
fn foreign_missing_and_over_budget_sources_cannot_certify_a_safe_stored_card() -> TestResult {
    for variant in 0..3 {
        let fixture = Fixture::new(true)?;
        let card = fixture.current_card()?;
        match variant {
            0 => {
                fixture.db.execute_raw(&format!(
                    "UPDATE sessions SET workspace_id = '{OTHER}' WHERE id = '{}'",
                    fixture.session
                ))?;
            }
            1 => {
                fixture.db.execute_raw(&format!(
                "UPDATE evidence_spans SET excerpt = 'unverifiable original failure' WHERE id = '{}'", fixture.failure
            ))?;
            }
            _ => {
                fixture.db.execute_raw(&format!(
                    "UPDATE evidence_spans SET metadata_json = '{}' WHERE id = '{}'",
                    serde_json::to_string(&"x".repeat(MAX_SOURCE_BYTES))?,
                    fixture.failure,
                ))?;
            }
        }
        assert_eq!(
            check(&fixture, std::slice::from_ref(&card))?,
            BTreeSet::from([card.id])
        );
    }
    Ok(())
}

#[test]
fn unavailable_sources_return_no_partial_verdict_and_caller_transaction_remains_owned() -> TestResult
{
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    let result: Result<()> = fixture.db.with_transaction(|| {
        fixture.db.execute_raw(&format!(
            "UPDATE sessions SET workspace_id = '{OTHER}' WHERE id = '{}'",
            fixture.session
        ))?;
        assert!(check(&fixture, std::slice::from_ref(&card))?.contains(&card.id));
        Err(unavailable())
    });
    assert!(result.is_err());
    assert!(check(&fixture, std::slice::from_ref(&card))?.is_empty());
    fixture.db.begin_read_snapshot()?;
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    let error = checks.withheld_with(|_| Err(unavailable())).unwrap_err();
    assert!(!error.to_string().contains(&fixture.session));
    fixture.db.rollback_read_snapshot()?;
    fixture.db.begin_read_snapshot()?;
    fixture.db.rollback_read_snapshot()?;
    Ok(())
}

pub(crate) fn record_card_feedback(
    connection: &DbConnection,
    workspace: &str,
    diagnostic: &CanonicalDiagnostic,
    target: &str,
    harmful: bool,
) -> Result<()> {
    let mut recording = crate::core::error_diagnosis::ErrorRepairLinkRecording::default();
    if harmful {
        recording.harmful_repairs.push(target.to_owned());
    } else {
        recording.helpful_repairs.push(target.to_owned());
    }
    crate::core::error_diagnosis::persist_error_repair_links(
        connection, workspace, diagnostic, &recording,
    )?;
    Ok(())
}

#[test]
fn harmful_card_or_repair_feedback_withholds_summary_without_rewriting_episode() -> TestResult {
    for target_is_card in [false, true] {
        let fixture = Fixture::new(true)?;
        let card = fixture.current_card()?;
        assert!(check(&fixture, std::slice::from_ref(&card))?.is_empty());
        let target = if target_is_card {
            &card.id
        } else {
            &fixture.repair
        };
        record_card_feedback(&fixture.db, WS, &fixture.diagnostic, target, true)?;
        // Helpful and harmful observations coexist; last writer does not win.
        record_card_feedback(&fixture.db, WS, &fixture.diagnostic, target, false)?;
        let key = fixture.diagnostic.layered_key().key;
        let history = fixture.db.list_error_repair_links(WS, &key)?;
        assert!(
            history
                .iter()
                .any(|row| row.target_id == *target && row.outcome == "harmful")
        );
        assert!(
            history
                .iter()
                .any(|row| row.target_id == *target && row.outcome == "helpful")
        );
        let before = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        assert_eq!(
            check(&fixture, std::slice::from_ref(&card))?,
            BTreeSet::from([card.id.clone()])
        );
        let proofs = session_proofs(&fixture.db, WS, Some(&key), &fixture.session)?;
        assert!(proofs.claims.contains(&Claim {
            failure: fixture.failure.clone(),
            target: fixture.repair.clone(),
            card: false
        }));
        assert!(!proofs.claims.contains(&Claim {
            failure: fixture.failure.clone(),
            target: card.id.clone(),
            card: true
        }));
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        checks.visit_verified(&fixture.db, WS, |_| {
            panic!("harmful guidance cannot reach a reader")
        })?;
        assert_eq!(fixture.db.list_error_repair_links(WS, &key)?, history);
        assert!(
            history
                .iter()
                .any(|row| row.link_kind == "proof" && row.target_id == fixture.proof)
        );
        for original in before {
            let current = fixture
                .db
                .get_evidence_span(&original.id)?
                .ok_or("source")?;
            assert_eq!(current.excerpt, original.excerpt);
            assert_eq!(current.content_hash, original.content_hash);
            assert_eq!(current.metadata_json, original.metadata_json);
            assert_eq!(
                current.canonical_provenance_uri(),
                original.canonical_provenance_uri()
            );
        }
    }
    Ok(())
}

#[test]
fn feedback_scope_is_exact_and_proof_targets_are_not_repair_targets() -> TestResult {
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    let unrelated = crate::core::error_recall::from_ee_error("other_error", "different failure");
    record_card_feedback(&fixture.db, OTHER, &fixture.diagnostic, &card.id, true)?;
    record_card_feedback(&fixture.db, WS, &unrelated, &fixture.repair, true)?;
    for alias in [
        format!("{}-copy", card.id),
        format!("{}\0alias", fixture.repair),
        fixture.proof.clone(),
    ] {
        record_card_feedback(&fixture.db, WS, &fixture.diagnostic, &alias, true)?;
    }
    assert!(check(&fixture, std::slice::from_ref(&card))?.is_empty());
    let mut checks = CardChecks::default();
    checks.observe(&card)?;
    let mut delivered = Vec::new();
    checks.visit_verified(&fixture.db, WS, |span| {
        delivered.push(span.id);
        Ok(())
    })?;
    assert_eq!(delivered, [card.id]);
    Ok(())
}

#[test]
fn every_native_error_in_a_card_binds_its_actual_repair_sources() -> TestResult {
    let fixture = Fixture::new(true)?;
    let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
    let spans = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    let mut arc = session_failure_arcs(WS, &session, &spans)
        .into_iter()
        .next()
        .ok_or("arc")?;
    let other = crate::core::error_recall::from_rustc(Some("E0277"), "missing bound");
    arc.diagnostics.push(other.clone());
    let card = super::super::incident_card_id(WS, &arc.failure_id);
    let mut feedback = CardFeedback::default();
    feedback.observe(&arc, &card);
    assert_eq!(feedback.by_fingerprint.len(), 2);
    assert!(feedback.withheld(&fixture.db, WS)?.is_empty());
    record_card_feedback(&fixture.db, WS, &other, &fixture.repair, true)?;
    assert_eq!(feedback.withheld(&fixture.db, WS)?, BTreeSet::from([card]));
    Ok(())
}

#[test]
fn feedback_batches_preserve_many_to_many_bindings_without_cross_error_contamination() -> TestResult
{
    let fixture = Fixture::new(true)?;
    let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
    let spans = fixture
        .db
        .list_evidence_spans_for_session(&fixture.session)?;
    let prototype = session_failure_arcs(WS, &session, &spans)
        .into_iter()
        .next()
        .ok_or("arc")?;
    let mut feedback = CardFeedback::default();
    let mut expected = BTreeSet::new();
    fixture.db.with_transaction(|| {
        for number in 0..257_u128 {
            let target = EvidenceId::from_uuid(uuid::Uuid::from_u128(0x90000 + number)).to_string();
            let mut arc = prototype.clone();
            arc.resolution.as_mut().expect("resolved").repair_ids = vec![target.clone()];
            for copy in 0..2_u128 {
                let card =
                    EvidenceId::from_uuid(uuid::Uuid::from_u128(0xa0000 + number * 2 + copy))
                        .to_string();
                feedback.observe(&arc, &card);
                if number % 7 == 0 {
                    expected.insert(card);
                }
            }
            record_card_feedback(
                &fixture.db,
                WS,
                &fixture.diagnostic,
                &target,
                number % 7 == 0,
            )?;
            record_card_feedback(&fixture.db, OTHER, &fixture.diagnostic, &target, true)?;
        }
        Ok(())
    })?;
    let targets = feedback.by_fingerprint.values().next().expect("bindings");
    assert_eq!(targets.len(), 257 * 3);
    assert_eq!(feedback.withheld(&fixture.db, WS)?, expected);
    assert_eq!(feedback.withheld(&fixture.db, WS)?, expected);
    Ok(())
}

#[test]
fn harmful_card_feedback_follows_caller_transactions_without_a_persistent_cache() -> TestResult {
    let fixture = Fixture::new(true)?;
    let card = fixture.current_card()?;
    let history = fixture
        .db
        .list_error_repair_links(WS, &fixture.diagnostic.layered_key().key)?;
    let transaction: Result<()> = fixture.db.with_transaction(|| {
        record_card_feedback(&fixture.db, WS, &fixture.diagnostic, &fixture.repair, true)?;
        assert!(check(&fixture, std::slice::from_ref(&card))?.contains(&card.id));
        Err(unavailable())
    });
    assert!(transaction.is_err());
    assert_eq!(
        fixture
            .db
            .list_error_repair_links(WS, &fixture.diagnostic.layered_key().key)?,
        history
    );
    assert!(check(&fixture, std::slice::from_ref(&card))?.is_empty());
    record_card_feedback(&fixture.db, WS, &fixture.diagnostic, &fixture.repair, true)?;
    assert!(check(&fixture, std::slice::from_ref(&card))?.contains(&card.id));
    Ok(())
}

#[test]
fn feedback_read_failure_withholds_cards_but_does_not_require_feedback_for_plain_sources()
-> TestResult {
    for stored_card in [false, true] {
        let fixture = Fixture::new(true)?;
        let card = if stored_card {
            Some(fixture.current_card()?)
        } else {
            None
        };
        fixture
            .db
            .execute_raw("ALTER TABLE error_repair_links RENAME TO unavailable_card_feedback")?;
        fixture.db.begin_read_snapshot()?;
        if let Some(card) = card {
            assert!(check(&fixture, std::slice::from_ref(&card)).is_err());
            let mut checks = CardChecks::default();
            checks.observe(&card)?;
            let result =
                checks.visit_verified(&fixture.db, WS, |_| panic!("no partial feedback approval"));
            assert!(result.is_err());
            assert!(
                !result
                    .expect_err("unavailable")
                    .to_string()
                    .contains("unavailable_card_feedback")
            );
        } else {
            let proofs = session_proofs(&fixture.db, WS, None, &fixture.session)?;
            assert!(proofs.claims.contains(&Claim {
                failure: fixture.failure.clone(),
                target: fixture.repair.clone(),
                card: false
            }));
            assert!(proofs.cards.is_empty());
        }
        let ordinary = fixture
            .db
            .get_evidence_span(&fixture.repair)?
            .ok_or("repair")?;
        assert!(check(&fixture, &[ordinary])?.is_empty());
        fixture.db.commit_read_snapshot()?;
    }
    let empty = DbConnection::open_memory()?;
    assert!(CardFeedback::default().withheld(&empty, WS)?.is_empty());
    Ok(())
}
