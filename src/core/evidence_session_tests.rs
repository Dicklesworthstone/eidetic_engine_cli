use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use crate::models::EvidenceId;
use std::collections::BTreeMap;
use std::path::PathBuf;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const WS: &str = "wsp_01234567890123456789012345";
const OTHER: &str = "wsp_01234567890123456789012346";
const SOURCE: &str = "The durable cache writes the checksum before publishing the committed page.";

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    session: String,
    evidence: Vec<String>,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let database = root.path().join("custom.db");
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
                cass_session_id: "session-bulk-core".to_owned(),
                source_path: None,
                agent_name: None,
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 2,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                metadata_json: None,
            },
        )?;
        let mut evidence = Vec::new();
        for line in 1..=2 {
            let id = EvidenceId::now().to_string();
            db.insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: WS.to_owned(),
                    session_id: session.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("bulk-core:{line}"),
                    span_kind: "message".to_owned(),
                    start_line: line,
                    end_line: line,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: SOURCE.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )?;
            evidence.push(id);
        }
        db.close()?;
        Ok(Self {
            root,
            database,
            session,
            evidence,
        })
    }

    fn options(&self, dry_run: bool) -> EvidenceSessionWithdrawOptions<'_> {
        EvidenceSessionWithdrawOptions {
            workspace_path: self.root.path(),
            database_path: Some(&self.database),
            session_id: &self.session,
            reason: "Withdraw this session's retained observations.",
            actor: Some("bulk-core-test"),
            dry_run,
        }
    }

    fn bytes(&self) -> TestResult<BTreeMap<PathBuf, Vec<u8>>> {
        let mut bytes = BTreeMap::new();
        for entry in std::fs::read_dir(self.root.path())? {
            let path = entry?.path();
            if path.is_file() {
                bytes.insert(path.clone(), std::fs::read(path)?);
            }
        }
        Ok(bytes)
    }
}

#[test]
fn session_preview_keeps_store_bytes_and_reports_only_prospective_counts() -> TestResult {
    let fixture = Fixture::new()?;
    let before = fixture.bytes()?;
    let report = withdraw_cass_session_evidence(&fixture.options(true))?;
    assert_eq!(report.schema, EVIDENCE_SESSION_WITHDRAW_SCHEMA_V1);
    assert_eq!(report.status, "would_withdraw");
    assert!(report.dry_run && report.would_change);
    assert!(!report.persisted && !report.changed);
    assert_eq!(report.retained_evidence_count, 2);
    assert_eq!(report.cass_evidence_count, 2);
    assert_eq!(report.would_withdraw_count, 2);
    assert_eq!(report.withdrawn_count, 0);
    assert_eq!(report.index_status, "dry_run_not_queued");
    assert!(report.audit_id.is_none() && report.index_job_id.is_none());
    assert_eq!(report.scope, "retained_sources");
    assert_eq!(report.future_imports, "unchanged");
    assert_eq!(fixture.bytes()?, before);
    assert!(!fixture.root.path().join(".ee").exists());
    let json = serde_json::to_string(&report)?;
    assert!(!json.contains(SOURCE));
    assert!(!json.contains(fixture.options(true).reason));
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    for id in &fixture.evidence {
        assert!(db.get_search_admitted_evidence_span(id, WS)?.is_some());
    }
    assert!(db.list_audit_entries(Some(WS), None)?.is_empty());
    assert!(db.list_search_index_jobs(WS, None)?.is_empty());
    Ok(())
}

#[test]
fn session_commit_and_idempotent_repeat_return_distinct_honest_receipts() -> TestResult {
    let fixture = Fixture::new()?;
    let report = withdraw_cass_session_evidence(&fixture.options(false))?;
    assert_eq!(report.status, "withdrawn");
    assert!(report.persisted && report.changed && report.would_change);
    assert_eq!(report.withdrawn_count, 2);
    assert_eq!(report.would_withdraw_count, 2);
    assert_eq!(report.index_status, "queued");
    assert!(report.audit_id.is_some() && report.index_job_id.is_some());
    for preview in [false, true] {
        let report = withdraw_cass_session_evidence(&fixture.options(preview))?;
        assert_eq!(report.status, "already_withdrawn");
        assert!(!report.persisted && !report.changed && !report.would_change);
        assert_eq!(report.withdrawn_count, 0);
        assert_eq!(report.would_withdraw_count, 0);
        assert_eq!(report.already_withdrawn_count, 2);
        assert_eq!(report.index_status, "not_scheduled");
        assert!(report.audit_id.is_none() && report.index_job_id.is_none());
    }
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    assert_eq!(db.list_audit_entries(Some(WS), None)?.len(), 1);
    assert_eq!(db.list_search_index_jobs(WS, None)?.len(), 1);
    for id in &fixture.evidence {
        assert!(db.get_search_admitted_evidence_span(id, WS)?.is_none());
        assert_eq!(
            db.get_evidence_span(id)?.ok_or("retained source")?.excerpt,
            SOURCE
        );
    }
    Ok(())
}

#[test]
fn session_withdrawal_validates_input_and_owner_without_mutating_sources() -> TestResult {
    let fixture = Fixture::new()?;
    let other = tempfile::tempdir()?;
    let db = DbConnection::open_file(&fixture.database)?;
    db.insert_workspace(
        OTHER,
        &CreateWorkspaceInput {
            path: other.path().to_string_lossy().into_owned(),
            name: None,
        },
    )?;
    db.close()?;
    for preview in [false, true] {
        for id in ["sess_invalid".to_owned(), format!(" {}", fixture.session)] {
            assert!(matches!(
                withdraw_cass_session_evidence(&EvidenceSessionWithdrawOptions {
                    session_id: &id,
                    ..fixture.options(preview)
                }),
                Err(DomainError::Usage { .. })
            ));
        }
        assert!(matches!(
            withdraw_cass_session_evidence(&EvidenceSessionWithdrawOptions {
                reason: " ",
                ..fixture.options(preview)
            }),
            Err(DomainError::Usage { .. })
        ));
        assert!(matches!(
            withdraw_cass_session_evidence(&EvidenceSessionWithdrawOptions {
                workspace_path: other.path(),
                ..fixture.options(preview)
            }),
            Err(DomainError::NotFound { .. })
        ));
    }
    // Write-mode opens may create lock sidecars. Source rows and receipts,
    // rather than incidental writer locks, are the non-mutation contract here.
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    assert!(db.list_audit_entries(Some(WS), None)?.is_empty());
    assert!(db.list_search_index_jobs(WS, None)?.is_empty());
    for id in &fixture.evidence {
        assert!(db.get_search_admitted_evidence_span(id, WS)?.is_some());
    }
    Ok(())
}

#[test]
fn session_preview_refuses_missing_and_unmigrated_stores_without_bootstrap() -> TestResult {
    let root = tempfile::tempdir()?;
    let id = SessionId::now().to_string();
    let options = EvidenceSessionWithdrawOptions {
        workspace_path: root.path(),
        database_path: None,
        session_id: &id,
        reason: "Withdraw retained observations.",
        actor: None,
        dry_run: true,
    };
    assert!(matches!(
        withdraw_cass_session_evidence(&options),
        Err(DomainError::WorkspaceStoreMissing { .. })
    ));
    assert!(!root.path().join(".ee").exists());
    let database = root.path().join("old.db");
    let db = DbConnection::open_file(&database)?;
    db.execute_raw("CREATE TABLE original_marker (value TEXT)")?;
    db.close()?;
    let before = std::fs::read(&database)?;
    assert!(matches!(
        withdraw_cass_session_evidence(&EvidenceSessionWithdrawOptions {
            database_path: Some(&database),
            ..options
        }),
        Err(DomainError::MigrationRequired { .. })
    ));
    assert_eq!(std::fs::read(&database)?, before);
    assert_eq!(
        DbConnection::open_file_read_only(&database)?.schema_version()?,
        None
    );
    Ok(())
}
