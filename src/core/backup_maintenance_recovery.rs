//! Preserve the operational constraints and replay state of restored memory.
//!
//! Restoring the same number of tripwires is not enough if their actions become
//! weaker. Likewise a consumed reflection must not become reusable, and a
//! sentinel's predicate or a promoted recipe's steps must not silently change.

use std::collections::BTreeSet;

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    BackupMaintenanceHistory, BackupRestoredDerivedAssetReport, MAINTENANCE_HISTORY_SCHEMA,
    WORK_HISTORY_CHUNK_ROWS, maintenance_row_lengths, read_restored_derived_json,
};
use crate::db::{DbConnection, StoredMaintenanceHistory};
use crate::models::DomainError;

const MAINTENANCE_TABLES: &[&str] = &[
    "debt_snapshots",
    "memory_sentinel_specs",
    "reflection_request_ledger",
    "situation_records",
    "tripwires",
    "tripwire_check_events",
    "plan_recipes",
];

pub(super) struct MaintenanceExpectation {
    workspace_id: String,
    rows: Rows,
}

impl MaintenanceExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let count = assets
            .iter()
            .filter(|asset| asset.kind == "maintenance_history")
            .count();
        let mut slots = BTreeSet::new();
        let mut source_workspace: Option<String> = None;
        let mut expected = Rows::default();
        for asset in assets
            .iter()
            .filter(|asset| asset.kind == "maintenance_history")
        {
            let chunk: BackupMaintenanceHistory =
                serde_json::from_value(read_restored_derived_json(asset)?)
                    .map_err(|_| recovery_error("Invalid recovered maintenance history"))?;
            if chunk.schema != MAINTENANCE_HISTORY_SCHEMA
                || chunk.backup_id != backup_id
                || chunk.chunk_count != count
                || chunk.chunk_index >= count
                || !slots.insert(chunk.chunk_index)
                || source_workspace
                    .as_deref()
                    .is_some_and(|id| id != chunk.workspace_id)
                || maintenance_row_lengths(&chunk.rows)
                    .into_iter()
                    .any(|n| n > WORK_HISTORY_CHUNK_ROWS)
            {
                return Err(recovery_error(
                    "Incomplete or substituted recovered maintenance history",
                ));
            }
            let mut rows = chunk.rows;
            // Scope rebinding is the only allowed change after the archive's
            // redaction and identity mapping. Never rerun checks, learn from
            // historical outcomes, or manufacture new reflection challenges.
            for row in &mut rows.debt_snapshots {
                rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
            }
            for row in &mut rows.reflection_requests {
                rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
            }
            for row in &mut rows.situations {
                rebind(&mut row.workspace_scope, &chunk.workspace_id, workspace_id)?;
            }
            for row in &mut rows.tripwires {
                rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
            }
            for row in &mut rows.tripwire_checks {
                rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
            }
            for row in &mut rows.recipes {
                rebind(&mut row.workspace_id, &chunk.workspace_id, workspace_id)?;
            }
            expected.insert_maintenance(&rows)?;
            source_workspace = Some(chunk.workspace_id);
        }
        Ok(Self {
            workspace_id: workspace_id.to_owned(),
            rows: expected,
        })
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let history = db
            .maintenance_history_for_recovery(&self.workspace_id)
            .map_err(storage_error)?;
        let mut actual = Rows::default();
        actual.insert_maintenance(&history)?;
        self.rows.verify(&actual, MAINTENANCE_TABLES)
    }
}

fn rebind(value: &mut String, source: &str, target: &str) -> Result<(), DomainError> {
    if value.as_str() != source {
        return Err(recovery_error("Foreign recovered maintenance row"));
    }
    *value = target.to_owned();
    Ok(())
}

impl Rows {
    fn insert_maintenance(&mut self, rows: &StoredMaintenanceHistory) -> Result<(), DomainError> {
        for row in &rows.debt_snapshots {
            self.insert(
                "debt_snapshots",
                &(&row.workspace_id, &row.snapshot_day, row.generation),
                row,
            )?;
        }
        for row in &rows.sentinel_specs {
            self.insert("memory_sentinel_specs", &row.spec_hash, row)?;
        }
        for row in &rows.reflection_requests {
            self.insert("reflection_request_ledger", &row.request_id, row)?;
        }
        for row in &rows.situations {
            self.insert("situation_records", &row.situation_id, row)?;
        }
        for row in &rows.tripwires {
            self.insert("tripwires", &row.id, row)?;
        }
        for row in &rows.tripwire_checks {
            self.insert("tripwire_check_events", &row.id, row)?;
        }
        for row in &rows.recipes {
            self.insert("plan_recipes", &row.id, row)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "backup_maintenance_recovery_tests.rs"]
mod tests;
