//! Schema-driven admission and publication fence for portable backup recovery.
//!
//! The authenticated archive describes the expected state; it cannot decide
//! which durable tables the current binary is allowed to omit. Count actual
//! rows in the unpublished database, not vectors passed to individual writers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::db::{DatabaseConfig, DbConnection};
use crate::models::DomainError;

use super::BackupTablePolicy;

#[path = "backup_history_recovery.rs"]
mod history;
pub(super) use history::HistoryExpectation;

#[cfg(test)]
#[path = "backup_typed_memory_recovery_tests.rs"]
mod typed_memory_tests;

// One registry owns both capture policy and restore obligations. Rebuildable
// indexes, host credentials, locks and migration metadata deliberately do not
// appear here. Empty durable tables still need an explicit inventory entry.
const REQUIRED_TABLES: &[(&str, &str, &str)] = &[
    ("workspaces", "maintain", "authenticated_manifest"),
    ("memories", "retrieve", "records_jsonl"),
    ("memory_tags", "retrieve", "records_jsonl"),
    ("memory_links", "retrieve", "records_jsonl"),
    ("attempt_families", "learn", "records_jsonl"),
    ("attempt_family_members", "learn", "records_jsonl"),
    ("audit_log", "maintain", "derived_artifact_restore"),
    ("task_episodes", "learn", "derived_artifact_restore"),
    ("journal_entries", "maintain", "derived_artifact_restore"),
    ("search_index_jobs", "maintain", "derived_artifact_restore"),
    ("recorder_runs", "maintain", "derived_artifact_restore"),
    ("recorder_events", "maintain", "derived_artifact_restore"),
    ("rch_verify_runs", "maintain", "derived_artifact_restore"),
    ("error_fingerprints", "maintain", "derived_artifact_restore"),
    ("error_repair_links", "maintain", "derived_artifact_restore"),
    ("artifacts", "maintain", "derived_artifact_restore"),
    ("artifact_links", "maintain", "derived_artifact_restore"),
    ("rationale_traces", "maintain", "derived_artifact_restore"),
    (
        "rationale_trace_links",
        "maintain",
        "derived_artifact_restore",
    ),
    ("causal_evidence", "maintain", "derived_artifact_restore"),
    ("agents", "maintain", "derived_artifact_restore"),
    ("certificates", "maintain", "derived_artifact_restore"),
    ("memory_seals", "maintain", "derived_artifact_restore"),
    ("trust_quarantine", "maintain", "derived_artifact_restore"),
    ("procedural_rules", "learn", "derived_artifact_restore"),
    ("rule_source_memories", "learn", "derived_artifact_restore"),
    ("rule_tags", "learn", "derived_artifact_restore"),
    ("feedback_events", "learn", "derived_artifact_restore"),
    (
        "agent_context_profiles",
        "learn",
        "derived_artifact_restore",
    ),
    ("debt_snapshots", "maintain", "derived_artifact_restore"),
    (
        "memory_sentinel_specs",
        "maintain",
        "derived_artifact_restore",
    ),
    (
        "reflection_request_ledger",
        "maintain",
        "derived_artifact_restore",
    ),
    ("situation_records", "maintain", "derived_artifact_restore"),
    (
        "tripwire_check_events",
        "maintain",
        "derived_artifact_restore",
    ),
    ("tripwires", "maintain", "derived_artifact_restore"),
    ("evidence_spans", "ingest", "derived_artifact_restore"),
    ("sessions", "ingest", "derived_artifact_restore"),
    ("import_ledger", "ingest", "derived_artifact_restore"),
    ("pack_baselines", "pack", "derived_artifact_restore"),
    (
        "pack_candidate_impressions",
        "pack",
        "derived_artifact_restore",
    ),
    ("pack_evidence_items", "pack", "derived_artifact_restore"),
    ("pack_items", "pack", "derived_artifact_restore"),
    ("pack_omissions", "pack", "derived_artifact_restore"),
    ("pack_records", "pack", "derived_artifact_restore"),
    ("curation_candidates", "learn", "derived_artifact_restore"),
    ("curation_ttl_policies", "learn", "derived_artifact_restore"),
    ("procedures", "learn", "derived_artifact_restore"),
    ("procedure_events", "learn", "derived_artifact_restore"),
    ("feedback_quarantine", "learn", "derived_artifact_restore"),
    ("learning_observations", "learn", "derived_artifact_restore"),
    ("outcome_evidence_rows", "learn", "derived_artifact_restore"),
    ("plan_recipes", "learn", "derived_artifact_restore"),
];

pub(super) fn required_table_policy(table: &str) -> Option<BackupTablePolicy> {
    REQUIRED_TABLES
        .iter()
        .find(|(name, _, _)| *name == table)
        .map(|(_, owner, coverage)| {
            BackupTablePolicy::new(owner, "export_restore_required", coverage)
        })
}

fn recovery_error(message: impl Into<String>) -> DomainError {
    DomainError::Import {
        message: message.into(),
        repair: Some(
            "Keep the backup and any staged files; inspect ee backup inspect <id-or-path> --json before retrying with a complete recovery point. The live store was not replaced."
                .to_owned(),
        ),
    }
}

fn storage_error(_: impl std::fmt::Display) -> DomainError {
    // SQL and filesystem errors may contain private data or host paths.
    recovery_error("Could not reconcile the unpublished restore with its durable inventory")
}

/// Source row counts accepted only after the caller authenticates the manifest.
/// A missing or explicitly partial inventory is not a complete recovery point.
pub(super) struct RestoreInventory {
    expected: BTreeMap<&'static str, u64>,
    source_audit_rows: u64,
}

impl RestoreInventory {
    pub(super) fn from_manifest(manifest: &Value) -> Result<Self, DomainError> {
        let inventory = manifest
            .get("recoveryInventory")
            .filter(|value| value.is_object())
            .ok_or_else(|| recovery_error("Backup is missing its durable recovery inventory"))?;
        if inventory.get("schema").and_then(Value::as_str)
            != Some("ee.backup.recovery_inventory.v1")
            || inventory
                .get("schemaCoverageComplete")
                .and_then(Value::as_bool)
                != Some(true)
            || inventory
                .get("snapshotCoverageComplete")
                .and_then(Value::as_bool)
                != Some(true)
            || [
                "uncoveredRequiredTableCount",
                "uncoveredRequiredRowCount",
                "unclassifiedTableCount",
            ]
            .iter()
            .any(|field| inventory.get(*field).and_then(Value::as_u64) != Some(0))
        {
            return Err(recovery_error(
                "Backup does not declare a complete, supported durable recovery inventory",
            ));
        }
        let rows = inventory
            .get("tables")
            .and_then(Value::as_array)
            .ok_or_else(|| recovery_error("Backup recovery inventory has no table array"))?;
        let mut by_table = BTreeMap::new();
        for row in rows {
            let name = row
                .get("table")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| recovery_error("Backup recovery inventory has an invalid table"))?;
            if by_table.insert(name, row).is_some() {
                return Err(recovery_error("Backup recovery inventory repeats a table"));
            }
            let policy = super::backup_table_policy(name);
            if !policy.schema_covered()
                || row.get("disposition").and_then(Value::as_str) != Some(policy.disposition)
                || row.get("coverage").and_then(Value::as_str) != Some(policy.coverage)
            {
                // Do not echo an untrusted table name or let archive labels
                // turn a required table into a cache or rekeyed credential.
                return Err(recovery_error(
                    "Backup recovery inventory disagrees with the binary's table policy",
                ));
            }
        }

        let mut expected = BTreeMap::new();
        for &(name, _, _) in REQUIRED_TABLES {
            let row = by_table.get(name).ok_or_else(|| {
                recovery_error(format!(
                    "Backup recovery inventory is missing required table {name}"
                ))
            })?;
            let count = row.get("rowCount").and_then(Value::as_u64).ok_or_else(|| {
                recovery_error(format!("Backup recovery count is invalid for {name}"))
            })?;
            if count > i64::MAX as u64
                || row.get("schemaCovered").and_then(Value::as_bool) != Some(true)
                || row.get("snapshotCovered").and_then(Value::as_bool) != Some(true)
            {
                return Err(recovery_error(format!(
                    "Backup does not contain complete recoverable rows for {name}"
                )));
            }
            expected.insert(name, count);
        }
        if expected.get("workspaces") != Some(&1) {
            return Err(recovery_error(
                "Side-path recovery requires exactly one captured workspace",
            ));
        }
        let source_audit_rows = expected.remove("audit_log").ok_or_else(|| {
            recovery_error("Backup recovery inventory has no audit-history obligation")
        })?;
        Ok(Self {
            expected,
            source_audit_rows,
        })
    }

    pub(super) fn expected_audit_rows(&self) -> u64 {
        self.source_audit_rows
    }

    /// Run after every durable family is restored and before index rebuilding
    /// or publication. Rebuilding is allowed to change derived generations/job
    /// state; it must not hide an omitted recovery writer or missing chunk.
    pub(super) fn verify_database(
        &self,
        path: &Path,
        history: &HistoryExpectation,
    ) -> Result<(), DomainError> {
        let connection = DbConnection::open(DatabaseConfig::read_only_file(path.to_path_buf()))
            .map_err(storage_error)?;
        let verified = (|| {
            let snapshot = RecoveryReadSnapshot::begin(&connection)?;
            self.verify_rows(&connection)?;
            history.verify_connection(&connection)?;
            snapshot.finish()
        })();
        let closed = connection.close().map(|_| ()).map_err(storage_error);
        verified.and(closed)
    }

    #[cfg(test)]
    fn verify_connection(&self, connection: &DbConnection) -> Result<(), DomainError> {
        let snapshot = RecoveryReadSnapshot::begin(connection)?;
        self.verify_rows(connection)?;
        snapshot.finish()
    }

    fn verify_rows(&self, connection: &DbConnection) -> Result<(), DomainError> {
        let tables: BTreeSet<_> = connection
            .list_user_tables()
            .map_err(storage_error)?
            .into_iter()
            .collect();
        for (&table, &expected) in &self.expected {
            Self::check_table(connection, &tables, table, expected, false)?;
        }
        // restore_audit_history independently checks and restores the exact
        // captured audit rows. Import and recovery append new audit entries;
        // demanding equality would reject every otherwise correct restore.
        Self::check_table(
            connection,
            &tables,
            "audit_log",
            self.source_audit_rows,
            true,
        )?;
        Ok(())
    }

    fn check_table(
        connection: &DbConnection,
        tables: &BTreeSet<String>,
        table: &'static str,
        expected: u64,
        allow_additions: bool,
    ) -> Result<(), DomainError> {
        if !tables.contains(table) {
            return Err(recovery_error(format!(
                "Restored database is missing durable table {table}"
            )));
        }
        let actual = connection.count_table_rows(table).map_err(storage_error)?;
        let actual = u64::try_from(actual).map_err(storage_error)?;
        if actual != expected && !(allow_additions && actual > expected) {
            return Err(recovery_error(format!(
                "Restored durable row count differs for {table}: expected {expected}, found {actual}; the restored store was not published"
            )));
        }
        Ok(())
    }
}

/// Only release a snapshot that this check successfully acquired. This also
/// releases it on an early mismatch, a query failure, or unwinding.
struct RecoveryReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> RecoveryReadSnapshot<'a> {
    fn begin(connection: &'a DbConnection) -> Result<Self, DomainError> {
        connection.begin_read_snapshot().map_err(storage_error)?;
        Ok(Self {
            connection,
            active: true,
        })
    }

    fn finish(mut self) -> Result<(), DomainError> {
        self.connection
            .commit_read_snapshot()
            .map_err(storage_error)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for RecoveryReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            tracing::error!(
                target: "ee::backup::recovery",
                "failed to release durable recovery verification snapshot"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::db::{CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::{MemoryId, WorkspaceId};
    use serde_json::json;

    fn manifest() -> Value {
        let tables: Vec<_> = REQUIRED_TABLES
            .iter()
            .map(|&(table, owner, coverage)| {
                json!({
                    "table": table,
                    "owner": owner,
                    "disposition": "export_restore_required",
                    "coverage": coverage,
                    "rowCount": u64::from(table == "workspaces"),
                    "schemaCovered": true,
                    "snapshotCovered": true,
                })
            })
            .collect();
        json!({"recoveryInventory": {
            "schema": "ee.backup.recovery_inventory.v1",
            "schemaCoverageComplete": true,
            "snapshotCoverageComplete": true,
            "uncoveredRequiredTableCount": 0,
            "uncoveredRequiredRowCount": 0,
            "unclassifiedTableCount": 0,
            "tables": tables,
        }})
    }

    fn row_mut<'a>(manifest: &'a mut Value, table: &str) -> &'a mut Value {
        manifest["recoveryInventory"]["tables"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["table"] == table)
            .unwrap()
    }

    fn fixture() -> (tempfile::TempDir, DbConnection, std::path::PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("restore.db");
        let db = DbConnection::open_file(&path).unwrap();
        db.migrate().unwrap();
        let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(17)).to_string();
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        (root, db, path, workspace)
    }

    fn seed_memory(db: &DbConnection, workspace: &str, number: u128) {
        db.insert_memory(
            &MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string(),
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                content: "Run cargo fmt before release.".to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                provenance_uri: Some("manual://recovery".to_owned()),
                tags: vec!["release".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .unwrap();
    }

    fn manifest_for_database(db: &DbConnection) -> Value {
        let mut value = manifest();
        for &(table, _, _) in REQUIRED_TABLES {
            row_mut(&mut value, table)["rowCount"] = json!(db.count_table_rows(table).unwrap());
        }
        value
    }

    #[test]
    fn all_required_tables_have_one_capture_and_restore_policy() {
        let names: BTreeSet<_> = REQUIRED_TABLES.iter().map(|(name, _, _)| name).collect();
        assert_eq!(names.len(), REQUIRED_TABLES.len());
        for &(name, owner, coverage) in REQUIRED_TABLES {
            let policy = super::super::backup_table_policy(name);
            assert_eq!(policy.owner, owner);
            assert_eq!(policy.disposition, "export_restore_required");
            assert_eq!(policy.coverage, coverage);
        }
        assert!(RestoreInventory::from_manifest(&manifest()).is_ok());
    }

    #[test]
    fn missing_zero_row_tables_are_not_silently_treated_as_empty() {
        for &(table, _, _) in REQUIRED_TABLES {
            let mut value = manifest();
            value["recoveryInventory"]["tables"]
                .as_array_mut()
                .unwrap()
                .retain(|row| row["table"] != table);
            assert!(RestoreInventory::from_manifest(&value).is_err(), "{table}");
        }
    }

    #[test]
    fn summary_flags_do_not_override_a_missing_or_partial_family() {
        for field in ["schemaCovered", "snapshotCovered"] {
            let mut value = manifest();
            row_mut(&mut value, "procedural_rules")[field] = json!(false);
            assert!(RestoreInventory::from_manifest(&value).is_err());
        }
        for field in ["schemaCoverageComplete", "snapshotCoverageComplete"] {
            let mut value = manifest();
            value["recoveryInventory"][field] = json!(false);
            assert!(RestoreInventory::from_manifest(&value).is_err());
        }
        for field in [
            "uncoveredRequiredTableCount",
            "uncoveredRequiredRowCount",
            "unclassifiedTableCount",
        ] {
            let mut value = manifest();
            value["recoveryInventory"][field] = json!(1);
            assert!(RestoreInventory::from_manifest(&value).is_err());
        }
    }

    #[test]
    fn archive_labels_cannot_exempt_durable_rows() {
        for (field, replacement) in [
            ("disposition", "derived_rebuildable"),
            ("disposition", "secret_rekeyed"),
            ("coverage", "rebuild_on_restore"),
        ] {
            let mut value = manifest();
            row_mut(&mut value, "pack_items")[field] = json!(replacement);
            assert!(RestoreInventory::from_manifest(&value).is_err());
        }
    }

    #[test]
    fn duplicate_tables_and_malformed_counts_are_rejected() {
        let mut duplicate = manifest();
        let row = row_mut(&mut duplicate, "sessions").clone();
        duplicate["recoveryInventory"]["tables"]
            .as_array_mut()
            .unwrap()
            .push(row);
        assert!(RestoreInventory::from_manifest(&duplicate).is_err());
        for count in [
            json!(null),
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(u64::MAX),
        ] {
            let mut value = manifest();
            row_mut(&mut value, "evidence_spans")["rowCount"] = count;
            assert!(RestoreInventory::from_manifest(&value).is_err());
        }
    }

    #[test]
    fn missing_schema_and_multi_workspace_archives_are_not_complete_side_path_restores() {
        assert!(RestoreInventory::from_manifest(&json!({})).is_err());
        let mut value = manifest();
        value["recoveryInventory"]["schema"] = json!("unknown");
        assert!(RestoreInventory::from_manifest(&value).is_err());
        for count in [0, 2] {
            let mut value = manifest();
            row_mut(&mut value, "workspaces")["rowCount"] = json!(count);
            assert!(RestoreInventory::from_manifest(&value).is_err());
        }
    }

    #[test]
    fn intentionally_rekeyed_or_rebuilt_state_is_not_required_to_round_trip() {
        let mut value = manifest();
        for table in ["mesh_peers", "graph_snapshots", "workspace_generations"] {
            let policy = super::super::backup_table_policy(table);
            value["recoveryInventory"]["tables"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "table": table,
                    "owner": policy.owner,
                    "disposition": policy.disposition,
                    "coverage": policy.coverage,
                    "rowCount": 25,
                    "schemaCovered": true,
                    "snapshotCovered": true,
                }));
        }
        let plan = RestoreInventory::from_manifest(&value).unwrap();
        assert!(!plan.expected.contains_key("mesh_peers"));
        assert!(!plan.expected.contains_key("graph_snapshots"));
        assert!(!plan.expected.contains_key("workspace_generations"));
    }

    #[test]
    fn diagnostics_do_not_echo_untrusted_inventory_names() {
        let mut value = manifest();
        row_mut(&mut value, "memories")["table"] = json!("/home/private/recovery-canary");
        let error = RestoreInventory::from_manifest(&value).err().unwrap();
        assert!(!error.message().contains("recovery-canary"));
        assert!(!error.message().contains("/home/private"));
    }

    #[test]
    fn real_migrated_store_contains_every_required_table() {
        let (_root, db, _path, _workspace) = fixture();
        let tables: BTreeSet<_> = db.list_user_tables().unwrap().into_iter().collect();
        for &(table, _, _) in REQUIRED_TABLES {
            assert!(tables.contains(table), "{table}");
        }
        let plan = RestoreInventory::from_manifest(&manifest_for_database(&db)).unwrap();
        plan.verify_connection(&db).unwrap();
    }

    #[test]
    fn real_store_omission_and_surplus_fail_but_exact_memory_and_tag_counts_pass() {
        let (_source_root, source, _source_path, source_workspace) = fixture();
        seed_memory(&source, &source_workspace, 1);
        let plan = RestoreInventory::from_manifest(&manifest_for_database(&source)).unwrap();
        let (_target_root, target, _target_path, target_workspace) = fixture();
        assert!(plan.verify_connection(&target).is_err());
        seed_memory(&target, &target_workspace, 1);
        plan.verify_connection(&target).unwrap();
        seed_memory(&target, &target_workspace, 2);
        assert!(plan.verify_connection(&target).is_err());
        // An early mismatch must not leave the connection pinned.
        target.begin_read_snapshot().unwrap();
        target.rollback_read_snapshot().unwrap();
    }

    #[test]
    fn missing_rule_family_is_detected_even_when_memory_recovery_succeeded() {
        let (_root, db, _path, workspace) = fixture();
        seed_memory(&db, &workspace, 1);
        let mut value = manifest_for_database(&db);
        row_mut(&mut value, "procedural_rules")["rowCount"] = json!(1);
        let plan = RestoreInventory::from_manifest(&value).unwrap();
        let error = plan.verify_connection(&db).err().unwrap();
        assert!(error.message().contains("procedural_rules"));
        assert!(error.message().contains("expected 1, found 0"));
    }

    #[test]
    fn restore_audit_additions_are_allowed_but_missing_source_audits_are_not() {
        let (_root, db, _path, workspace) = fixture();
        let empty = RestoreInventory::from_manifest(&manifest_for_database(&db)).unwrap();
        db.insert_audit(
            &crate::db::generate_audit_id(),
            &CreateAuditInput {
                workspace_id: Some(workspace),
                actor: Some("ee backup restore".to_owned()),
                action: "backup.restore".to_owned(),
                target_type: None,
                target_id: None,
                details: None,
            },
        )
        .unwrap();
        empty.verify_connection(&db).unwrap();
        let mut missing = manifest_for_database(&db);
        let actual = db.count_table_rows("audit_log").unwrap();
        row_mut(&mut missing, "audit_log")["rowCount"] = json!(actual + 1);
        assert!(
            RestoreInventory::from_manifest(&missing)
                .unwrap()
                .verify_connection(&db)
                .is_err()
        );
    }

    #[test]
    fn caller_owned_snapshot_is_not_rolled_back_on_nested_begin_failure() {
        let (_root, db, _path, _workspace) = fixture();
        let plan = RestoreInventory::from_manifest(&manifest_for_database(&db)).unwrap();
        db.begin_read_snapshot().unwrap();
        assert!(plan.verify_connection(&db).is_err());
        db.commit_read_snapshot().unwrap();
    }

    #[test]
    fn file_verification_reopens_read_only_and_does_not_change_memories() {
        let (root, db, path, workspace) = fixture();
        seed_memory(&db, &workspace, 1);
        let before = db.list_memories(&workspace, None, true).unwrap();
        let plan = RestoreInventory::from_manifest(&manifest_for_database(&db)).unwrap();
        // This unit test starts after archive authentication. Even an otherwise
        // empty database has durable migration-seeded TTL policies, so capture
        // them explicitly rather than letting destination defaults stand in for
        // archived state. Real authenticated restores are exercised separately.
        let chunk = crate::core::backup::BackupCurationHistory {
            schema: crate::core::backup::CURATION_HISTORY_SCHEMA.to_owned(),
            backup_id: "backup-empty".to_owned(),
            workspace_id: workspace.clone(),
            chunk_index: 0,
            chunk_count: 1,
            candidates: Vec::new(),
            policies: db.list_curation_ttl_policies().unwrap(),
            authentication: None,
        };
        assert!(!chunk.policies.is_empty());
        let captured = root.path().join("curation-history.json");
        std::fs::write(&captured, serde_json::to_vec(&chunk).unwrap()).unwrap();
        let asset = crate::core::backup::BackupRestoredDerivedAssetReport {
            path: "curation-history.json".to_owned(),
            kind: "curation_history".to_owned(),
            restore_path: captured.to_string_lossy().into_owned(),
            lab_episode_path: None,
        };
        let history = HistoryExpectation::from_assets(
            &[asset],
            "backup-empty",
            &db.get_workspace(&workspace).unwrap().unwrap(),
        )
        .unwrap();
        db.close().unwrap();
        plan.verify_database(&path, &history).unwrap();
        let reopened = DbConnection::open_file(&path).unwrap();
        assert_eq!(
            before,
            reopened.list_memories(&workspace, None, true).unwrap()
        );
    }

    #[test]
    fn migration_defaults_cannot_substitute_for_missing_captured_policy_history() {
        let (_root, db, path, workspace) = fixture();
        let before = db.list_curation_ttl_policies().unwrap();
        assert!(!before.is_empty());
        let plan = RestoreInventory::from_manifest(&manifest_for_database(&db)).unwrap();
        let history = HistoryExpectation::from_assets(
            &[],
            "backup-empty",
            &db.get_workspace(&workspace).unwrap().unwrap(),
        )
        .unwrap();
        db.close().unwrap();
        let error = plan.verify_database(&path, &history).unwrap_err();
        assert!(error.message().contains("curation_ttl_policies"));
        let reopened = DbConnection::open_file(&path).unwrap();
        assert_eq!(before, reopened.list_curation_ttl_policies().unwrap());
    }
}
