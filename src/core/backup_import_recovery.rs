//! Bind resumed imports to both their captured checkpoint and the chosen store.
//!
//! A portable CASS checkpoint legitimately changes its host path, not its query,
//! cursor, counts, failure state or timestamps. Freeze the destination workspace
//! from the authenticated manifest rather than deriving authority from a row
//! that a later writer could change alongside the checkpoint.

use std::collections::BTreeSet;

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    BackupImportHistory, BackupRestoredDerivedAssetReport, IMPORT_HISTORY_SCHEMA,
    WORK_HISTORY_CHUNK_ROWS, read_restored_derived_json, valid_cass_checkpoint_query,
};
use crate::db::{DbConnection, StoredWorkspace};
use crate::models::DomainError;

pub(super) struct ImportExpectation {
    workspace_id: String,
    rows: Rows,
}

impl ImportExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace: &StoredWorkspace,
    ) -> Result<Self, DomainError> {
        let mut rows = Rows::default();
        rows.insert("workspaces", &workspace.id, workspace)?;
        let count = assets
            .iter()
            .filter(|asset| asset.kind == "import_history")
            .count();
        let mut slots = BTreeSet::new();
        let mut sources = BTreeSet::new();
        for asset in assets.iter().filter(|asset| asset.kind == "import_history") {
            let chunk: BackupImportHistory =
                serde_json::from_value(read_restored_derived_json(asset)?)
                    .map_err(|_| recovery_error("Invalid recovered import history"))?;
            if chunk.schema != IMPORT_HISTORY_SCHEMA
                || chunk.backup_id != backup_id
                || chunk.workspace_id != workspace.id
                || chunk.chunk_count != count
                || chunk.chunk_index >= count
                || !slots.insert(chunk.chunk_index)
                || chunk.imports.is_empty()
                || chunk.imports.len() > WORK_HISTORY_CHUNK_ROWS
            {
                return Err(recovery_error(
                    "Incomplete or substituted recovered import history",
                ));
            }
            for checkpoint in chunk.imports {
                let mut row = checkpoint.ledger;
                if row.workspace_id != chunk.workspace_id {
                    return Err(recovery_error("Foreign recovered import checkpoint"));
                }
                if let Some(query) = checkpoint.cass_query {
                    if row.source_kind != "cass" || !valid_cass_checkpoint_query(&query) {
                        return Err(recovery_error("Invalid recovered CASS checkpoint query"));
                    }
                    row.source_id = format!("cass://sessions?workspace={}&{query}", workspace.path);
                }
                if !sources.insert((row.source_kind.clone(), row.source_id.clone())) {
                    return Err(recovery_error("Duplicate recovered import source"));
                }
                row.workspace_id.clone_from(&workspace.id);
                if row.status == "running" {
                    row.status = "pending".to_owned();
                    row.started_at = None;
                    row.completed_at = None;
                }
                rows.insert("import_ledger", &row.id, &row)?;
            }
        }
        Ok(Self {
            workspace_id: workspace.id.clone(),
            rows,
        })
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for workspace in db.list_workspaces().map_err(storage_error)? {
            actual.insert("workspaces", &workspace.id, &workspace)?;
        }
        for row in db
            .list_import_ledgers(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert("import_ledger", &row.id, &row)?;
        }
        self.rows
            .verify_complete(&actual, db, &["workspaces", "import_ledger"])
    }
}

#[cfg(test)]
#[path = "backup_import_recovery_tests.rs"]
mod tests;
