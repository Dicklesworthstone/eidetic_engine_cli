//! Preserve completed work, pending work and the evidence explaining it.
//!
//! A restore with the right row counts can still lose a journal tombstone,
//! fabricate a successful task, retarget an index job or rewrite an audit.
//! Compare against the admitted archive at both publication fences. Rebuilding
//! indexes does not authorize changing historical work or artifact evidence.

use std::collections::{BTreeMap, BTreeSet};

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    ARTIFACT_REGISTRY_SCHEMA, AUDIT_HISTORY_SCHEMA, BackupArtifactRegistry, BackupAuditHistory,
    BackupRestoredDerivedAssetReport, BackupWorkHistory, WORK_HISTORY_CHUNK_ROWS,
    read_restored_derived_json, task_episode_json,
};
use crate::db::{DbConnection, JournalEntryListFilter, StoredJournalEntry};
use crate::models::{DomainError, RedactionLevel};
use crate::policy::{InstructionRisk, detect_instruction_like_content};

const WORK_TABLES: &[&str] = &[
    "journal_entries",
    "search_index_jobs",
    "task_episodes",
    "artifacts",
    "artifact_links",
];

pub(super) struct OperationalExpectation {
    workspace_id: String,
    rows: Rows,
    // Signed chunk order, not ID order or reconstructed wall-clock order.
    // Numeric cursors belong to the restored store and need not equal the
    // source cursors, but the archive's relative replay order must survive.
    audit_ids: Vec<String>,
}

impl OperationalExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let mut expected = Self {
            workspace_id: workspace_id.to_owned(),
            rows: Rows::default(),
            audit_ids: Vec::new(),
        };
        let mut audit_chunks = BTreeMap::new();
        for kind in ["work_history", "artifact_registry", "audit_history"] {
            let count = assets.iter().filter(|asset| asset.kind == kind).count();
            let mut slots = BTreeSet::new();
            let mut source_workspace: Option<String> = None;
            for asset in assets.iter().filter(|asset| asset.kind == kind) {
                let value = read_restored_derived_json(asset)?;
                let (source, index, declared, lengths) = match kind {
                    "work_history" => {
                        let chunk: BackupWorkHistory = serde_json::from_value(value)
                            .map_err(|_| recovery_error("Invalid recovered work history"))?;
                        if chunk.schema != "ee.backup.work_history.v1" {
                            return Err(recovery_error("Unsupported recovered work history"));
                        }
                        let lengths = [chunk.journal_entries.len(), chunk.search_index_jobs.len()];
                        for mut row in chunk.journal_entries {
                            rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
                            // Match the recovery writer: rescreen text, but never
                            // lower risk because redaction removed its trigger.
                            rescreen_journal(&mut row)?;
                            expected
                                .rows
                                .insert("journal_entries", &row.entry_id, &row)?;
                        }
                        for mut row in chunk.search_index_jobs {
                            rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
                            if row.status == "running" {
                                // A lease in the source process cannot survive
                                // recovery. Terminal failures are not retried.
                                row.status = "pending".to_owned();
                                row.documents_indexed = 0;
                                row.started_at = None;
                                row.completed_at = None;
                                row.error_message = None;
                            }
                            expected.rows.insert("search_index_jobs", &row.id, &row)?;
                        }
                        (
                            chunk.workspace_id,
                            chunk.chunk_index,
                            chunk.chunk_count,
                            lengths,
                        )
                    }
                    "artifact_registry" => {
                        let chunk: BackupArtifactRegistry = serde_json::from_value(value)
                            .map_err(|_| recovery_error("Invalid recovered artifact registry"))?;
                        if chunk.schema != ARTIFACT_REGISTRY_SCHEMA || chunk.backup_id != backup_id
                        {
                            return Err(recovery_error("Substituted recovered artifact registry"));
                        }
                        let lengths = [chunk.artifacts.len(), chunk.links.len()];
                        for entry in chunk.artifacts {
                            let mut row = entry.row;
                            rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
                            expected.rows.insert("artifacts", &row.id, &row)?;
                        }
                        for row in chunk.links {
                            expected.rows.insert(
                                "artifact_links",
                                &(
                                    &row.artifact_id,
                                    &row.target_type,
                                    &row.target_id,
                                    &row.relation,
                                ),
                                &row,
                            )?;
                        }
                        (
                            chunk.workspace_id,
                            chunk.chunk_index,
                            chunk.chunk_count,
                            lengths,
                        )
                    }
                    _ => {
                        let chunk: BackupAuditHistory = serde_json::from_value(value)
                            .map_err(|_| recovery_error("Invalid recovered audit history"))?;
                        if chunk.schema != AUDIT_HISTORY_SCHEMA || chunk.backup_id != backup_id {
                            return Err(recovery_error("Substituted recovered audit history"));
                        }
                        let lengths = [chunk.rows.len(), 0];
                        let mut audit_ids = Vec::with_capacity(chunk.rows.len());
                        for entry in chunk.rows {
                            let row = entry.row;
                            if row
                                .workspace_id
                                .as_deref()
                                .is_some_and(|id| id != workspace_id)
                            {
                                return Err(recovery_error("Foreign recovered audit history"));
                            }
                            // Audits retain their source workspace identity and
                            // chain hashes; the recovery writer does not rekey them.
                            audit_ids.push(row.id.clone());
                            expected.rows.insert("audit_log", &row.id, &row)?;
                        }
                        audit_chunks.insert(chunk.chunk_index, audit_ids);
                        (
                            chunk.workspace_id,
                            chunk.chunk_index,
                            chunk.chunk_count,
                            lengths,
                        )
                    }
                };
                if declared != count
                    || index >= count
                    || !slots.insert(index)
                    || source_workspace.as_deref().is_some_and(|id| id != source)
                    || lengths.into_iter().any(|n| n > WORK_HISTORY_CHUNK_ROWS)
                {
                    return Err(recovery_error(
                        "Incomplete or foreign recovered work evidence",
                    ));
                }
                source_workspace = Some(source);
            }
        }
        // Asset enumeration order is not authoritative. The same signed
        // chunk sequence that drives restore also drives this frozen fence.
        expected.audit_ids = audit_chunks.into_values().flatten().collect();
        // Frozen lab files share the asset kind but are copied as files, not
        // rehydrated into task_episodes. Match the recovery writer's dispatch
        // boundary so a valid frozen companion cannot block durable recovery.
        for asset in assets.iter().filter(|asset| {
            asset.kind == "lab_episode" && asset.path.starts_with("derived/lab/episodes/")
        }) {
            let value = read_restored_derived_json(asset)?;
            if value.get("schema").and_then(serde_json::Value::as_str)
                != Some("ee.backup.derived.lab_episode.v1")
            {
                return Err(recovery_error("Unsupported recovered task episode"));
            }
            let row = value
                .get("episode")
                .filter(|row| row.is_object())
                .ok_or_else(|| recovery_error("Invalid recovered task episode"))?;
            let id = row
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| recovery_error("Missing recovered task identity"))?
                .to_owned();
            // Episode export uses the original workspace, not redacted IDs.
            if row.get("workspaceId").and_then(serde_json::Value::as_str) != Some(workspace_id) {
                return Err(recovery_error("Foreign recovered task episode"));
            }
            expected.rows.insert("task_episodes", &id, row)?;
        }
        Ok(expected)
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for row in db
            .list_journal_entries(
                &self.workspace_id,
                &JournalEntryListFilter {
                    limit: u32::MAX,
                    ..Default::default()
                },
            )
            .map_err(storage_error)?
        {
            actual.insert("journal_entries", &row.entry_id, &row)?;
        }
        // The recovery rebuild uses a full source snapshot, not the job worker.
        // It neither consumes nor invents jobs. Do not allow arbitrary lifecycle
        // changes merely because a derived index was rebuilt successfully.
        for row in db
            .list_search_index_jobs(&self.workspace_id, None)
            .map_err(storage_error)?
        {
            actual.insert("search_index_jobs", &row.id, &row)?;
        }
        for row in db
            .list_task_episodes(Some(&self.workspace_id), None, u32::MAX)
            .map_err(storage_error)?
        {
            let value = task_episode_json(&row, "", RedactionLevel::None, &BTreeMap::new());
            actual.insert("task_episodes", &row.id, &value["episode"])?;
        }
        for row in db
            .list_artifacts(&self.workspace_id, None)
            .map_err(storage_error)?
        {
            for link in db.list_artifact_links(&row.id).map_err(storage_error)? {
                actual.insert(
                    "artifact_links",
                    &(
                        &link.artifact_id,
                        &link.target_type,
                        &link.target_id,
                        &link.relation,
                    ),
                    &link,
                )?;
            }
            actual.insert("artifacts", &row.id, &row)?;
        }
        self.rows.verify_complete(&actual, db, WORK_TABLES)?;
        // Import and rebuilding append their own audit entries legitimately.
        // Every captured row must still exist unchanged, including its chain
        // commitments. A new valid hash cannot authorize rewriting history.
        for id in &self.audit_ids {
            let row = db
                .get_audit(id)
                .map_err(storage_error)?
                .ok_or_else(|| recovery_error("Restored durable content differs for audit_log"))?;
            actual.insert("audit_log", &row.id, &row)?;
        }
        self.rows.verify(&actual, &["audit_log"])?;
        verify_audit_order(db, &self.audit_ids)
    }
}

/// Check the archive's ordered subsequence in the caller's publication snapshot.
/// Import/rebuild audits and gaps in numeric rowids are legitimate; reordering
/// captured entries is not. Row hashes do not cover SQLite's implicit rowid,
/// and per-ID content equality alone therefore cannot establish replay order.
fn verify_audit_order(db: &DbConnection, expected: &[String]) -> Result<(), DomainError> {
    use sqlmodel_core::Value;

    let invalid = || recovery_error("Restored durable replay order differs for audit_log");
    let mut previous = 0_i64;
    for page in expected.chunks(256) {
        let parameters = page
            .iter()
            .map(|id| Value::Text(id.clone()))
            .collect::<Vec<_>>();
        let slots = (1..=page.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql =
            format!("SELECT id, rowid FROM audit_log WHERE id IN ({slots}) ORDER BY rowid ASC");
        let rows = db.query(&sql, &parameters).map_err(storage_error)?;
        if rows.len() != page.len() {
            return Err(invalid());
        }
        for (row, id) in rows.iter().zip(page) {
            let cursor = row.get(1).and_then(Value::as_i64).ok_or_else(invalid)?;
            if row.get(0).and_then(Value::as_str) != Some(id.as_str()) || cursor <= previous {
                return Err(invalid());
            }
            previous = cursor;
        }
    }
    Ok(())
}

fn rebind(value: &mut String, source: &str, target: &str) -> Result<(), DomainError> {
    if value.as_str() != source {
        return Err(recovery_error("Foreign recovered work row"));
    }
    *value = target.to_owned();
    Ok(())
}

fn rescreen_journal(row: &mut StoredJournalEntry) -> Result<(), DomainError> {
    let recorded = match row.instruction_risk.as_str() {
        "none" => InstructionRisk::None,
        "low" => InstructionRisk::Low,
        "medium" => InstructionRisk::Medium,
        "high" => InstructionRisk::High,
        _ => return Err(recovery_error("Invalid recovered journal risk")),
    };
    let content = detect_instruction_like_content(&row.body).risk;
    let structured = row
        .structured
        .as_deref()
        .map_or(InstructionRisk::None, |text| {
            detect_instruction_like_content(text).risk
        });
    row.instruction_risk = recorded.max(content).max(structured).as_str().to_owned();
    Ok(())
}

#[cfg(test)]
#[path = "backup_operational_recovery_tests.rs"]
mod tests;

#[cfg(test)]
mod audit_order_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::core::backup::{BackupAuditEntry, hash_bytes, recovery_faults};
    use crate::db::{CreateAuditInput, CreateWorkspaceInput, StoredAuditEntry};
    use std::path::Path;

    const WORKSPACE: &str = "wsp_00000000000000000000000091";
    const BACKUP: &str = "audit-order-fixture";

    fn fixture() -> (tempfile::TempDir, DbConnection) {
        let root = tempfile::tempdir().expect("temporary store");
        let path = root.path().canonicalize().expect("physical path");
        let db = DbConnection::open_file(&path.join("audit.db")).expect("open store");
        db.migrate().expect("real migrated schema");
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: path.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .expect("workspace");
        (root, db)
    }

    fn append(db: &DbConnection, id: &str) -> StoredAuditEntry {
        db.insert_audit(
            id,
            &CreateAuditInput {
                workspace_id: Some(WORKSPACE.to_owned()),
                actor: Some("AUDIT_ORDER_PRIVATE_CANARY".to_owned()),
                action: "backup.order_fixture".to_owned(),
                target_type: None,
                target_id: None,
                details: Some("private fixture annotation".to_owned()),
            },
        )
        .expect("append audit");
        db.get_audit(id)
            .expect("read audit")
            .expect("inserted audit")
    }

    fn seed(db: &DbConnection, count: usize) -> Vec<StoredAuditEntry> {
        db.with_transaction(|| {
            Ok((0..count)
                // IDs deliberately run opposite to the durable insertion order.
                .map(|index| append(db, &format!("audit_{:026}", count - index)))
                .collect())
        })
        .expect("seed audit history")
    }

    fn assets(root: &Path, rows: &[StoredAuditEntry]) -> Vec<BackupRestoredDerivedAssetReport> {
        let count = rows.len().div_ceil(WORK_HISTORY_CHUNK_ROWS).max(1);
        (0..count)
            .map(|index| {
                let chunk = BackupAuditHistory {
                    schema: AUDIT_HISTORY_SCHEMA.to_owned(),
                    backup_id: BACKUP.to_owned(),
                    workspace_id: WORKSPACE.to_owned(),
                    chunk_index: index,
                    chunk_count: count,
                    rows: rows
                        .iter()
                        .skip(index * WORK_HISTORY_CHUNK_ROWS)
                        .take(WORK_HISTORY_CHUNK_ROWS)
                        .map(|row| BackupAuditEntry {
                            row: row.clone(),
                            source_prev_row_hash: row.prev_row_hash.clone(),
                            source_row_hash: row.this_row_hash.clone(),
                            transformed: false,
                        })
                        .collect(),
                    authentication: None,
                };
                // The production caller authenticates these assets first.
                // Here the real frozen-expectation reader is tested directly.
                let path = root.join(format!("audit-{index:08}.json"));
                std::fs::write(&path, serde_json::to_vec(&chunk).expect("encode chunk"))
                    .expect("write fixture asset");
                BackupRestoredDerivedAssetReport {
                    path: format!("derived/audit-history/{index:08}.json"),
                    kind: "audit_history".to_owned(),
                    restore_path: path.to_string_lossy().into_owned(),
                    lab_episode_path: None,
                }
            })
            .collect()
    }

    fn expected(root: &Path, rows: &[StoredAuditEntry]) -> OperationalExpectation {
        let mut assets = assets(root, rows);
        // Enumeration order cannot replace the signed chunk index order.
        assets.reverse();
        OperationalExpectation::from_assets(&assets, BACKUP, WORKSPACE).expect("freeze archive")
    }

    fn assert_unchanged(db: &DbConnection, rows: &[StoredAuditEntry]) {
        for row in rows {
            assert_eq!(db.get_audit(&row.id).unwrap().as_ref(), Some(row));
            assert_eq!(crate::db::compute_audit_row_hash(row), {
                let actual = db.get_audit(&row.id).unwrap().unwrap();
                crate::db::compute_audit_row_hash(&actual)
            });
        }
    }

    #[test]
    fn audit_order_fence_keeps_chunk_order_and_all_pages_without_sorting_ids() {
        let (root, db) = fixture();
        let rows = seed(&db, 513);
        let expected = expected(root.path(), &rows);
        assert_eq!(
            expected.audit_ids,
            rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>()
        );
        db.begin_read_snapshot().unwrap();
        expected
            .verify_connection(&db)
            .expect("complete ordered history");
        db.commit_read_snapshot()
            .expect("caller still owns snapshot");
        assert_unchanged(&db, &rows);
    }

    #[test]
    fn audit_order_fence_rejects_same_content_and_hashes_in_a_different_replay_order() {
        let (root, db) = fixture();
        let rows = seed(&db, 3);
        let expected = expected(root.path(), &rows);
        recovery_faults::inject_history_corruption(
            &db,
            "audit_log",
            "UPDATE audit_log SET rowid = 1000000 - rowid",
        )
        .expect("reorder below the append-only guard");
        assert_unchanged(&db, &rows);
        assert_eq!(db.count_table_rows("audit_log").unwrap(), 3);
        let error = expected.verify_connection(&db).unwrap_err();
        assert!(error.message().contains("replay order"));
        assert!(!error.message().contains("AUDIT_ORDER_PRIVATE_CANARY"));
        assert!(rows.iter().all(|row| !error.message().contains(&row.id)));
    }

    #[test]
    fn audit_order_fence_checks_the_boundary_between_individually_ordered_batches() {
        let (root, db) = fixture();
        let rows = seed(&db, 257);
        let expected = expected(root.path(), &rows);
        recovery_faults::inject_history_corruption(
            &db,
            "audit_log",
            "UPDATE audit_log SET rowid = rowid + 1000000 WHERE rowid <= 256",
        )
        .unwrap();
        assert_unchanged(&db, &rows);
        let error = expected.verify_connection(&db).unwrap_err();
        assert!(error.message().contains("replay order"));
    }

    #[test]
    fn audit_order_fence_allows_new_audits_and_noncontiguous_recovery_cursors() {
        let (root, db) = fixture();
        let first = append(&db, "audit_00000000000000000000000003");
        append(&db, "audit_00000000000000000000000009");
        let last = append(&db, "audit_00000000000000000000000001");
        let rows = vec![first, last];
        let expected = expected(root.path(), &rows);
        recovery_faults::inject_history_corruption(
            &db,
            "audit_log",
            "UPDATE audit_log SET rowid = rowid + 1000000",
        )
        .unwrap();
        append(&db, "audit_00000000000000000000000008");
        let before = db.count_table_rows("audit_log").unwrap();
        expected
            .verify_connection(&db)
            .expect("relative order preserved");
        assert_eq!(db.count_table_rows("audit_log").unwrap(), before);
        assert_unchanged(&db, &rows);
    }

    #[test]
    fn audit_order_fence_withholds_missing_or_unreadable_history_without_private_details() {
        let (_root, db) = fixture();
        let row = append(&db, "audit_00000000000000000000000001");
        let missing = vec![row.id.clone(), "AUDIT_ORDER_PRIVATE_CANARY".to_owned()];
        let error = verify_audit_order(&db, &missing).unwrap_err();
        assert!(!error.message().contains("AUDIT_ORDER_PRIVATE_CANARY"));
        db.execute_raw("ALTER TABLE audit_log RENAME TO retained_audit_history")
            .expect("retain rather than delete the unavailable table");
        let error = verify_audit_order(&db, &[row.id]).unwrap_err();
        assert!(!error.message().contains("private fixture annotation"));
        assert!(!error.message().contains("AUDIT_ORDER_PRIVATE_CANARY"));
        verify_audit_order(&db, &[]).expect("empty history needs no query");
    }

    #[test]
    fn audit_order_fence_borrows_the_snapshot_across_a_concurrent_reordering() {
        let (root, reader) = fixture();
        let rows = seed(&reader, 3);
        let expected = expected(root.path(), &rows);
        let writer = DbConnection::open_file(&root.path().join("audit.db")).unwrap();
        reader.begin_read_snapshot().unwrap();
        expected.verify_connection(&reader).unwrap();
        recovery_faults::inject_history_corruption(
            &writer,
            "audit_log",
            "UPDATE audit_log SET rowid = 1000000 - rowid",
        )
        .unwrap();
        expected
            .verify_connection(&reader)
            .expect("pinned order retained");
        reader
            .commit_read_snapshot()
            .expect("helper did not release snapshot");
        reader.begin_read_snapshot().unwrap();
        assert!(expected.verify_connection(&reader).is_err());
        reader
            .commit_read_snapshot()
            .expect("failure also preserves caller ownership");
    }

    #[test]
    fn audit_order_fence_preserves_the_empty_authenticated_archive_shape() {
        let (root, db) = fixture();
        let expected = expected(root.path(), &[]);
        assert!(expected.audit_ids.is_empty());
        expected
            .verify_connection(&db)
            .expect("zero captured audits");
    }

    #[test]
    fn audit_order_fence_blocks_post_rebuild_reordering_and_the_backup_remains_recoverable() {
        use crate::core::backup::{
            BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions, WORKSPACE_MARKER,
            create_backup, restore_backup_to_side_path,
            restore_backup_to_side_path_with_recovery_hooks, verify_backup,
        };
        use std::path::PathBuf;

        let (root, workspace, database) = crate::core::backup::tests::fixture().unwrap();
        let source = DbConnection::open_file(&database).unwrap();
        source
            .insert_audit(
                "audit_00000000000000000000000092",
                &CreateAuditInput {
                    workspace_id: source
                        .list_workspaces()
                        .unwrap()
                        .first()
                        .map(|w| w.id.clone()),
                    actor: Some("audit-order-recovery".to_owned()),
                    action: "backup.order_fixture".to_owned(),
                    target_type: None,
                    target_id: None,
                    details: None,
                },
            )
            .unwrap();
        source.close().unwrap();
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
        .unwrap();
        let records = PathBuf::from(&backup.backup_path).join("records.jsonl");
        let original_hash = hash_bytes(&std::fs::read(&records).unwrap());
        let mut options = BackupRestoreOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&backup.backup_path),
            side_path: root.path().canonicalize().unwrap().join("order-refused"),
            restore_graph_cache: false,
            dry_run: false,
        };
        let reached = std::cell::Cell::new(false);
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &options,
            |_| Ok(()),
            |path| {
                let db = DbConnection::open_file(path).map_err(storage_error)?;
                assert!(db.count_table_rows("audit_log").unwrap() >= 2);
                recovery_faults::inject_history_corruption(
                    &db,
                    "audit_log",
                    "UPDATE audit_log SET rowid = 1000000 - rowid",
                )?;
                db.close().map_err(storage_error)?;
                reached.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            reached.get(),
            "post-rebuild fault did not execute: {error:?}"
        );
        assert!(error.message().contains("replay order"), "{error:?}");
        assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
        assert_eq!(hash_bytes(&std::fs::read(records).unwrap()), original_hash);
        assert_eq!(
            verify_backup(&BackupVerifyOptions {
                workspace_path: workspace,
                backup_path: options.backup_path.clone(),
            })
            .unwrap()
            .status,
            "verified"
        );
        options.side_path = root.path().canonicalize().unwrap().join("order-retry");
        restore_backup_to_side_path(&options).expect("valid history still recovers");
        assert!(options.side_path.join(WORKSPACE_MARKER).is_dir());
    }
}
