//! Real restore/publication checks, including corruptions that preserve counts.

use super::tests::{fixture, recovery_agent_profile, recovery_feedback, recovery_rule};
use super::*;
use crate::models::{MemoryId, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), String>;

fn learning_fixture(
    redaction: RedactionLevel,
) -> Result<(tempfile::TempDir, PathBuf, BackupRestoreOptions), String> {
    let (root, workspace, database) = fixture().map_err(|e| e.message())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let rule = recovery_rule(&workspace_id, 0);
    db.with_transaction(|| {
        db.insert_procedural_rule_for_recovery(&rule)?;
        db.insert_procedural_rule_for_recovery(&recovery_rule(&workspace_id, 1))?;
        db.restore_rule_source(&rule.id, &memory_id)?;
        db.restore_rule_tag(&rule.id, "release")?;
        db.insert_feedback_event_for_recovery(&recovery_feedback(&workspace_id, &memory_id, 1))?;
        db.insert_agent_context_profile_for_recovery(&recovery_agent_profile(
            &workspace_id,
            &memory_id,
            "RecoveryAgent",
        ))
    })
    .map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
    let backup = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        output_dir: None,
        label: None,
        redaction_level: redaction,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.message())?;
    assert!(backup.recovery_inventory.snapshot_coverage_complete);
    let restore = BackupRestoreOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(backup.backup_path),
        side_path: root.path().join("restored"),
        restore_graph_cache: false,
        dry_run: false,
    };
    Ok((root, database, restore))
}

#[test]
fn learned_content_survives_restore_and_rebackup_without_replaying_feedback() -> TestResult {
    for redaction in [
        RedactionLevel::None,
        RedactionLevel::Standard,
        RedactionLevel::Strict,
    ] {
        let (root, _source_database, mut options) = learning_fixture(redaction)?;
        for generation in 0..2 {
            let restored = restore_backup_to_side_path(&options).map_err(|e| e.message())?;
            assert_eq!(restored.restored_rule_count, 2);
            assert_eq!(restored.restored_rule_source_count, 1);
            assert_eq!(restored.restored_rule_tag_count, 1);
            assert_eq!(restored.restored_feedback_count, 1);
            assert_eq!(restored.restored_agent_profile_count, 1);
            let db = DbConnection::open_file(&restored.restored_database_path)
                .map_err(|e| e.to_string())?;
            let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
            let original = recovery_rule(&workspace_id, 0);
            assert_eq!(
                db.get_procedural_rule(&original.id)
                    .map_err(|e| e.to_string())?,
                Some(original)
            );
            let feedback = db
                .list_feedback_events(&workspace_id)
                .map_err(|e| e.to_string())?;
            assert_eq!(feedback.len(), 1);
            assert_eq!(
                feedback[0].applied_at.as_deref(),
                Some("2026-09-01T00:01:00Z")
            );
            assert_eq!(feedback[0].weight, 0.5);
            let profiles = db
                .list_agent_context_profiles_for_recovery(&workspace_id)
                .map_err(|e| e.to_string())?;
            assert_eq!(profiles.len(), 1);
            assert_eq!(profiles[0].weight_cached, 0.04);
            db.close().map_err(|e| e.to_string())?;
            if generation == 0 {
                let backup = create_backup(&BackupCreateOptions {
                    workspace_path: options.side_path.clone(),
                    database_path: Some(PathBuf::from(restored.restored_database_path)),
                    output_dir: None,
                    label: None,
                    redaction_level: redaction,
                    include_derived: false,
                    include_graph_cache: false,
                    dry_run: false,
                })
                .map_err(|e| e.message())?;
                options.workspace_path = options.side_path;
                options.backup_path = PathBuf::from(backup.backup_path);
                options.side_path = root.path().join("restored-again");
            }
        }
    }
    Ok(())
}

fn assert_same_count_corruption_refused(table: &str, sql: &str) -> TestResult {
    let (_root, source_database, options) = learning_fixture(RedactionLevel::None)?;
    let error = restore_backup_to_side_path_with_verification_hook(&options, |path| {
        let db = DbConnection::open_file(path).map_err(work_history_error)?;
        let tables = db.list_user_tables().map_err(work_history_error)?;
        let before = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        db.execute_raw(sql).map_err(work_history_error)?;
        let after = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        assert_eq!(
            before, after,
            "corruption must preserve every table's count"
        );
        db.close().map_err(work_history_error)
    })
    .err()
    .ok_or_else(|| format!("published changed {table} content"))?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!error.message().contains("MUTATION_SENTINEL"));
    assert!(!error.message().contains("RecoveryAgent"));
    assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
    let db = DbConnection::open_file(&source_database).map_err(|e| e.to_string())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let original = recovery_rule(&workspace_id, 0);
    assert_eq!(
        db.get_procedural_rule(&original.id)
            .map_err(|e| e.to_string())?,
        Some(original)
    );
    db.close().map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: options.workspace_path,
        backup_path: options.backup_path,
    })
    .map_err(|e| e.message())?;
    assert_eq!(verified.status, "verified");
    Ok(())
}

#[test]
fn learned_content_fence_rejects_changed_rule_authority() -> TestResult {
    assert_same_count_corruption_refused(
        "procedural_rules",
        "UPDATE procedural_rules SET protected = 0, content = 'MUTATION_SENTINEL'",
    )
}

#[test]
fn learned_content_fence_rejects_reassigned_rule_evidence() -> TestResult {
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let other = recovery_rule(&workspace_id, 1).id;
    assert_same_count_corruption_refused(
        "rule_source_memories",
        &format!("UPDATE rule_source_memories SET rule_id = '{other}'"),
    )
}

#[test]
fn learned_content_fence_rejects_changed_rule_tags() -> TestResult {
    assert_same_count_corruption_refused(
        "rule_tags",
        "UPDATE rule_tags SET tag = 'MUTATION_SENTINEL'",
    )
}

#[test]
fn learned_content_fence_rejects_feedback_reapplication() -> TestResult {
    assert_same_count_corruption_refused(
        "feedback_events",
        "UPDATE feedback_events SET applied_at = NULL",
    )
}

#[test]
fn learned_content_fence_rejects_changed_agent_bias() -> TestResult {
    // Stay inside the +/-0.05 schema bound, so this exercises the publication
    // fence after a successful write rather than an earlier CHECK refusal.
    assert_same_count_corruption_refused(
        "agent_context_profiles",
        "UPDATE agent_context_profiles SET weight_cached = -0.04",
    )
}

mod transcript_publication {
    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, EvidenceProducerKind, StoredSession,
    };
    use crate::models::{EvidenceId, SessionId};

    struct TranscriptFixture {
        root: tempfile::TempDir,
        database: PathBuf,
        options: BackupRestoreOptions,
        workspace_id: String,
        source_session: StoredSession,
        source_spans: Vec<StoredEvidenceSpan>,
        expected_session: StoredSession,
        expected_spans: Vec<BackupCassEvidenceRecord>,
    }

    fn transcript_fixture() -> Result<TranscriptFixture, String> {
        let (root, workspace, database) = fixture().map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let session_id = SessionId::from_uuid(Uuid::from_u128(501)).to_string();
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.insert_session(
            &session_id,
            &CreateSessionInput {
                workspace_id: workspace_id.clone(),
                cass_session_id: "/portable-fixture/session.jsonl".to_owned(),
                source_path: Some("/portable-fixture/source.jsonl".to_owned()),
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 257,
                token_count: None,
                content_hash: hash_bytes(b"recovery transcript fixture"),
                metadata_json: None,
            },
        )
        .map_err(|e| e.to_string())?;
        for n in (1..=257_u32).rev() {
            let id = EvidenceId::from_uuid(Uuid::from_u128(u128::from(n))).to_string();
            let body = format!("Recovery message {n}: {}", "界".repeat(1024));
            db.insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: workspace_id.clone(),
                    session_id: session_id.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("source-line-{n}"),
                    span_kind: "message".to_owned(),
                    start_line: n,
                    end_line: n,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    content_hash: hash_bytes(body.as_bytes()),
                    excerpt: body,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            if n % 2 == 0 {
                db.execute_raw(&format!("UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied' WHERE id = '{id}'")).map_err(|e| e.to_string())?;
            }
        }
        let source_session = db
            .get_session(&session_id)
            .map_err(|e| e.to_string())?
            .ok_or("session missing")?;
        let source_spans = db
            .list_evidence_spans_for_workspace(&workspace_id)
            .map_err(|e| e.to_string())?;
        let expected_session = BackupCassSessionRecord::from_stored(&source_session)
            .into_restored(workspace_id.clone());
        let expected_spans = source_spans
            .iter()
            .map(|span| {
                let admitted =
                    span.is_derivation_admitted_for_session(&workspace_id, &source_session);
                let mut row = BackupCassEvidenceRecord::from_stored(span);
                row.redact_for_export(RedactionLevel::None, admitted);
                row
            })
            .collect();
        db.close().map_err(|e| e.to_string())?;
        let backup = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        assert!(backup.recovery_inventory.snapshot_coverage_complete);
        let options = BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(backup.backup_path),
            side_path: root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("restored-transcript"),
            restore_graph_cache: false,
            dry_run: false,
        };
        Ok(TranscriptFixture {
            root,
            database,
            options,
            workspace_id,
            source_session,
            source_spans,
            expected_session,
            expected_spans,
        })
    }

    fn assert_source_unchanged(source: &TranscriptFixture) -> TestResult {
        let db = DbConnection::open_file(&source.database).map_err(|e| e.to_string())?;
        assert_eq!(
            db.get_session(&source.source_session.id)
                .map_err(|e| e.to_string())?,
            Some(source.source_session.clone())
        );
        assert_eq!(
            db.list_evidence_spans_for_workspace(&source.workspace_id)
                .map_err(|e| e.to_string())?,
            source.source_spans
        );
        db.close().map_err(|e| e.to_string())
    }

    fn assert_restored(source: &TranscriptFixture, database: &Path) -> TestResult {
        let db = DbConnection::open_file(database).map_err(|e| e.to_string())?;
        assert_eq!(
            db.get_session(&source.expected_session.id)
                .map_err(|e| e.to_string())?,
            Some(source.expected_session.clone())
        );
        let actual = db
            .list_evidence_spans_for_workspace(&source.workspace_id)
            .map_err(|e| e.to_string())?;
        let records: Vec<_> = actual
            .iter()
            .map(BackupCassEvidenceRecord::from_stored)
            .collect();
        assert_eq!(records, source.expected_spans);
        assert_eq!(actual.len(), 257);
        assert_eq!(
            actual
                .iter()
                .filter(|row| row.pack_eligibility == "denied")
                .count(),
            128
        );
        assert_eq!(
            db.count_table_rows("sessions").map_err(|e| e.to_string())?,
            1
        );
        assert_eq!(
            db.count_table_rows("evidence_spans")
                .map_err(|e| e.to_string())?,
            257
        );
        db.close().map_err(|e| e.to_string())
    }

    #[test]
    fn multi_chunk_transcripts_survive_both_fences_and_a_second_recovery_generation() -> TestResult
    {
        let source = transcript_fixture()?;
        let first = restore_backup_to_side_path(&source.options).map_err(|e| e.message())?;
        assert_eq!(first.restored_cass_session_count, 1);
        assert_eq!(first.restored_evidence_span_count, 257);
        assert_restored(&source, Path::new(&first.restored_database_path))?;
        assert_source_unchanged(&source)?;
        let backup = create_backup(&BackupCreateOptions {
            workspace_path: source.options.side_path.clone(),
            database_path: Some(PathBuf::from(first.restored_database_path)),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::None,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        let second = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: source.options.side_path.clone(),
            backup_path: PathBuf::from(backup.backup_path),
            side_path: source
                .root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("restored-transcript-again"),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        assert_restored(&source, Path::new(&second.restored_database_path))?;
        assert_source_unchanged(&source)
    }

    fn refuse_late_transcript_change(table: &str, sql: &str) -> TestResult {
        let source = transcript_fixture()?;
        let mut staged = None;
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &source.options,
            |_| Ok(()),
            |path| {
                assert!(
                    path.parent()
                        .ok_or_else(|| work_history_error("missing staged parent"))?
                        .join("index/meta.json")
                        .is_file()
                );
                staged = Some(path.to_path_buf());
                let db = DbConnection::open_file(path).map_err(work_history_error)?;
                let count = db.count_table_rows(table).map_err(work_history_error)?;
                db.execute_raw(sql).map_err(work_history_error)?;
                assert_eq!(
                    db.count_table_rows(table).map_err(work_history_error)?,
                    count
                );
                db.close().map_err(work_history_error)
            },
        )
        .err()
        .ok_or("published a modified transcript")?;
        assert!(
            error
                .message()
                .contains(&format!("content differs for {table}")),
            "{}",
            error.message()
        );
        assert!(!error.message().contains("PRIVATE_TRANSCRIPT_MUTATION"));
        assert!(!source.options.side_path.join(WORKSPACE_MARKER).exists());
        assert!(
            staged
                .ok_or("final publication hook did not execute")?
                .is_file()
        );
        assert_source_unchanged(&source)?;
        assert_eq!(
            verify_backup(&BackupVerifyOptions {
                workspace_path: source.options.workspace_path.clone(),
                backup_path: source.options.backup_path.clone(),
            })
            .map_err(|e| e.message())?
            .status,
            "verified"
        );
        let mut retry = source.options.clone();
        retry.side_path.set_file_name("restored-transcript-retry");
        let restored = restore_backup_to_side_path(&retry).map_err(|e| e.message())?;
        assert_restored(&source, Path::new(&restored.restored_database_path))
    }

    #[test]
    fn last_transcript_body_changed_after_rebuild_is_not_published() -> TestResult {
        let last = EvidenceId::from_uuid(Uuid::from_u128(257)).to_string();
        refuse_late_transcript_change(
            "evidence_spans",
            &format!(
                "UPDATE evidence_spans SET excerpt = 'PRIVATE_TRANSCRIPT_MUTATION' WHERE id = '{last}'"
            ),
        )
    }

    #[test]
    fn restored_session_cannot_regain_a_host_path_after_rebuild() -> TestResult {
        refuse_late_transcript_change(
            "sessions",
            "UPDATE sessions SET source_path = '/PRIVATE_TRANSCRIPT_MUTATION'",
        )
    }
}
