//! Recovery must preserve trust decisions, not just the number of trust rows.
//!
//! A changed quarantine expiry, revealed seal, or newly verified certificate
//! can change what later retrieval admits. Compare the exact archived trust
//! state in the unpublished store's existing read snapshot, without running
//! new verification or treating recovery as evidence of increased authority.

use std::collections::BTreeSet;

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    BackupRestoredDerivedAssetReport, BackupTrustHistory, TRUST_HISTORY_SCHEMA,
    WORK_HISTORY_CHUNK_ROWS, read_restored_derived_json,
};
use crate::db::DbConnection;
use crate::models::DomainError;

const TRUST_TABLES: &[&str] = &["memory_seals", "trust_quarantine", "certificates", "agents"];

pub(super) struct TrustExpectation {
    workspace_id: String,
    rows: Rows,
}

impl TrustExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let count = assets.iter().filter(|a| a.kind == "trust_history").count();
        let mut slots = BTreeSet::new();
        let mut source_workspace: Option<String> = None;
        let mut rows = Rows::default();
        for asset in assets.iter().filter(|a| a.kind == "trust_history") {
            let chunk: BackupTrustHistory =
                serde_json::from_value(read_restored_derived_json(asset)?)
                    .map_err(|_| recovery_error("Invalid recovered trust-history rows"))?;
            if chunk.schema != TRUST_HISTORY_SCHEMA
                || chunk.backup_id != backup_id
                || chunk.chunk_count != count
                || chunk.chunk_index >= count
                || !slots.insert(chunk.chunk_index)
                || source_workspace
                    .as_deref()
                    .is_some_and(|id| id != chunk.workspace_id)
                || [
                    chunk.seals.len(),
                    chunk.quarantines.len(),
                    chunk.certificates.len(),
                    chunk.agents.len(),
                ]
                .into_iter()
                .any(|n| n > WORK_HISTORY_CHUNK_ROWS)
            {
                return Err(recovery_error(
                    "Incomplete or substituted recovered trust history",
                ));
            }
            source_workspace = Some(chunk.workspace_id.clone());
            for row in chunk.seals {
                rows.insert("memory_seals", &row.memory_id, &row)?;
            }
            for mut row in chunk.quarantines {
                check_scope(&row.workspace_id, &chunk.workspace_id)?;
                row.workspace_id = workspace_id.to_owned();
                rows.insert(
                    "trust_quarantine",
                    &(&row.workspace_id, &row.source_uri),
                    &row,
                )?;
            }
            for mut row in chunk.certificates {
                check_scope(&row.workspace_id, &chunk.workspace_id)?;
                row.workspace_id = workspace_id.to_owned();
                rows.insert("certificates", &row.id, &row)?;
            }
            for mut row in chunk.agents {
                check_scope(&row.workspace_id, &chunk.workspace_id)?;
                row.workspace_id = workspace_id.to_owned();
                rows.insert("agents", &row.id, &row)?;
            }
        }
        Ok(Self {
            workspace_id: workspace_id.to_owned(),
            rows,
        })
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for row in db
            .list_memory_seals_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert("memory_seals", &row.memory_id, &row)?;
        }
        // Include released and expired decisions too; filtering to active rows
        // would let a late release disappear from this comparison.
        for row in db
            .list_trust_quarantine(&self.workspace_id, false)
            .map_err(storage_error)?
        {
            actual.insert(
                "trust_quarantine",
                &(&row.workspace_id, &row.source_uri),
                &row,
            )?;
        }
        for row in db
            .list_certificates_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert("certificates", &row.id, &row)?;
        }
        for row in db
            .list_agents_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert("agents", &row.id, &row)?;
        }
        self.rows.verify(&actual, TRUST_TABLES)
    }
}

fn check_scope(actual: &str, source: &str) -> Result<(), DomainError> {
    if actual != source {
        return Err(recovery_error("Foreign recovered trust row"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "backup_trust_recovery_tests.rs"]
mod tests;
