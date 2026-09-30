//! Publication-time fidelity checks for historical context selection.
//!
//! A replay ledger can be internally valid yet differ from the admitted
//! backup. Legacy packs can legitimately have no ledger at all. Compare all
//! recorded values, including omissions, impressions and agent baselines,
//! rather than accepting a checksum-valid or merely self-consistent history.

use std::collections::BTreeMap;

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    BackupPackHistory, BackupRestoredDerivedAssetReport, PACK_HISTORY_SCHEMA,
    read_restored_derived_json,
};
use crate::db::{DbConnection, StoredPackHistory};
use crate::models::DomainError;

const PACK_TABLES: &[&str] = &[
    "pack_records",
    "pack_items",
    "pack_evidence_items",
    "pack_omissions",
    "pack_candidate_impressions",
    "pack_baselines",
];

pub(super) struct PackExpectation {
    workspace_id: String,
    rows: Rows,
    admission_order: Vec<String>,
}

impl PackExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let count = assets
            .iter()
            .filter(|asset| asset.kind == "pack_history")
            .count();
        let mut rows = Rows::default();
        let mut order = BTreeMap::new();
        let mut source_workspace: Option<String> = None;
        for asset in assets.iter().filter(|asset| asset.kind == "pack_history") {
            let chunk: BackupPackHistory =
                serde_json::from_value(read_restored_derived_json(asset)?)
                    .map_err(|_| recovery_error("Invalid recovered pack-history rows"))?;
            if chunk.schema != PACK_HISTORY_SCHEMA
                || chunk.backup_id != backup_id
                || chunk.chunk_count != count
                || chunk.chunk_index >= count
                || chunk.history.record.workspace_id != chunk.workspace_id
                || source_workspace
                    .as_deref()
                    .is_some_and(|id| id != chunk.workspace_id)
            {
                return Err(recovery_error(
                    "Incomplete or substituted recovered pack history",
                ));
            }
            source_workspace = Some(chunk.workspace_id.clone());
            let original = chunk.history;
            let mut history = original.clone();
            history.record.workspace_id = workspace_id.to_owned();
            for row in &mut history.impressions {
                if row.workspace_id != chunk.workspace_id {
                    return Err(recovery_error("Foreign recovered pack impression"));
                }
                row.workspace_id.clone_from(&history.record.workspace_id);
            }
            // Match the explicitly permitted restore projection. Do not rerun
            // selection, synthesize a missing ledger, or recalculate learned
            // scores, historical entity revisions or generation witnesses.
            history
                .rebind_recovery_ledger(&original, str::to_owned)
                .map_err(|_| recovery_error("Invalid recovered pack replay ledger"))?;
            rows.insert_pack(&history)?;
            if order.insert(chunk.chunk_index, history.record.id).is_some() {
                return Err(recovery_error(
                    "Recovered pack history repeats an admission slot",
                ));
            }
        }
        // Every index is bounded by count and unique, so the sorted values are
        // exactly the captured admission sequence, not lexical/time order.
        Ok(Self {
            workspace_id: workspace_id.to_owned(),
            rows,
            admission_order: order.into_values().collect(),
        })
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let order = db
            .list_pack_record_ids_for_recovery(&self.workspace_id)
            .map_err(storage_error)?;
        if order != self.admission_order {
            return Err(recovery_error(
                "Restored durable content differs for pack_records admission order; the restored store was not published",
            ));
        }
        let mut actual = Rows::default();
        for id in order {
            let history = db.get_pack_history_for_recovery(&id).map_err(|_| {
                recovery_error(
                    "Restored pack history is malformed; the restored store was not published",
                )
            })?;
            actual.insert_pack(&history)?;
        }
        // The record reader is workspace-scoped and each child reader starts
        // from those admitted records. Matching their projection cannot expose
        // foreign packs or orphan selections, impressions and baselines. The
        // isolated side store must contain exactly the admitted population,
        // including at the second fence after derived-state rebuilding.
        self.rows.verify_complete(&actual, db, PACK_TABLES)
    }
}

impl Rows {
    fn insert_pack(&mut self, history: &StoredPackHistory) -> Result<(), DomainError> {
        self.insert("pack_records", &history.record.id, &history.record)?;
        for row in &history.items {
            // V122 makes rank the unique slot across typed selection identities.
            self.insert("pack_items", &(&row.pack_id, row.rank), row)?;
        }
        for row in &history.evidence_items {
            self.insert(
                "pack_evidence_items",
                &(&row.pack_id, &row.evidence_id),
                row,
            )?;
        }
        for row in &history.omissions {
            self.insert("pack_omissions", &(&row.pack_id, &row.memory_id), row)?;
        }
        for row in &history.impressions {
            self.insert(
                "pack_candidate_impressions",
                &(&row.pack_id, &row.memory_id),
                row,
            )?;
        }
        for row in &history.baselines {
            self.insert(
                "pack_baselines",
                &(
                    &history.record.workspace_id,
                    &row.agent_name,
                    &row.task_key,
                    &row.pack_id,
                ),
                row,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "backup_pack_recovery_tests.rs"]
mod tests;

#[cfg(test)]
mod population_tests {
    use super::*;
    use crate::db::{CreatePackRecordInput, CreateWorkspaceInput};
    use crate::models::{PackId, WorkspaceId};
    use uuid::Uuid;

    type TestResult = Result<(), String>;

    fn workspace(db: &DbConnection, n: u128) -> Result<String, String> {
        let id = WorkspaceId::from_uuid(Uuid::from_u128(n)).to_string();
        db.insert_workspace(
            &id,
            &CreateWorkspaceInput {
                path: format!("/pack-recovery-population/{n}"),
                name: None,
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn empty_pack(db: &DbConnection, workspace_id: &str, n: u128) -> Result<String, String> {
        let id = PackId::from_uuid(Uuid::from_u128(n)).to_string();
        db.insert_pack_record_with_timings_task_lens_and_evidence(
            &id,
            &CreatePackRecordInput {
                task_paths: Vec::new(),
                workspace_id: workspace_id.to_owned(),
                query: "PRIVATE_FOREIGN_PACK_QUERY".to_owned(),
                profile: "balanced".to_owned(),
                max_tokens: 4000,
                used_tokens: 0,
                item_count: 0,
                omitted_count: 0,
                pack_hash: crate::core::backup::hash_bytes(b"empty recovery pack"),
                degraded_json: None,
                created_by: None,
            },
            &[],
            &[],
            &[],
            None,
        )
        .map_err(|error| error.to_string())?;
        Ok(id)
    }

    #[test]
    fn empty_pack_population_is_valid() -> TestResult {
        let db = DbConnection::open_memory().map_err(|error| error.to_string())?;
        db.migrate().map_err(|error| error.to_string())?;
        let target = workspace(&db, 1)?;
        PackExpectation::from_assets(&[], "empty-backup", &target)
            .map_err(|error| error.message())?
            .verify_connection(&db)
            .map_err(|error| error.message())
    }

    #[test]
    fn exact_legacy_pack_population_preserves_admission_order() -> TestResult {
        let db = DbConnection::open_memory().map_err(|error| error.to_string())?;
        db.migrate().map_err(|error| error.to_string())?;
        let target = workspace(&db, 1)?;
        let ids = vec![empty_pack(&db, &target, 80)?, empty_pack(&db, &target, 3)?];
        let mut rows = Rows::default();
        for id in &ids {
            let history = db
                .get_pack_history_for_recovery(id)
                .map_err(|error| error.to_string())?;
            rows.insert_pack(&history)
                .map_err(|error| error.message())?;
        }
        PackExpectation {
            workspace_id: target,
            rows,
            admission_order: ids,
        }
        .verify_connection(&db)
        .map_err(|error| error.message())
    }

    #[test]
    fn publication_recheck_rejects_a_pack_hidden_in_another_workspace() -> TestResult {
        let db = DbConnection::open_memory().map_err(|error| error.to_string())?;
        db.migrate().map_err(|error| error.to_string())?;
        let target = workspace(&db, 1)?;
        let foreign = workspace(&db, 2)?;
        let expected = PackExpectation::from_assets(&[], "empty-backup", &target)
            .map_err(|error| error.message())?;
        expected
            .verify_connection(&db)
            .map_err(|error| error.message())?;

        let hidden = empty_pack(&db, &foreign, 23)?;
        assert!(
            db.list_pack_record_ids_for_recovery(&target)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
        assert_eq!(
            db.count_table_rows("pack_records")
                .map_err(|error| error.to_string())?,
            1
        );
        // The old scoped-content comparison accepted this same projection.
        expected
            .rows
            .verify(&Rows::default(), PACK_TABLES)
            .map_err(|error| error.message())?;
        let error = expected
            .verify_connection(&db)
            .expect_err("a foreign pack must not pass the publication recheck");
        let message = error.message();
        assert!(message.contains("Restored durable population differs for pack_records"));
        assert!(!message.contains(&foreign));
        assert!(!message.contains(&hidden));
        assert!(!message.contains("PRIVATE_FOREIGN_PACK_QUERY"));
        Ok(())
    }
}
