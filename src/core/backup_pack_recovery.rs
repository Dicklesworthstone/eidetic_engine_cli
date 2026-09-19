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
        self.rows.verify(&actual, PACK_TABLES)
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
            self.insert("pack_evidence_items", &(&row.pack_id, &row.evidence_id), row)?;
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
