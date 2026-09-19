use super::*;
use crate::core::backup::restore_backup_to_side_path_with_recovery_hooks;

fn assert_late_change_refused(table: &str, sql: &str) -> TestResult {
    let fixture = fixture(RedactionLevel::None)?;
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    let original = read_state(&source)?;
    source.close().map_err(|e| e.to_string())?;
    let mut staged_database = None;
    let error = restore_backup_to_side_path_with_recovery_hooks(
        &fixture.options,
        |_| Ok(()),
        |path| {
            // This callback is after a successfully published staging index,
            // not the earlier durable-row admission hook used by other tests.
            let store = path
                .parent()
                .ok_or_else(|| work_history_error("missing staged store"))?;
            assert!(store.join("index/meta.json").is_file());
            staged_database = Some(path.to_path_buf());
            let db = DbConnection::open_file(path).map_err(work_history_error)?;
            let tables = db.list_user_tables().map_err(work_history_error)?;
            let before = tables
                .iter()
                .map(|t| db.count_table_rows(t))
                .collect::<Result<Vec<_>, _>>()
                .map_err(work_history_error)?;
            let history = read_state(&db).map_err(work_history_error)?;
            db.execute_raw(sql).map_err(work_history_error)?;
            assert_ne!(read_state(&db).map_err(work_history_error)?, history);
            let after = tables
                .iter()
                .map(|t| db.count_table_rows(t))
                .collect::<Result<Vec<_>, _>>()
                .map_err(work_history_error)?;
            assert_eq!(before, after);
            db.close().map_err(work_history_error)
        },
    )
    .err()
    .ok_or("published authority modified after index rebuild")?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!fixture.options.side_path.join(WORKSPACE_MARKER).exists());
    let staged = staged_database.ok_or("post-rebuild callback did not execute")?;
    assert!(
        staged.is_file(),
        "retain failed staged state for inspection"
    );
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    assert_eq!(read_state(&source)?, original);
    source.close().map_err(|e| e.to_string())?;
    assert_eq!(
        verify_backup(&BackupVerifyOptions {
            workspace_path: fixture.options.workspace_path,
            backup_path: fixture.options.backup_path,
        })
        .map_err(|e| e.message())?
        .status,
        "verified"
    );
    Ok(())
}

#[test]
fn publication_fence_rejects_quarantine_release_after_index_rebuild() -> TestResult {
    assert_late_change_refused(
        "trust_quarantine",
        "UPDATE trust_quarantine SET status = 'released'",
    )
}

#[test]
fn publication_fence_rejects_fabricated_reveal_after_index_rebuild() -> TestResult {
    assert_late_change_refused(
        "memory_seals",
        "UPDATE memory_seals SET revealed_at = '2026-09-02T00:00:00Z', reveal_verified = 1",
    )
}

#[test]
fn publication_fence_rejects_fabricated_verification_after_index_rebuild() -> TestResult {
    assert_late_change_refused(
        "certificates",
        "UPDATE certificates SET status = 'valid', verified_at = '2026-09-02T00:00:00Z'",
    )
}
