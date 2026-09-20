//! Whole-store recovery must leave workflow recall and completion usable.
//! Fault controls alter membership but not a single durable row count.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::core::memory::{WorkflowCloseOptions, close_workflow};
use crate::core::why::{WhyOptions, explain_memory_with_connection};
use crate::db::{CreateMemoryInput, CreateSearchIndexJobInput, SearchIndexJobType};
use crate::models::{MemoryId, WorkspaceId};
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
    let a = if private { "api_key=WORKFLOW_RECOVERY_PRIVATE_A" } else { "workflow-a" }.to_owned();
    let b = if private { "api_key=WORKFLOW_RECOVERY_PRIVATE_B" } else { "workflow-b" }.to_owned();
    let db = DbConnection::open_file(&database).unwrap();
    for n in 0..6 {
        db.insert_memory_with_timestamps(
            &id(n),
            &CreateMemoryInput {
                workspace_id: workspace_id(),
                level: "working".to_owned(),
                kind: "observation".to_owned(),
                content: format!("Workflow recovery observation {n}."),
                workflow_id: match n { 2 => Some(b.clone()), 3 => None, _ => Some(a.clone()) },
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
            TIME, TIME, &id(n),
        ).unwrap();
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
    }).unwrap()
}

fn options(root: &Path, workspace: &Path, backup: &BackupCreateReport, name: &str) -> BackupRestoreOptions {
    BackupRestoreOptions {
        workspace_path: workspace.to_owned(),
        backup_path: PathBuf::from(&backup.backup_path),
        side_path: root.canonicalize().unwrap().join(name),
        restore_graph_cache: false,
        dry_run: false,
    }
}

#[test]
fn two_recovery_generations_preserve_membership_why_and_workflow_completion() {
    for level in [RedactionLevel::None, RedactionLevel::Minimal, RedactionLevel::Standard, RedactionLevel::Strict] {
        let (root, mut workspace, mut database, original_a, original_b) = source(level != RedactionLevel::None);
        let a = redact_recovery_identity(&original_a, level);
        let b = redact_recovery_identity(&original_b, level);
        assert_ne!(a, b);
        let mut first_archive = None;
        for generation in 0..2 {
            let source_db = DbConnection::open_file(&database).unwrap();
            let before = source_db.list_memories(&workspace_id(), None, true).unwrap();
            source_db.close().unwrap();
            let backup = create(&workspace, &database, level);
            assert_ne!(first_archive.as_deref(), Some(backup.backup_id.as_str()));
            first_archive = Some(backup.backup_id.clone());
            let text = fs::read_to_string(Path::new(&backup.backup_path).join(RECORDS_FILE)).unwrap();
            if level != RedactionLevel::None {
                assert!(!text.contains("WORKFLOW_RECOVERY_PRIVATE"));
            }
            let exported = text.lines().filter_map(|line| {
                let row: JsonValue = serde_json::from_str(line).unwrap();
                (row["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
                    .then(|| serde_json::from_value::<ExportMemoryRecord>(row).unwrap())
            }).collect::<Vec<_>>();
            assert_eq!(exported.len(), before.len());
            let opts = options(root.path(), &workspace, &backup, &format!("restored-{generation}"));
            let restored = restore_backup_to_side_path(&opts).unwrap();
            let db = DbConnection::open_file(&restored.restored_database_path).unwrap();
            for expected in &exported {
                let restored_id = import_memory_id(expected, level).unwrap();
                let row = db.get_memory(&restored_id).unwrap().unwrap();
                assert_eq!(row.workflow_id, expected.workflow_id);
                assert_eq!(row.level, expected.level);
                assert_eq!(row.tombstoned_at, expected.tombstoned_at);
            }
            let members = db.list_recent_workflow_memories(&workspace_id(), &a, "not-a-memory", 20).unwrap();
            assert_eq!(members.len(), 3, "dead and unassigned memories must not join the active workflow");
            let others = db.list_recent_workflow_memories(&workspace_id(), &b, "not-a-memory", 20).unwrap();
            assert_eq!(others.len(), 1);
            let why = explain_memory_with_connection(&WhyOptions {
                database_path: Path::new(&restored.restored_database_path),
                memory_id: &members[0].id,
                confidence_threshold: 0.5,
            }, &db);
            assert!(why.found);
            assert_eq!(why.storage.unwrap().workflow_id.as_deref(), Some(a.as_str()));
            db.close().unwrap();
            // Prove that recovery did not rewrite the source, then use the
            // ordinary public completion operation on the restored store.
            let source_db = DbConnection::open_file(&database).unwrap();
            assert_eq!(source_db.list_memories(&workspace_id(), None, true).unwrap(), before);
            source_db.close().unwrap();
            let restored_path = PathBuf::from(&restored.restored_database_path);
            let closed = close_workflow(&WorkflowCloseOptions {
                workspace_path: &opts.side_path,
                database_path: Some(&restored_path),
                workflow_id: &a,
            }).unwrap();
            assert_eq!(closed.promoted_count, if generation == 0 { 2 } else { 0 });
            assert_eq!(closed.audit_ids.len(), closed.promoted_memory_ids.len());
            let db = DbConnection::open_file(&restored_path).unwrap();
            for promoted in &closed.promoted_memory_ids {
                let row = db.get_memory(promoted).unwrap().unwrap();
                assert_eq!(row.level, "episodic");
                assert_eq!(row.workflow_id.as_deref(), Some(a.as_str()));
                assert!(db.list_pending_search_index_jobs(&workspace_id(), None).unwrap().iter()
                    .any(|job| job.document_id.as_deref() == Some(promoted.as_str())));
            }
            assert_eq!(db.get_memory(&others[0].id).unwrap().unwrap().level, "working");
            db.close().unwrap();
            workspace = opts.side_path;
            database = restored_path;
        }
    }
}

#[test]
fn workflow_fence_rejects_lost_added_or_reassigned_membership_without_count_changes() {
    for sql in [
        format!("UPDATE memories SET workflow_id = NULL WHERE id = '{}'", id(0)),
        format!("UPDATE memories SET workflow_id = 'workflow-b' WHERE id = '{}'", id(0)),
        format!("UPDATE memories SET workflow_id = 'workflow-a' WHERE id = '{}'", id(3)),
    ] {
        for late in [false, true] {
            let (root, workspace, database, _, _) = source(false);
            let source_db = DbConnection::open_file(&database).unwrap();
            let before = source_db.list_memories(&workspace_id(), None, true).unwrap();
            source_db.close().unwrap();
            let backup = create(&workspace, &database, RedactionLevel::None);
            let opts = options(root.path(), &workspace, &backup, "refused-restore");
            let corrupt = |path: &Path| -> Result<(), DomainError> {
                let db = DbConnection::open_file(path).map_err(work_history_error)?;
                let rows = db.list_memories(&workspace_id(), None, true).map_err(work_history_error)?;
                let tables = db.list_user_tables().map_err(work_history_error)?;
                let counts = tables.iter().map(|table| db.count_table_rows(table)).collect::<Result<Vec<_>, _>>().map_err(work_history_error)?;
                db.execute_raw(&sql).map_err(work_history_error)?;
                assert_ne!(db.list_memories(&workspace_id(), None, true).map_err(work_history_error)?, rows);
                assert_eq!(tables.iter().map(|table| db.count_table_rows(table)).collect::<Result<Vec<_>, _>>().map_err(work_history_error)?, counts);
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
            assert_eq!(source_db.list_memories(&workspace_id(), None, true).unwrap(), before);
            source_db.close().unwrap();
            assert_eq!(verify_backup(&BackupVerifyOptions { workspace_path: workspace, backup_path: opts.backup_path }).unwrap().status, "verified");
        }
    }
}

#[test]
fn workflow_completion_queues_only_promoted_memories_and_repeated_close_is_a_noop() {
    let (_root, _workspace, database, a, _) = source(false);
    let db = DbConnection::open_file(&database).unwrap();
    let promoted = db.promote_workflow_working_memories_audited(&workspace_id(), &a, "test", TIME).unwrap();
    assert_eq!(promoted.len(), 2);
    let jobs = db.list_pending_search_index_jobs(&workspace_id(), None).unwrap();
    assert_eq!(jobs.len(), promoted.len(), "promotion must enqueue one index job per eligible memory");
    for item in &promoted {
        assert!(db.get_audit(&item.audit_id).unwrap().is_some());
        assert_eq!(jobs.iter().filter(|job| job.document_id.as_deref() == Some(item.memory_id.as_str())
            && job.document_source.as_deref() == Some("memory")
            && job.job_type_enum() == Some(SearchIndexJobType::SingleDocument)
            && job.documents_total == 1).count(), 1);
    }
    assert!(db.promote_workflow_working_memories_audited(&workspace_id(), &a, "test", TIME).unwrap().is_empty());
    assert_eq!(db.list_pending_search_index_jobs(&workspace_id(), None).unwrap(), jobs);
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
    db.execute_raw("CREATE UNIQUE INDEX workflow_test_job_target ON search_index_jobs(document_id)").unwrap();
    db.insert_search_index_job("preexisting-workflow-job", &CreateSearchIndexJobInput {
        workspace_id: workspace_id(),
        job_type: SearchIndexJobType::SingleDocument,
        document_source: Some("memory".to_owned()),
        document_id: Some(id(1)),
        documents_total: 1,
    }).unwrap();
    let before = db.list_memories(&workspace_id(), None, true).unwrap();
    let audit_count = db.count_table_rows("audit_log").unwrap();
    let jobs = db.list_pending_search_index_jobs(&workspace_id(), None).unwrap();
    assert!(db.promote_workflow_working_memories_audited(&workspace_id(), &a, "test", TIME).is_err());
    assert_eq!(db.list_memories(&workspace_id(), None, true).unwrap(), before);
    assert_eq!(db.count_table_rows("audit_log").unwrap(), audit_count);
    assert_eq!(db.list_pending_search_index_jobs(&workspace_id(), None).unwrap(), jobs);
    db.close().unwrap();
}
