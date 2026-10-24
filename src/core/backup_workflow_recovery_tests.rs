//! Whole-store recovery must leave workflow recall and completion usable.
//! Fault controls alter membership but not a single durable row count.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::core::memory::{WorkflowCloseOptions, close_workflow};
use crate::core::why::{WhyOptions, explain_memory_with_connection};
use crate::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreateSearchIndexJobInput, CreateSessionInput,
    EvidenceProducerKind, SearchIndexJobType,
};
use crate::models::{EvidenceId, MemoryId, SessionId, WorkspaceId};
use uuid::Uuid;

const TIME: &str = "2026-09-01T00:00:00+00:00";

fn id(n: u128) -> String {
    MemoryId::from_uuid(Uuid::from_u128(100 + n)).to_string()
}

fn workspace_id() -> String {
    WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string()
}

fn source(private: bool) -> (tempfile::TempDir, PathBuf, PathBuf, String, String) {
    let (root, workspace, database) = tests::fixture().unwrap();
    let a = if private {
        "api_key=WORKFLOW_RECOVERY_PRIVATE_A"
    } else {
        "workflow-a"
    }
    .to_owned();
    let b = if private {
        "api_key=WORKFLOW_RECOVERY_PRIVATE_B"
    } else {
        "workflow-b"
    }
    .to_owned();
    let db = DbConnection::open_file(&database).unwrap();
    for n in 0..6 {
        db.insert_memory_with_timestamps(
            &id(n),
            &CreateMemoryInput {
                workspace_id: workspace_id(),
                level: "working".to_owned(),
                kind: "observation".to_owned(),
                content: format!("Workflow recovery observation {n}."),
                workflow_id: match n {
                    2 => Some(b.clone()),
                    3 => None,
                    _ => Some(a.clone()),
                },
                confidence: 0.8,
                utility: 0.7,
                importance: if n == 5 { 0.2 } else { 0.8 },
                provenance_uri: Some("ee-test://workflow-recovery".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: vec![],
                valid_from: Some("2026-09-01T00:00:00Z".to_owned()),
                valid_to: None,
            },
            TIME,
            TIME,
            &id(n),
        )
        .unwrap();
    }
    db.restore_imported_memory_tombstone(&id(4), TIME).unwrap();
    db.close().unwrap();
    (root, workspace, database, a, b)
}

fn create(workspace: &Path, database: &Path, level: RedactionLevel) -> BackupCreateReport {
    create_backup(&BackupCreateOptions {
        workspace_path: workspace.to_owned(),
        database_path: Some(database.to_owned()),
        output_dir: None,
        label: None,
        redaction_level: level,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .unwrap()
}

fn options(
    root: &Path,
    workspace: &Path,
    backup: &BackupCreateReport,
    name: &str,
) -> BackupRestoreOptions {
    BackupRestoreOptions {
        workspace_path: workspace.to_owned(),
        backup_path: PathBuf::from(&backup.backup_path),
        side_path: root.canonicalize().unwrap().join(name),
        restore_graph_cache: false,
        dry_run: false,
    }
}

fn evidence_source() -> (tempfile::TempDir, PathBuf, PathBuf, Vec<StoredEvidenceSpan>) {
    let (root, workspace, database) = tests::fixture().unwrap();
    let db = DbConnection::open_file(&database).unwrap();
    let session_id = SessionId::from_uuid(Uuid::from_u128(10)).to_string();
    db.insert_session(
        &session_id,
        &CreateSessionInput {
            workspace_id: workspace_id(),
            cass_session_id: "cass://recovery-fidelity".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: Some(TIME.to_owned()),
            ended_at: None,
            message_count: 4,
            token_count: None,
            content_hash: hash_bytes(b"recovery-fidelity-session"),
            metadata_json: None,
        },
    )
    .unwrap();
    for (index, (producer, role, body)) in [
        (
            EvidenceProducerKind::CassImport,
            "assistant",
            "The release checks passed.",
        ),
        (
            EvidenceProducerKind::CassImport,
            "assistant",
            "The credential parser passed the release checks.",
        ),
        (
            EvidenceProducerKind::CassImport,
            "system",
            "Release environment metadata.",
        ),
        (
            EvidenceProducerKind::DocsBootstrap,
            "docs_bootstrap",
            "Documentation described the release checks.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let excerpt = if producer == EvidenceProducerKind::CassImport {
            json!({"type": role, "message": {"role": role, "content": body}}).to_string()
        } else {
            body.to_owned()
        };
        db.insert_evidence_span(
            &EvidenceId::from_uuid(Uuid::from_u128(20 + index as u128)).to_string(),
            &CreateEvidenceSpanInput {
                workspace_id: workspace_id(),
                session_id: session_id.clone(),
                memory_id: None,
                producer_kind: producer,
                cass_span_id: format!("cass://recovery-fidelity:{index}"),
                span_kind: "message".to_owned(),
                start_line: index as u32 + 1,
                end_line: index as u32 + 1,
                start_byte: None,
                end_byte: None,
                role: Some(role.to_owned()),
                content_hash: hash_bytes(excerpt.as_bytes()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: vec![],
            },
        )
        .unwrap();
    }
    let rows = db
        .list_evidence_spans_for_workspace(&workspace_id())
        .unwrap();
    let session = db.get_session(&session_id).unwrap().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .filter(|row| row.pack_eligibility == "admitted")
            .count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.pack_eligibility == "quarantined")
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.pack_eligibility == "denied")
            .count(),
        1
    );
    assert!(
        rows.iter()
            .filter(|row| row.pack_eligibility == "admitted")
            .all(|row| { row.is_direct_pack_admitted_for_session(&workspace_id(), &session) })
    );
    db.close().unwrap();
    (root, workspace, database, rows)
}

#[test]
fn evidence_admission_fidelity_round_trips_none_and_warns_for_minimal() {
    let (root, workspace, database, before) = evidence_source();
    for level in [RedactionLevel::None, RedactionLevel::Minimal] {
        let lossy = level == RedactionLevel::Minimal;
        let backup = create(&workspace, &database, level);
        let summary = backup
            .recovery_inventory
            .evidence_admission
            .as_ref()
            .unwrap();
        let source_counts = BackupEvidenceEligibilityCounts {
            admitted: 2,
            quarantined: 1,
            denied: 1,
        };
        let exported_counts = if lossy {
            BackupEvidenceEligibilityCounts {
                admitted: 1,
                quarantined: 0,
                denied: 3,
            }
        } else {
            source_counts.clone()
        };
        assert_eq!(summary.source_search, source_counts);
        assert_eq!(summary.source_pack, source_counts);
        assert_eq!(summary.exported_search, exported_counts);
        assert_eq!(summary.exported_pack, exported_counts);
        let mut manifest = read_backup_manifest(Path::new(&backup.backup_path))
            .unwrap()
            .1;
        assert_eq!(
            manifest["recoveryInventory"]["schema"],
            "ee.backup.recovery_inventory.v2"
        );
        assert_eq!(
            manifest["recoveryInventory"]["rowCountBasis"],
            "source_snapshot"
        );
        let warning = backup
            .degraded
            .iter()
            .find(|entry| entry.code == "backup_evidence_admission_reduced");
        assert_eq!(warning.is_some(), lossy);
        if let Some(warning) = warning {
            assert!(
                warning
                    .message
                    .contains("2 evidence span(s) to pack-denied")
            );
            assert!(warning.next_action.contains("--redaction none"));
        }
        if lossy {
            // The measured state, rather than a copied warning string, must
            // keep inspection and restore honest for another valid producer.
            manifest["degraded"] = json!([]);
            let key = StoreAuthRoot::open(workspace_keys_dir(&workspace)).unwrap();
            authenticate_backup_manifest(&mut manifest, &key).unwrap();
            fs::write(
                &backup.manifest_path,
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
        }
        let inspected = inspect_backup(&BackupInspectOptions {
            backup_path: PathBuf::from(&backup.backup_path),
        })
        .unwrap();
        assert_eq!(
            inspected
                .degraded
                .iter()
                .any(|entry| entry.code == "backup_evidence_admission_reduced"),
            lossy
        );
        let verified = verify_backup(&BackupVerifyOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&backup.backup_path),
        })
        .unwrap();
        assert_eq!(verified.status, if lossy { "degraded" } else { "verified" });
        assert_eq!(
            verified
                .issues
                .iter()
                .any(|issue| issue.code == "backup_evidence_admission_reduced"),
            lossy
        );
        let opts = options(
            root.path(),
            &workspace,
            &backup,
            if lossy { "minimal" } else { "none" },
        );
        let restored = restore_backup_to_side_path(&opts).unwrap();
        assert_eq!(restored.restored_evidence_span_count, 4);
        assert_eq!(
            restored
                .degraded
                .iter()
                .any(|entry| entry.code == "backup_evidence_admission_reduced"),
            lossy
        );
        let db = DbConnection::open_file(&restored.restored_database_path).unwrap();
        for asset in backup
            .derived
            .iter()
            .filter(|asset| asset.kind == "cass_evidence_spans")
        {
            let chunk: BackupCassEvidenceChunk = serde_json::from_slice(
                &fs::read(Path::new(&backup.backup_path).join(&asset.path)).unwrap(),
            )
            .unwrap();
            for row in chunk.evidence_spans {
                let restored_row = db.get_evidence_span(&row.id).unwrap().unwrap();
                assert_eq!(BackupCassEvidenceRecord::from_stored(&restored_row), row);
            }
        }
        db.close().unwrap();
        let source = DbConnection::open_file(&database).unwrap();
        assert_eq!(
            source
                .list_evidence_spans_for_workspace(&workspace_id())
                .unwrap(),
            before
        );
        source.close().unwrap();
    }
}

#[test]
fn evidence_admission_verification_refuses_signed_counts_that_disagree_with_artifacts() {
    let (root, workspace, database, _) = evidence_source();
    let backup = create(&workspace, &database, RedactionLevel::Minimal);
    let mut manifest = read_backup_manifest(Path::new(&backup.backup_path))
        .unwrap()
        .1;
    // These are internally consistent counts, with a real source signature,
    // but they falsely claim that the redacted artifact retained eligibility.
    let summary = &mut manifest["recoveryInventory"]["evidenceAdmission"];
    summary["exportedSearch"] = summary["sourceSearch"].clone();
    summary["exportedPack"] = summary["sourcePack"].clone();
    let key = StoreAuthRoot::open(workspace_keys_dir(&workspace)).unwrap();
    authenticate_backup_manifest(&mut manifest, &key).unwrap();
    verify_backup_manifest_authentication(&workspace, &manifest).unwrap();
    fs::write(
        &backup.manifest_path,
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: workspace.clone(),
        backup_path: PathBuf::from(&backup.backup_path),
    })
    .unwrap();
    assert_eq!(verified.status, "failed");
    assert!(
        verified
            .issues
            .iter()
            .any(|issue| issue.code == "manifest_evidence_admission_invalid")
    );
    assert!(
        !verified
            .issues
            .iter()
            .any(|issue| issue.code == "manifest_authentication_failed")
    );
    let opts = options(root.path(), &workspace, &backup, "rejected-admission-claim");
    assert!(restore_backup_to_side_path(&opts).is_err());
    assert!(!opts.side_path.exists());
}

#[test]
fn two_recovery_generations_preserve_membership_why_and_workflow_completion() {
    for level in [
        RedactionLevel::None,
        RedactionLevel::Minimal,
        RedactionLevel::Standard,
        RedactionLevel::Strict,
    ] {
        let (root, mut workspace, mut database, original_a, original_b) =
            source(level != RedactionLevel::None);
        let a = redact_recovery_identity(&original_a, level);
        let b = redact_recovery_identity(&original_b, level);
        assert_ne!(a, b);
        let mut first_archive = None;
        for generation in 0..2 {
            // bd-fdw88 DIAGNOSTIC: this test performs 4 redaction levels x 2 generations of
            // full backup/restore. Before the promotion reindex was wired it died at the
            // first generation of the first level, so its real cost was never observed. With
            // `--nocapture` these lines distinguish "slow but progressing" from "stalled in
            // one cycle", which a bare timeout cannot.
            eprintln!("[bd-fdw88] level={level:?} generation={generation} starting");
            let source_db = DbConnection::open_file(&database).unwrap();
            let before = source_db
                .list_memories(&workspace_id(), None, true)
                .unwrap();
            source_db.close().unwrap();
            let backup = create(&workspace, &database, level);
            assert_ne!(first_archive.as_deref(), Some(backup.backup_id.as_str()));
            first_archive = Some(backup.backup_id.clone());
            let text =
                fs::read_to_string(Path::new(&backup.backup_path).join(RECORDS_FILE)).unwrap();
            if level != RedactionLevel::None {
                assert!(!text.contains("WORKFLOW_RECOVERY_PRIVATE"));
            }
            let exported = text
                .lines()
                .filter_map(|line| {
                    let row: JsonValue = serde_json::from_str(line).unwrap();
                    (row["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
                        .then(|| serde_json::from_value::<ExportMemoryRecord>(row).unwrap())
                })
                .collect::<Vec<_>>();
            assert_eq!(exported.len(), before.len());
            let opts = options(
                root.path(),
                &workspace,
                &backup,
                &format!("restored-{generation}"),
            );
            let restored = restore_backup_to_side_path(&opts).unwrap();
            let db = DbConnection::open_file(&restored.restored_database_path).unwrap();
            for expected in &exported {
                let restored_id = import_memory_id(expected, level).unwrap();
                let row = db.get_memory(&restored_id).unwrap().unwrap();
                assert_eq!(row.workflow_id, expected.workflow_id);
                assert_eq!(row.level, expected.level);
                assert_eq!(row.tombstoned_at, expected.tombstoned_at);
            }
            let members = db
                .list_recent_workflow_memories(&workspace_id(), &a, "not-a-memory", 20)
                .unwrap();
            assert_eq!(
                members.len(),
                3,
                "dead and unassigned memories must not join the active workflow"
            );
            let others = db
                .list_recent_workflow_memories(&workspace_id(), &b, "not-a-memory", 20)
                .unwrap();
            assert_eq!(others.len(), 1);
            let why = explain_memory_with_connection(
                &WhyOptions {
                    database_path: Path::new(&restored.restored_database_path),
                    memory_id: &members[0].id,
                    confidence_threshold: 0.5,
                },
                &db,
            );
            assert!(why.found);
            assert_eq!(
                why.storage.unwrap().workflow_id.as_deref(),
                Some(a.as_str())
            );
            db.close().unwrap();
            // Prove that recovery did not rewrite the source, then use the
            // ordinary public completion operation on the restored store.
            let source_db = DbConnection::open_file(&database).unwrap();
            assert_eq!(
                source_db
                    .list_memories(&workspace_id(), None, true)
                    .unwrap(),
                before
            );
            source_db.close().unwrap();
            let restored_path = PathBuf::from(&restored.restored_database_path);
            let closed = close_workflow(&WorkflowCloseOptions {
                workspace_path: &opts.side_path,
                database_path: Some(&restored_path),
                workflow_id: &a,
            })
            .unwrap();
            assert_eq!(closed.promoted_count, if generation == 0 { 2 } else { 0 });
            assert_eq!(closed.audit_ids.len(), closed.promoted_memory_ids.len());
            let db = DbConnection::open_file(&restored_path).unwrap();
            for promoted in &closed.promoted_memory_ids {
                let row = db.get_memory(promoted).unwrap().unwrap();
                assert_eq!(row.level, "episodic");
                assert_eq!(row.workflow_id.as_deref(), Some(a.as_str()));
                assert!(
                    db.list_pending_search_index_jobs(&workspace_id(), None)
                        .unwrap()
                        .iter()
                        .any(|job| job.document_id.as_deref() == Some(promoted.as_str()))
                );
            }
            assert_eq!(
                db.get_memory(&others[0].id).unwrap().unwrap().level,
                "working"
            );
            db.close().unwrap();
            workspace = opts.side_path;
            database = restored_path;
        }
    }
}

#[test]
fn workflow_fence_rejects_lost_added_or_reassigned_membership_without_count_changes() {
    for sql in [
        format!(
            "UPDATE memories SET workflow_id = NULL WHERE id = '{}'",
            id(0)
        ),
        format!(
            "UPDATE memories SET workflow_id = 'workflow-b' WHERE id = '{}'",
            id(0)
        ),
        format!(
            "UPDATE memories SET workflow_id = 'workflow-a' WHERE id = '{}'",
            id(3)
        ),
    ] {
        for late in [false, true] {
            // bd-fdw88 DIAGNOSTIC: 3 SQL mutations x 2 hook positions = 6 full
            // fixture/backup/restore/verify cycles. With `--nocapture` these lines separate
            // "slow but progressing" from "stalled in one cycle", which is what a bare
            // timeout cannot report. The companion loop in
            // two_recovery_generations measured ~250-280s per cycle this way.
            eprintln!("[bd-fdw88] fence sql={sql} late={late} starting");
            let (root, workspace, database, _, _) = source(false);
            let source_db = DbConnection::open_file(&database).unwrap();
            let before = source_db
                .list_memories(&workspace_id(), None, true)
                .unwrap();
            source_db.close().unwrap();
            let backup = create(&workspace, &database, RedactionLevel::None);
            let opts = options(root.path(), &workspace, &backup, "refused-restore");
            let corrupt = |path: &Path| -> Result<(), DomainError> {
                let db = DbConnection::open_file(path).map_err(work_history_error)?;
                let rows = db
                    .list_memories(&workspace_id(), None, true)
                    .map_err(work_history_error)?;
                let tables = db.list_user_tables().map_err(work_history_error)?;
                let counts = tables
                    .iter()
                    .map(|table| db.count_table_rows(table))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(work_history_error)?;
                db.execute_raw(&sql).map_err(work_history_error)?;
                assert_ne!(
                    db.list_memories(&workspace_id(), None, true)
                        .map_err(work_history_error)?,
                    rows
                );
                assert_eq!(
                    tables
                        .iter()
                        .map(|table| db.count_table_rows(table))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(work_history_error)?,
                    counts
                );
                db.close().map_err(work_history_error)
            };
            let result = restore_backup_to_side_path_with_recovery_hooks(
                &opts,
                |path| if late { Ok(()) } else { corrupt(path) },
                |path| if late { corrupt(path) } else { Ok(()) },
            );
            let error = result.expect_err("altered workflow membership must never be published");
            assert!(error.message().contains("memory"), "{}", error.message());
            assert!(!opts.side_path.join(WORKSPACE_MARKER).exists());
            let source_db = DbConnection::open_file(&database).unwrap();
            assert_eq!(
                source_db
                    .list_memories(&workspace_id(), None, true)
                    .unwrap(),
                before
            );
            source_db.close().unwrap();
            assert_eq!(
                verify_backup(&BackupVerifyOptions {
                    workspace_path: workspace,
                    backup_path: opts.backup_path
                })
                .unwrap()
                .status,
                "verified"
            );
        }
    }
}

#[test]
fn workflow_completion_queues_only_promoted_memories_and_repeated_close_is_a_noop() {
    let (_root, _workspace, database, a, _) = source(false);
    let db = DbConnection::open_file(&database).unwrap();
    let promoted = db
        .promote_workflow_working_memories_audited(&workspace_id(), &a, "test", TIME)
        .unwrap();
    assert_eq!(promoted.len(), 2);
    let jobs = db
        .list_pending_search_index_jobs(&workspace_id(), None)
        .unwrap();
    assert_eq!(
        jobs.len(),
        promoted.len(),
        "promotion must enqueue one index job per eligible memory"
    );
    for item in &promoted {
        assert!(db.get_audit(&item.audit_id).unwrap().is_some());
        assert_eq!(
            jobs.iter()
                .filter(
                    |job| job.document_id.as_deref() == Some(item.memory_id.as_str())
                        && job.document_source.as_deref() == Some("memory")
                        && job.job_type_enum() == Some(SearchIndexJobType::SingleDocument)
                        && job.documents_total == 1
                )
                .count(),
            1
        );
    }
    assert!(
        db.promote_workflow_working_memories_audited(&workspace_id(), &a, "test", TIME)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.list_pending_search_index_jobs(&workspace_id(), None)
            .unwrap(),
        jobs
    );
    assert_eq!(db.get_memory(&id(2)).unwrap().unwrap().level, "working");
    assert_eq!(db.get_memory(&id(3)).unwrap().unwrap().level, "working");
    assert_eq!(db.get_memory(&id(4)).unwrap().unwrap().level, "working");
    assert_eq!(db.get_memory(&id(5)).unwrap().unwrap().level, "working");
    db.close().unwrap();
}

#[test]
fn failed_workflow_job_insert_rolls_back_every_promotion_and_audit() {
    let (_root, _workspace, database, a, _) = source(false);
    let db = DbConnection::open_file(&database).unwrap();
    // Fail the SECOND eligible row after the first promotion, audit and job
    // have been inserted, without mocking any database operation.
    db.execute_raw(
        "CREATE UNIQUE INDEX workflow_test_job_target ON search_index_jobs(document_id)",
    )
    .unwrap();
    // bd-fdw88: this fixture id was "preexisting-workflow-job", which is 24 characters and
    // does not begin with "sidx_", so it violated the search_index_jobs CHECK
    // (`id GLOB 'sidx_*' AND length(id) = 31`, src/db/mod.rs:3821) and this test panicked in
    // its own SETUP, never reaching the rollback assertion it exists for. Nothing caught it
    // because the file was mounted by nothing and had never compiled. 31 characters, correct
    // prefix.
    db.insert_search_index_job(
        "sidx_preexisting000000000000000",
        &CreateSearchIndexJobInput {
            workspace_id: workspace_id(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(id(1)),
            documents_total: 1,
        },
    )
    .unwrap();
    let before = db.list_memories(&workspace_id(), None, true).unwrap();
    let audit_count = db.count_table_rows("audit_log").unwrap();
    let jobs = db
        .list_pending_search_index_jobs(&workspace_id(), None)
        .unwrap();
    assert!(
        db.promote_workflow_working_memories_audited(&workspace_id(), &a, "test", TIME)
            .is_err()
    );
    assert_eq!(
        db.list_memories(&workspace_id(), None, true).unwrap(),
        before
    );
    assert_eq!(db.count_table_rows("audit_log").unwrap(), audit_count);
    assert_eq!(
        db.list_pending_search_index_jobs(&workspace_id(), None)
            .unwrap(),
        jobs
    );
    db.close().unwrap();
}
