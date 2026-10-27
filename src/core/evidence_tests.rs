use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
    StoredEvidenceSpan, audit_actions,
};
use crate::models::SessionId;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const OTHER: &str = "wsp_01234567890123456789012346";
const SOURCE: &str = "The cache flushes pending writes before reading the saved transaction.";

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    evidence: String,
    session: String,
}

impl Fixture {
    fn new(producer: EvidenceProducerKind) -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join(".ee"))?;
        let database = root.path().join(".ee/ee.db");
        let db = DbConnection::open_file(&database)?;
        db.migrate()?;
        db.insert_workspace(
            WS,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: None,
            },
        )?;
        let session = SessionId::now().to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "withdraw-fixture".to_owned(),
                source_path: None,
                agent_name: None,
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                metadata_json: None,
            },
        )?;
        let evidence = EvidenceId::now().to_string();
        db.insert_evidence_span(
            &evidence,
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: session.clone(),
                memory_id: None,
                producer_kind: producer,
                cass_span_id: "withdraw-fixture:1".to_owned(),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: SOURCE.to_owned(),
                content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        db.close()?;
        Ok(Self {
            root,
            database,
            evidence,
            session,
        })
    }

    fn options(&self, dry_run: bool) -> EvidenceWithdrawOptions<'_> {
        EvidenceWithdrawOptions {
            workspace_path: self.root.path(),
            database_path: None,
            evidence_id: &self.evidence,
            reason: "The operator withdrew this source.",
            actor: Some("withdraw-test"),
            dry_run,
        }
    }

    fn row(&self) -> TestResult<StoredEvidenceSpan> {
        let db = DbConnection::open_file_read_only(&self.database)?;
        Ok(db.get_evidence_span(&self.evidence)?.ok_or("source")?)
    }
}

fn files(root: &Path) -> TestResult<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(root: &Path, at: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) -> TestResult {
        for entry in std::fs::read_dir(at)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(root, &path, found)?;
            } else {
                found.insert(path.strip_prefix(root)?.to_path_buf(), std::fs::read(path)?);
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found)?;
    Ok(found)
}

#[test]
fn evidence_withdraw_preview_is_read_only_and_reports_no_committed_work() -> TestResult {
    let fixture = Fixture::new(EvidenceProducerKind::CassImport)?;
    let before = files(fixture.root.path())?;
    let row = fixture.row()?;
    let report = withdraw_cass_evidence(&fixture.options(true))?;
    assert_eq!(report.status, "would_withdraw");
    assert!(report.dry_run && report.would_change);
    assert!(!report.persisted && !report.changed);
    assert_eq!(report.previous_search_eligibility, "admitted");
    assert_eq!(report.search_eligibility, "denied");
    assert_eq!(report.pack_eligibility, "denied");
    assert_eq!(report.index_status, "dry_run_not_queued");
    assert!(report.audit_id.is_none() && report.index_job_id.is_none());
    assert!(!serde_json::to_string(&report)?.contains(SOURCE));
    assert_eq!(fixture.row()?, row);
    assert_eq!(files(fixture.root.path())?, before);
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    assert!(db.list_audit_entries(Some(WS), None)?.is_empty());
    assert!(db.list_search_index_jobs(WS, None)?.is_empty());
    Ok(())
}

#[test]
fn evidence_withdraw_preview_never_creates_or_migrates_a_store() -> TestResult {
    let root = tempfile::tempdir()?;
    let id = EvidenceId::now().to_string();
    let options = EvidenceWithdrawOptions {
        workspace_path: root.path(),
        database_path: None,
        evidence_id: &id,
        reason: "No longer relevant.",
        actor: None,
        dry_run: true,
    };
    assert!(matches!(
        withdraw_cass_evidence(&options),
        Err(DomainError::WorkspaceStoreMissing { .. })
    ));
    assert!(!root.path().join(".ee").exists());

    let database = root.path().join("older.db");
    let db = DbConnection::open_file(&database)?;
    db.execute_raw("CREATE TABLE original_marker (value TEXT)")?;
    db.close()?;
    let before = files(root.path())?;
    assert!(matches!(
        withdraw_cass_evidence(&EvidenceWithdrawOptions {
            database_path: Some(&database),
            ..options
        }),
        Err(DomainError::MigrationRequired { .. })
    ));
    assert_eq!(files(root.path())?, before);
    let db = DbConnection::open_file_read_only(&database)?;
    assert_eq!(db.schema_version()?, None);
    Ok(())
}

#[test]
fn evidence_withdraw_commits_denial_audit_job_and_retains_source_bytes() -> TestResult {
    let fixture = Fixture::new(EvidenceProducerKind::CassImport)?;
    let before = fixture.row()?;
    let mut options = fixture.options(false);
    // The audit records the operator's reason without copying a pasted secret.
    let secret = ["sk-", &"withdrawtest".repeat(6)].concat();
    let reason = format!("Retire the source that exposed api_key={secret}");
    options.reason = &reason;
    let report = withdraw_cass_evidence(&options)?;
    assert_eq!(report.status, "withdrawn");
    assert!(!report.dry_run);
    assert!(report.persisted && report.changed && report.would_change);
    assert_eq!(report.index_status, "queued");
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    let after = db.get_evidence_span(&fixture.evidence)?.ok_or("source")?;
    let session = db.get_session(&fixture.session)?.ok_or("session")?;
    assert_eq!(after.search_eligibility, "denied");
    assert_eq!(after.pack_eligibility, "denied");
    assert_eq!(after.excerpt, before.excerpt);
    assert_eq!(after.content_hash, before.content_hash);
    assert_eq!(after.canonical_excerpt_hash, before.canonical_excerpt_hash);
    assert_eq!(after.cass_span_id, before.cass_span_id);
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.memory_id, before.memory_id);
    assert_eq!(
        (after.start_line, after.end_line),
        (before.start_line, before.end_line)
    );
    let mut metadata: serde_json::Value =
        serde_json::from_str(before.metadata_json.as_deref().ok_or("metadata")?)?;
    metadata["searchEligibility"] = "denied".into();
    metadata["packEligibility"] = "denied".into();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            after.metadata_json.as_deref().ok_or("metadata")?
        )?,
        metadata,
    );
    assert!(!after.is_search_admitted_for_session(WS, &session));
    assert!(!after.is_direct_pack_admitted_for_session(WS, &session));
    assert!(!after.is_derivation_admitted_for_session(WS, &session));
    assert!(!after.is_class_a_derivation_readable(WS, &session));
    assert!(
        db.get_search_admitted_evidence_span(&fixture.evidence, WS)?
            .is_none()
    );
    assert!(
        db.get_derivation_admitted_evidence_span(&fixture.evidence, WS)?
            .is_none()
    );
    assert!(
        db.list_search_admitted_evidence_spans_for_session(WS, &fixture.session)?
            .is_empty()
    );
    assert!(
        db.get_evidence_spans_with_sessions(&[&fixture.evidence])?
            .iter()
            .all(|row| !row.is_direct_pack_admitted(WS))
    );
    let audit = db
        .get_audit(report.audit_id.as_deref().ok_or("audit id")?)?
        .ok_or("audit")?;
    assert_eq!(audit.action, audit_actions::EVIDENCE_WITHDRAW);
    assert!(audit.before_hash.is_some() && audit.after_hash.is_some());
    assert_ne!(audit.before_hash, audit.after_hash);
    let details = audit.details.as_deref().ok_or("audit details")?;
    assert!(!details.contains(SOURCE) && !details.contains(&secret));
    let job = db
        .get_search_index_job(report.index_job_id.as_deref().ok_or("job id")?)?
        .ok_or("job")?;
    assert_eq!(job.status, "pending");
    assert_eq!(job.job_type, "incremental");
    assert_eq!(job.document_source.as_deref(), Some("session"));
    assert_eq!(job.document_id.as_deref(), Some(fixture.session.as_str()));
    assert_eq!(job.documents_indexed, 0);
    db.close()?;

    let db = DbConnection::open_file_read_only(&fixture.database)?;
    let generation = db.get_workspace_generation(WS)?;
    let audits = db.list_audit_entries(Some(WS), None)?;
    let jobs = db.list_search_index_jobs(WS, None)?;
    db.close()?;
    for dry_run in [true, false] {
        let repeat = withdraw_cass_evidence(&fixture.options(dry_run))?;
        assert_eq!(repeat.status, "already_withdrawn");
        assert!(!repeat.changed && !repeat.persisted && !repeat.would_change);
        assert!(repeat.audit_id.is_none() && repeat.index_job_id.is_none());
        assert_eq!(repeat.index_status, "not_scheduled");
        assert_eq!(fixture.row()?, after);
    }
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    assert_eq!(db.get_workspace_generation(WS)?, generation);
    assert_eq!(db.list_audit_entries(Some(WS), None)?, audits);
    assert_eq!(db.list_search_index_jobs(WS, None)?, jobs);
    Ok(())
}

#[test]
fn evidence_withdraw_refuses_foreign_sources_and_non_cass_producers() -> TestResult {
    for producer in [
        EvidenceProducerKind::CassImport,
        EvidenceProducerKind::DocsBootstrap,
    ] {
        let fixture = Fixture::new(producer)?;
        let before = fixture.row()?;
        let other = tempfile::tempdir()?;
        let db = DbConnection::open_file(&fixture.database)?;
        db.insert_workspace(
            OTHER,
            &CreateWorkspaceInput {
                path: other.path().to_string_lossy().into_owned(),
                name: None,
            },
        )?;
        let generation = db.get_workspace_generation(WS)?;
        db.close()?;
        for dry_run in [true, false] {
            let mut options = fixture.options(dry_run);
            if producer == EvidenceProducerKind::CassImport {
                options.workspace_path = other.path();
                options.database_path = Some(&fixture.database);
                assert!(matches!(
                    withdraw_cass_evidence(&options),
                    Err(DomainError::NotFound { .. })
                ));
            } else {
                assert!(matches!(
                    withdraw_cass_evidence(&options),
                    Err(DomainError::Usage { .. })
                ));
            }
        }
        assert_eq!(fixture.row()?, before);
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert_eq!(db.get_workspace_generation(WS)?, generation);
        assert!(db.list_audit_entries(Some(WS), None)?.is_empty());
        assert!(db.list_search_index_jobs(WS, None)?.is_empty());
        if producer == EvidenceProducerKind::DocsBootstrap {
            assert!(
                db.get_derivation_admitted_evidence_span(&fixture.evidence, WS)?
                    .is_some()
            );
        }
    }
    Ok(())
}

#[test]
fn evidence_withdraw_rolls_back_denial_when_audit_or_job_fails() -> TestResult {
    for table in ["search_index_jobs", "audit_log"] {
        let fixture = Fixture::new(EvidenceProducerKind::CassImport)?;
        let before = fixture.row()?;
        let db = DbConnection::open_file(&fixture.database)?;
        db.execute_raw(&format!(
            "CREATE TRIGGER reject_withdrawal BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'withdrawal failure fixture'); END"
        ))?;
        let generation = db.get_workspace_generation(WS)?;
        db.close()?;
        assert!(matches!(
            withdraw_cass_evidence(&fixture.options(false)),
            Err(DomainError::Storage { .. })
        ));
        assert_eq!(fixture.row()?, before);
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert_eq!(db.get_workspace_generation(WS)?, generation);
        assert!(db.list_audit_entries(Some(WS), None)?.is_empty());
        assert!(db.list_search_index_jobs(WS, None)?.is_empty());
        assert!(
            db.get_search_admitted_evidence_span(&fixture.evidence, WS)?
                .is_some()
        );
    }
    Ok(())
}

#[test]
fn evidence_withdraw_requires_reason_and_valid_canonical_metadata() -> TestResult {
    let fixture = Fixture::new(EvidenceProducerKind::CassImport)?;
    let before = files(fixture.root.path())?;
    for reason in ["", " \n\t"] {
        let mut options = fixture.options(false);
        options.reason = reason;
        assert!(matches!(
            withdraw_cass_evidence(&options),
            Err(DomainError::Usage { .. })
        ));
    }
    assert_eq!(files(fixture.root.path())?, before);
    let db = DbConnection::open_file(&fixture.database)?;
    db.execute_raw("UPDATE evidence_spans SET metadata_json = '{}' ")?;
    db.close()?;
    let row = fixture.row()?;
    for dry_run in [true, false] {
        assert!(matches!(
            withdraw_cass_evidence(&fixture.options(dry_run)),
            Err(DomainError::Storage { .. })
        ));
    }
    assert_eq!(fixture.row()?, row);
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    assert!(db.list_audit_entries(Some(WS), None)?.is_empty());
    assert!(db.list_search_index_jobs(WS, None)?.is_empty());
    Ok(())
}

#[test]
fn evidence_withdraw_propagates_to_current_incident_card_proof_without_rewriting_history()
-> TestResult {
    use crate::core::incident_card::proof::{CardChecks, tests::Fixture as CardFixture};

    for source in ["attempt", "failure", "repair", "proof"] {
        let fixture = CardFixture::new_public(true)?;
        let card = fixture.current_card()?;
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        assert!(checks.withheld(&fixture.db, WS)?.is_empty());
        let id = match source {
            "attempt" => &fixture.attempt,
            "failure" => &fixture.failure,
            "repair" => &fixture.repair,
            _ => &fixture.proof,
        };
        let before = fixture.db.get_evidence_span(id)?.ok_or("source")?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        assert!(before.is_class_a_derivation_readable(WS, &session));
        fixture
            .db
            .with_transaction_error(|| {
                fixture.db.withdraw_cass_evidence_in_current_snapshot(
                    WS,
                    id,
                    "Withdraw the incident source.",
                    None,
                    false,
                )
            })
            .map_err(|error| format!("withdrawal failed: {error:?}"))?;
        let after = fixture.db.get_evidence_span(id)?.ok_or("retained source")?;
        assert_eq!(after.excerpt, before.excerpt);
        assert!(!after.is_class_a_derivation_readable(WS, &session));
        assert!(!after.is_derivation_admitted_for_session(WS, &session));
        let mut checks = CardChecks::default();
        checks.observe(&card)?;
        assert!(
            checks.withheld(&fixture.db, WS)?.contains(&card.id),
            "{source} withdrawal must revoke the source proof"
        );
        assert_eq!(
            fixture
                .db
                .get_evidence_span(&card.id)?
                .ok_or("retained card")?,
            card
        );
    }
    Ok(())
}

#[test]
fn evidence_withdraw_of_a_derived_card_survives_rederivation() -> TestResult {
    use crate::core::cass_error_recall::record_session_error_recall;
    use crate::core::incident_card::proof::tests::Fixture as CardFixture;

    let fixture = CardFixture::new_public(true)?;
    let card = fixture.current_card()?;
    fixture
        .db
        .with_transaction_error(|| {
            fixture.db.withdraw_cass_evidence_in_current_snapshot(
                WS,
                &card.id,
                "Withdraw this derived guidance.",
                None,
                false,
            )
        })
        .map_err(|error| format!("withdrawal failed: {error:?}"))?;
    let denied = fixture.db.get_evidence_span(&card.id)?.ok_or("card")?;
    assert_eq!(denied.excerpt, card.excerpt);
    let rederived = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
    assert_eq!(rederived.incident_cards_recorded, 0);
    assert_eq!(
        fixture.db.get_evidence_span(&card.id)?.ok_or("card")?,
        denied
    );
    assert!(
        fixture
            .db
            .get_search_admitted_evidence_span(&card.id, WS)?
            .is_none()
    );
    // Withdrawing a derived summary does not withdraw its original sources.
    assert!(
        fixture
            .db
            .get_search_admitted_evidence_span(&fixture.repair, WS)?
            .is_some()
    );
    Ok(())
}
