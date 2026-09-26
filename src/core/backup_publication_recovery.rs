//! Recheck authority after derived rebuilding, immediately before publication.
//!
//! The full recovery rebuild changes derived generations, not durable table
//! populations. Reconcile every binary-owned table obligation in the same read
//! snapshot as captured content, so extra foreign or orphaned rows cannot hide
//! behind a workspace filter after the earlier inventory fence has passed.

use std::path::Path;

use super::super::super::backup_table_policy;
use super::super::{RecoveryReadSnapshot, RestoreInventory};
use super::{HistoryExpectation, recovery_error, storage_error};
use crate::db::{DatabaseConfig, DbConnection};
use crate::models::DomainError;

impl HistoryExpectation {
    pub(in crate::core::backup) fn verify_before_publication(
        &self,
        path: &Path,
        inventory: &RestoreInventory,
    ) -> Result<(), DomainError> {
        let db = DbConnection::open(DatabaseConfig::read_only_file(path.to_path_buf()))
            .map_err(storage_error)?;
        let result = (|| {
            let snapshot = RecoveryReadSnapshot::begin(&db)?;
            verify_storage_graph(&db)?;
            inventory.verify_rows(&db)?;
            self.verify_connection(&db)?;
            snapshot.finish()
        })();
        let closed = db.close().map(|_| ()).map_err(storage_error);
        result.and(closed)
    }
}

/// Counts and row fingerprints do not establish relational integrity, and an
/// inventory cannot authorize omission of a table introduced after capture.
/// Inspect the actual staged schema and all declared foreign keys, including
/// derived tables populated by rebuilding, before publishing any workspace.
/// This check neither repairs rows nor enables/disables foreign-key enforcement;
/// it runs inside the caller's already-owned read snapshot.
fn verify_storage_graph(db: &DbConnection) -> Result<(), DomainError> {
    for table in db.list_user_tables().map_err(storage_error)? {
        if !backup_table_policy(&table).schema_covered() {
            // A schema identifier can contain private or attacker-chosen text.
            // Do not echo it, SQL diagnostics, rowids or foreign-key values.
            return Err(recovery_error(
                "Restored database contains an unsupported recovery table; the restored store was not published",
            ));
        }
    }
    let integrity = db.check_foreign_keys().map_err(storage_error)?;
    if !integrity.passed || !integrity.violations.is_empty() {
        return Err(recovery_error(
            "Restored database contains broken foreign-key relationships; the restored store was not published",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::core::backup::{
        BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions, create_backup,
        restore_backup_to_side_path, restore_backup_to_side_path_with_recovery_hooks,
        verify_backup,
    };
    use crate::models::RedactionLevel;
    use std::path::PathBuf;

    type TestResult = Result<(), String>;

    fn error_text(error: impl std::fmt::Display) -> String {
        error.to_string()
    }

    fn read_storage_graph(path: &Path) -> Result<(), DomainError> {
        let db = DbConnection::open(DatabaseConfig::read_only_file(path.to_path_buf()))
            .map_err(storage_error)?;
        let result = (|| {
            let snapshot = RecoveryReadSnapshot::begin(&db)?;
            verify_storage_graph(&db)?;
            snapshot.finish()
        })();
        let closed = db.close().map(|_| ()).map_err(storage_error);
        result.and(closed)
    }

    fn backup_fixture() -> Result<(tempfile::TempDir, PathBuf, BackupRestoreOptions), String> {
        let (root, workspace, database) =
            crate::core::backup::tests::fixture().map_err(error_text)?;
        let backup = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::Standard,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(error_text)?;
        assert!(backup.recovery_inventory.snapshot_coverage_complete);
        let options = BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(backup.backup_path),
            side_path: root
                .path()
                .canonicalize()
                .map_err(error_text)?
                .join("restored"),
            restore_graph_cache: false,
            dry_run: false,
        };
        Ok((root, database, options))
    }

    #[test]
    fn migrated_store_passes_the_read_only_relational_check() -> TestResult {
        let (_root, _workspace, database) =
            crate::core::backup::tests::fixture().map_err(error_text)?;
        read_storage_graph(&database).map_err(error_text)?;
        let db = DbConnection::open_file(&database).map_err(error_text)?;
        assert!(db.check_foreign_keys().map_err(error_text)?.passed);
        db.close().map_err(error_text)
    }

    #[test]
    fn orphaned_tag_is_rejected_even_when_row_counts_are_unchanged() -> TestResult {
        let (_root, _workspace, database) =
            crate::core::backup::tests::fixture().map_err(error_text)?;
        let db = DbConnection::open_file(&database).map_err(error_text)?;
        let before = db.count_table_rows("memory_tags").map_err(error_text)?;
        assert!(before > 0, "the corruption must change a real tag row");
        db.execute_raw("PRAGMA foreign_keys = OFF")
            .map_err(error_text)?;
        db.execute_raw("UPDATE memory_tags SET memory_id = 'mem_00000000000000000000009999'")
            .map_err(error_text)?;
        assert_eq!(
            before,
            db.count_table_rows("memory_tags").map_err(error_text)?
        );
        assert!(!db.check_foreign_keys().map_err(error_text)?.passed);
        // Checking does not depend on enforcement being enabled on the writer.
        db.close().map_err(error_text)?;
        let error = read_storage_graph(&database).expect_err("dangling tag must be rejected");
        assert!(
            error
                .to_string()
                .contains("broken foreign-key relationships")
        );
        assert!(!error.to_string().contains("00009999"));
        let db = DbConnection::open_file(&database).map_err(error_text)?;
        assert_eq!(
            before,
            db.count_table_rows("memory_tags").map_err(error_text)?
        );
        assert!(!db.check_foreign_keys().map_err(error_text)?.passed);
        db.close().map_err(error_text)
    }

    #[test]
    fn unsupported_empty_table_is_not_hidden_by_complete_counts() -> TestResult {
        let (_root, _workspace, database) =
            crate::core::backup::tests::fixture().map_err(error_text)?;
        let db = DbConnection::open_file(&database).map_err(error_text)?;
        db.execute_raw("CREATE TABLE private_recovery_schema_canary (id TEXT PRIMARY KEY)")
            .map_err(error_text)?;
        assert!(db.check_foreign_keys().map_err(error_text)?.passed);
        db.close().map_err(error_text)?;
        let error = read_storage_graph(&database).expect_err("unknown schema must be rejected");
        assert!(error.to_string().contains("unsupported recovery table"));
        assert!(!error.to_string().contains("private_recovery_schema_canary"));
        Ok(())
    }

    #[test]
    fn relational_check_does_not_release_its_callers_snapshot() -> TestResult {
        let (_root, _workspace, database) =
            crate::core::backup::tests::fixture().map_err(error_text)?;
        let db =
            DbConnection::open(DatabaseConfig::read_only_file(database)).map_err(error_text)?;
        let snapshot = RecoveryReadSnapshot::begin(&db).map_err(error_text)?;
        verify_storage_graph(&db).map_err(error_text)?;
        assert!(
            db.begin_read_snapshot().is_err(),
            "the caller still owns the snapshot"
        );
        snapshot.finish().map_err(error_text)?;
        let next = RecoveryReadSnapshot::begin(&db).map_err(error_text)?;
        next.finish().map_err(error_text)?;
        db.close().map_err(error_text)
    }

    #[test]
    fn complete_restore_still_rebuilds_and_publishes_a_valid_storage_graph() -> TestResult {
        let (_root, _database, options) = backup_fixture()?;
        let restored = restore_backup_to_side_path(&options).map_err(error_text)?;
        assert!(options.side_path.join(".ee").is_dir());
        read_storage_graph(Path::new(&restored.restored_database_path)).map_err(error_text)
    }

    #[test]
    fn schema_drift_after_rebuild_prevents_publication_and_preserves_the_backup() -> TestResult {
        let (_root, database, options) = backup_fixture()?;
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &options,
            |_| Ok(()),
            |path| {
                let db = DbConnection::open_file(path).map_err(storage_error)?;
                db.execute_raw("CREATE TABLE private_recovery_schema_canary (id TEXT PRIMARY KEY)")
                    .map_err(storage_error)?;
                db.close().map_err(storage_error)
            },
        )
        .expect_err("an unclassified staged table must prevent publication");
        assert!(error.to_string().contains("unsupported recovery table"));
        assert!(!error.to_string().contains("private_recovery_schema_canary"));
        assert!(!options.side_path.join(".ee").exists());
        read_storage_graph(&database).map_err(error_text)?;
        let verified = verify_backup(&BackupVerifyOptions {
            workspace_path: options.workspace_path.clone(),
            backup_path: options.backup_path.clone(),
        })
        .map_err(error_text)?;
        assert_eq!(verified.status, "verified");
        // Failed publication retains staging rather than poisoning the source
        // or the recovery point. A fresh side path can still recover normally.
        let mut retry = options;
        retry.side_path.set_file_name("restored-retry");
        let restored = restore_backup_to_side_path(&retry).map_err(error_text)?;
        read_storage_graph(Path::new(&restored.restored_database_path)).map_err(error_text)
    }
}
