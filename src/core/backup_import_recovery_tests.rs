//! Real-store resumption and destination-binding regressions at both fences.

use super::*;
use crate::core::backup::*;
use crate::db::StoredImportLedger;
use crate::models::{RedactionLevel, WorkspaceId};
use std::path::{Path, PathBuf};
use uuid::Uuid;

type TestResult = Result<(), String>;
const TIME: &str = "2026-09-01T01:00:00Z";

fn seed(db: &DbConnection, workspace: &str, path: &Path) -> Result<(), crate::db::DbError> {
    for (n, status) in ["running", "failed", "completed"].into_iter().enumerate() {
        db.insert_import_ledger_for_recovery(&StoredImportLedger {
            id: format!("imp_{n:026}"),
            workspace_id: workspace.to_owned(),
            source_kind: "cass".to_owned(),
            source_id: format!(
                "cass://sessions?workspace={}&limit={}&since=2026-09-01T00:00:00Z",
                path.display(),
                n + 7
            ),
            status: status.to_owned(),
            cursor_json: Some(r#"{"offset":42,"lastSession":"source-session"}"#.to_owned()),
            imported_session_count: 3,
            imported_span_count: 11,
            attempt_count: 2,
            error_code: (n == 1).then(|| "source_unavailable".to_owned()),
            error_message: (n == 1).then(|| "source import failed".to_owned()),
            started_at: Some(TIME.to_owned()),
            completed_at: (n != 0).then(|| TIME.to_owned()),
            metadata_json: Some(r#"{"captureVersion":2}"#.to_owned()),
            created_at: TIME.to_owned(),
            updated_at: TIME.to_owned(),
        })?;
    }
    Ok(())
}

fn snapshot(db: &DbConnection, workspace: &str) -> Result<serde_json::Value, crate::db::DbError> {
    Ok(serde_json::json!({
        "workspaces": db.list_workspaces()?,
        "imports": db.list_import_ledgers(workspace)?,
    }))
}

fn create(
    workspace: &Path,
    database: &Path,
    redaction: RedactionLevel,
) -> Result<BackupCreateReport, String> {
    create_backup(&BackupCreateOptions {
        workspace_path: workspace.to_owned(),
        database_path: Some(database.to_owned()),
        output_dir: None,
        label: None,
        redaction_level: redaction,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.message())
}

fn assert_refused(
    table: &str,
    sql: &str,
    paired_workspace: bool,
    extra_workspace: bool,
) -> TestResult {
    for late in [false, true] {
        let (root, workspace, database) =
            crate::core::backup::tests::fixture().map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        seed(
            &db,
            &workspace_id,
            &workspace.canonicalize().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let backup = create(&workspace, &database, RedactionLevel::None)?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let original = snapshot(&db, &workspace_id).map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let options = BackupRestoreOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&backup.backup_path),
            side_path: root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("refused"),
            restore_graph_cache: false,
            dry_run: false,
        };
        let changed = std::cell::Cell::new(false);
        let mutate = |path: &Path| -> Result<(), DomainError> {
            if late {
                assert!(
                    path.parent()
                        .ok_or_else(|| work_history_error("no staged parent"))?
                        .join("index/meta.json")
                        .is_file()
                );
            }
            let db = DbConnection::open_file(path).map_err(work_history_error)?;
            let before = snapshot(&db, &workspace_id).map_err(work_history_error)?;
            let count = db.count_table_rows(table).map_err(work_history_error)?;
            if extra_workspace {
                let mut foreign = db
                    .get_workspace(&workspace_id)
                    .map_err(work_history_error)?
                    .ok_or_else(|| work_history_error("missing fixture workspace"))?;
                foreign.id = WorkspaceId::from_uuid(Uuid::from_u128(9)).to_string();
                foreign.path = "/RECOVERY_PRIVATE_CANARY/foreign".to_owned();
                db.restore_workspace_row(&foreign)
                    .map_err(work_history_error)?;
            } else {
                db.execute_raw(sql).map_err(work_history_error)?;
                if paired_workspace {
                    db.execute_raw("UPDATE workspaces SET path = '/RECOVERY_PRIVATE_CANARY'")
                        .map_err(work_history_error)?;
                }
            }
            assert_ne!(
                before,
                snapshot(&db, &workspace_id).map_err(work_history_error)?
            );
            assert_eq!(
                db.count_table_rows(table).map_err(work_history_error)?,
                if extra_workspace { count + 1 } else { count }
            );
            db.close().map_err(work_history_error)?;
            changed.set(true);
            Ok(())
        };
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &options,
            |path| if late { Ok(()) } else { mutate(path) },
            |path| if late { mutate(path) } else { Ok(()) },
        )
        .err()
        .ok_or("published altered checkpoint or destination binding")?;
        assert!(
            changed.get(),
            "corruption not exercised: {}",
            error.message()
        );
        assert!(
            error.message().contains(table),
            "wrong refusal: {}",
            error.message()
        );
        assert!(!error.message().contains("RECOVERY_PRIVATE_CANARY"));
        assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        assert_eq!(
            snapshot(&db, &workspace_id).map_err(|e| e.to_string())?,
            original
        );
        db.close().map_err(|e| e.to_string())?;
        assert_eq!(
            verify_backup(&BackupVerifyOptions {
                workspace_path: workspace,
                backup_path: options.backup_path,
            })
            .map_err(|e| e.message())?
            .status,
            "verified"
        );
    }
    Ok(())
}

#[test]
fn import_fence_preserves_resume_cursor() -> TestResult {
    assert_refused(
        "import_ledger",
        "UPDATE import_ledger SET cursor_json = '{}'",
        false,
        false,
    )
}

#[test]
fn import_fence_preserves_terminal_failure() -> TestResult {
    assert_refused(
        "import_ledger",
        "UPDATE import_ledger SET status = 'completed', error_code = NULL, error_message = NULL WHERE status = 'failed'",
        false,
        false,
    )
}

#[test]
fn import_fence_preserves_accounted_progress() -> TestResult {
    assert_refused(
        "import_ledger",
        "UPDATE import_ledger SET imported_span_count = 999, attempt_count = 0",
        false,
        false,
    )
}

#[test]
fn import_fence_preserves_cass_source_query() -> TestResult {
    assert_refused(
        "import_ledger",
        "UPDATE import_ledger SET source_id = 'RECOVERY_PRIVATE_CANARY' WHERE id = 'imp_00000000000000000000000000'",
        false,
        false,
    )
}

#[test]
fn import_fence_rejects_paired_source_and_workspace_retargeting() -> TestResult {
    assert_refused(
        "workspaces",
        "UPDATE import_ledger SET source_id = 'cass://sessions?workspace=/RECOVERY_PRIVATE_CANARY&limit=7&since=2026-09-01T00:00:00Z' WHERE id = 'imp_00000000000000000000000000'",
        true,
        false,
    )
}

#[test]
fn import_fence_rejects_additional_workspace_after_index_rebuild() -> TestResult {
    assert_refused("workspaces", "", false, true)
}

#[test]
fn import_history_survives_two_relocations_without_replaying_progress() -> TestResult {
    for redaction in [RedactionLevel::None, RedactionLevel::Standard] {
        let (root, mut workspace, mut database) =
            crate::core::backup::tests::fixture().map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        seed(
            &db,
            &workspace_id,
            &workspace.canonicalize().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let original_workspace = db
            .get_workspace(&workspace_id)
            .map_err(|e| e.to_string())?
            .ok_or("missing source workspace")?;
        let original_imports = db
            .list_import_ledgers(&workspace_id)
            .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        for round in 0..2 {
            let backup = create(&workspace, &database, redaction)?;
            let side = root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join(format!("round-{round}"));
            let restored = restore_backup_to_side_path(&BackupRestoreOptions {
                workspace_path: workspace,
                backup_path: PathBuf::from(backup.backup_path),
                side_path: side.clone(),
                restore_graph_cache: false,
                dry_run: false,
            })
            .map_err(|e| e.message())?;
            let db = DbConnection::open_file(&restored.restored_database_path)
                .map_err(|e| e.to_string())?;
            let mut expected_workspace = original_workspace.clone();
            expected_workspace.path = side.to_string_lossy().into_owned();
            assert_eq!(
                db.list_workspaces().map_err(|e| e.to_string())?,
                vec![expected_workspace]
            );
            let mut expected = original_imports.clone();
            for row in &mut expected {
                let query = row
                    .source_id
                    .rsplit_once("&limit=")
                    .ok_or("missing captured CASS query")?
                    .1;
                row.source_id =
                    format!("cass://sessions?workspace={}&limit={query}", side.display());
                if row.status == "running" {
                    row.status = "pending".to_owned();
                    row.started_at = None;
                    row.completed_at = None;
                }
            }
            assert_eq!(
                db.list_import_ledgers(&workspace_id)
                    .map_err(|e| e.to_string())?,
                expected
            );
            db.close().map_err(|e| e.to_string())?;
            workspace = side;
            database = PathBuf::from(restored.restored_database_path);
        }
    }
    Ok(())
}
