//! Preserve the evidence that drives learning, not just learned-rule counters.
//!
//! Rebinding an authenticated payload to its restored workspace is permitted;
//! changing the observation, release decision, source authority or chronology
//! is not. Invalid quarantined payloads retain their invalid source hashes.

use std::collections::BTreeSet;

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    BackupLearningSignals, BackupRestoredDerivedAssetReport, LEARNING_SIGNALS_SCHEMA,
    WORK_HISTORY_CHUNK_ROWS, quarantine_payload_hash, read_restored_derived_json,
};
use crate::db::{DbConnection, StoredOutcomeEvidence};
use crate::models::DomainError;

const SIGNAL_TABLES: &[&str] = &[
    "learning_observations",
    "feedback_quarantine",
    "outcome_evidence_rows",
];

pub(super) struct SignalExpectation {
    workspace_id: String,
    rows: Rows,
}

impl SignalExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let count = assets.iter().filter(|a| a.kind == "learning_signals").count();
        let mut slots = BTreeSet::new();
        let mut source_workspace: Option<String> = None;
        let mut rows = Rows::default();
        for asset in assets.iter().filter(|a| a.kind == "learning_signals") {
            let chunk: BackupLearningSignals =
                serde_json::from_value(read_restored_derived_json(asset)?)
                    .map_err(|_| recovery_error("Invalid recovered learning-signal rows"))?;
            if chunk.schema != LEARNING_SIGNALS_SCHEMA
                || chunk.backup_id != backup_id
                || chunk.chunk_count != count
                || chunk.chunk_index >= count
                || !slots.insert(chunk.chunk_index)
                || source_workspace.as_deref().is_some_and(|id| id != chunk.workspace_id)
                || [chunk.observations.len(), chunk.quarantine.len(), chunk.outcomes.len()]
                    .into_iter().any(|n| n > WORK_HISTORY_CHUNK_ROWS)
            {
                return Err(recovery_error("Incomplete or substituted recovered learning signals"));
            }
            source_workspace = Some(chunk.workspace_id.clone());
            for mut row in chunk.observations {
                check_scope(&row.workspace_id, &chunk.workspace_id)?;
                row.workspace_id = workspace_id.to_owned();
                rows.insert("learning_observations", &row.id, &row)?;
            }
            for entry in chunk.quarantine {
                let mut row = entry.row;
                check_scope(&row.workspace_id, &chunk.workspace_id)?;
                row.workspace_id = workspace_id.to_owned();
                if entry.payload_hash_verified {
                    row.raw_event_hash = quarantine_payload_hash(&row)
                        .map_err(|_| recovery_error("Invalid recovered feedback payload"))?
                        .ok_or_else(|| recovery_error("Recovered verified feedback lacks its identity"))?;
                }
                // An unverified source hash is evidence of an invalid payload.
                // Never repair it into a releasable event as a side effect here.
                rows.insert("feedback_quarantine", &row.id, &row)?;
            }
            for entry in chunk.outcomes {
                let mut row = entry.row;
                check_scope(&row.workspace_id, &chunk.workspace_id)?;
                row.workspace_id = workspace_id.to_owned();
                row.provenance_hash = row.computed_provenance_hash();
                rows.insert_outcome(&row)?;
            }
        }
        Ok(Self { workspace_id: workspace_id.to_owned(), rows })
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for row in db.list_learning_observations(&self.workspace_id, None).map_err(storage_error)? {
            actual.insert("learning_observations", &row.id, &row)?;
        }
        for row in db.list_feedback_quarantine(&self.workspace_id, None).map_err(storage_error)? {
            actual.insert("feedback_quarantine", &row.id, &row)?;
        }
        for row in db.list_outcome_evidence_for_recovery(&self.workspace_id).map_err(storage_error)? {
            actual.insert_outcome(&row)?;
        }
        self.rows.verify(&actual, SIGNAL_TABLES)
    }
}

fn check_scope(actual: &str, source: &str) -> Result<(), DomainError> {
    if actual != source {
        return Err(recovery_error("Foreign recovered learning-signal row"));
    }
    Ok(())
}

impl Rows {
    fn insert_outcome(&mut self, row: &StoredOutcomeEvidence) -> Result<(), DomainError> {
        self.insert(
            "outcome_evidence_rows",
            &(&row.workspace_id, row.source.as_str(), &row.evidence_ref, &row.observed_at),
            row,
        )
    }
}

#[cfg(test)]
#[path = "backup_signal_recovery_tests.rs"]
mod tests;
