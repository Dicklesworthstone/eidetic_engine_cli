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
        let count = assets
            .iter()
            .filter(|a| a.kind == "learning_signals")
            .count();
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
                || source_workspace
                    .as_deref()
                    .is_some_and(|id| id != chunk.workspace_id)
                || [
                    chunk.observations.len(),
                    chunk.quarantine.len(),
                    chunk.outcomes.len(),
                ]
                .into_iter()
                .any(|n| n > WORK_HISTORY_CHUNK_ROWS)
            {
                return Err(recovery_error(
                    "Incomplete or substituted recovered learning signals",
                ));
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
                        .ok_or_else(|| {
                            recovery_error("Recovered verified feedback lacks its identity")
                        })?;
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
        Ok(Self {
            workspace_id: workspace_id.to_owned(),
            rows,
        })
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for row in db
            .list_learning_observations(&self.workspace_id, None)
            .map_err(storage_error)?
        {
            actual.insert("learning_observations", &row.id, &row)?;
        }
        for row in db
            .list_feedback_quarantine(&self.workspace_id, None)
            .map_err(storage_error)?
        {
            actual.insert("feedback_quarantine", &row.id, &row)?;
        }
        for row in db
            .list_outcome_evidence_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert_outcome(&row)?;
        }
        // Matching this workspace's projection does not rule out extra
        // observations or releasable feedback elsewhere in the side store.
        // Count the complete populations in the caller's pinned snapshot;
        // neither admission nor this fence may replay or repair old signals.
        self.rows.verify_complete(&actual, db, SIGNAL_TABLES)
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
            &(
                &row.workspace_id,
                row.source.as_str(),
                &row.evidence_ref,
                &row.observed_at,
            ),
            row,
        )
    }
}

#[cfg(test)]
#[path = "backup_signal_recovery_tests.rs"]
mod tests;

#[cfg(test)]
mod population_tests {
    use super::*;
    use crate::db::{
        CreateWorkspaceInput, OutcomeEvidenceSource, StoredFeedbackQuarantine,
        StoredLearningObservation,
    };
    use crate::models::{MemoryId, WorkspaceId};
    use uuid::Uuid;

    type TestResult = Result<(), String>;

    fn seed(db: &DbConnection, workspace: &str, table: &str) -> Result<Rows, String> {
        let time = "2026-09-01T00:00:00Z";
        let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        let mut rows = Rows::default();
        match table {
            "learning_observations" => {
                let row = StoredLearningObservation {
                    id: "lobs_population_signal".to_owned(),
                    workspace_id: workspace.to_owned(),
                    observation_kind: "experiment_observe".to_owned(),
                    source_type: "experiment".to_owned(),
                    source_id: Some("PRIVATE_SIGNAL_SENTINEL".to_owned()),
                    target_type: "memory".to_owned(),
                    target_id: memory_id,
                    topic: Some("release".to_owned()),
                    signal: "helpful".to_owned(),
                    evidence_json: Some("{\"note\":\"measured result\"}".to_owned()),
                    observed_at: time.to_owned(),
                    created_at: time.to_owned(),
                };
                db.insert_learning_observation_for_recovery(&row)
                    .map_err(|error| error.to_string())?;
                rows.insert("learning_observations", &row.id, &row)
                    .map_err(|error| error.message())?;
            }
            "feedback_quarantine" => {
                let row = StoredFeedbackQuarantine {
                    id: format!("fq_{:026}", 23),
                    workspace_id: workspace.to_owned(),
                    source_id: "PRIVATE_SIGNAL_SENTINEL".to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: memory_id,
                    signal: "harmful".to_owned(),
                    weight: 0.5,
                    source_type: "outcome_observed".to_owned(),
                    proposed_event_id: Some(format!("fb_{:026}", 23)),
                    recorded_at: time.to_owned(),
                    reason: "Requires explicit review".to_owned(),
                    event_reason: Some("Observed failure".to_owned()),
                    evidence_json: None,
                    session_id: None,
                    raw_event_hash: crate::core::backup::hash_bytes(b"invalid original hash"),
                    status: "pending".to_owned(),
                    reviewed_at: None,
                    reviewed_by: None,
                    released_feedback_event_id: None,
                };
                assert_ne!(
                    quarantine_payload_hash(&row)
                        .map_err(|error| error.message())?
                        .as_deref(),
                    Some(row.raw_event_hash.as_str())
                );
                db.insert_feedback_quarantine_for_recovery(&row)
                    .map_err(|error| error.to_string())?;
                rows.insert("feedback_quarantine", &row.id, &row)
                    .map_err(|error| error.message())?;
            }
            "outcome_evidence_rows" => {
                let source = OutcomeEvidenceSource::ExplicitHuman;
                let mut row = StoredOutcomeEvidence {
                    workspace_id: workspace.to_owned(),
                    source,
                    evidence_family: source.evidence_family().to_owned(),
                    signal_direction: source.default_direction().unwrap_or("negative").to_owned(),
                    base_weight_milli: source.base_weight_milli(),
                    evidence_ref: "PRIVATE_SIGNAL_SENTINEL".to_owned(),
                    agent_id: Some("historical-agent".to_owned()),
                    task_id: Some("historical-task".to_owned()),
                    run_id: Some("historical-run".to_owned()),
                    observed_at: time.to_owned(),
                    provenance_hash: String::new(),
                    created_at: time.to_owned(),
                };
                row.provenance_hash = row.computed_provenance_hash();
                db.insert_outcome_evidence_for_recovery(&row)
                    .map_err(|error| error.to_string())?;
                rows.insert_outcome(&row)
                    .map_err(|error| error.message())?;
            }
            _ => return Err("unhandled signal population fixture".to_owned()),
        }
        Ok(rows)
    }

    #[test]
    fn every_signal_table_rejects_hidden_rows_without_applying_feedback() -> TestResult {
        for &table in SIGNAL_TABLES {
            let (_root, _, database) =
                crate::core::backup::tests::fixture().map_err(|error| error.message())?;
            let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
            let source = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
            let target = WorkspaceId::from_uuid(Uuid::from_u128(99)).to_string();
            db.insert_workspace(
                &target,
                &CreateWorkspaceInput {
                    path: "/signal-recovery-population/target".to_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
            let empty = SignalExpectation::from_assets(&[], "empty-backup", &target)
                .map_err(|error| error.message())?;
            empty
                .verify_connection(&db)
                .map_err(|error| error.message())?;
            let exact = SignalExpectation {
                rows: seed(&db, &source, table)?,
                workspace_id: source.clone(),
            };
            exact
                .verify_connection(&db)
                .map_err(|error| error.message())?;
            assert!(
                db.list_learning_observations(&target, None)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
            assert!(
                db.list_feedback_quarantine(&target, None)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
            assert!(
                db.list_outcome_evidence_for_recovery(&target)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
            assert_eq!(
                db.count_table_rows(table)
                    .map_err(|error| error.to_string())?,
                1
            );
            let error = empty
                .verify_connection(&db)
                .expect_err("hidden learning signals must prevent publication");
            let message = error.message();
            assert!(message.contains(&format!(
                "Restored durable population differs for {table}"
            )));
            assert!(!message.contains(&source));
            assert!(!message.contains("PRIVATE_SIGNAL_SENTINEL"));
            exact
                .verify_connection(&db)
                .map_err(|error| error.message())?;
            assert_eq!(
                db.count_table_rows("feedback_events")
                    .map_err(|error| error.to_string())?,
                0,
                "checking restored evidence must not apply feedback"
            );
            db.close().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
