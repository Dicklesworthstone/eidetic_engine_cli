//! Bind restored review decisions and executable procedures to their archive.
//!
//! Row counts cannot distinguish an accepted proposal from a rejected one, or
//! a retired procedure from a mature one. Preserve every typed field except
//! the new clock values explicitly created by a redaction-review transition.
//! Those clocks must still be present, valid, and coherent; historical clocks
//! on unchanged rows are compared exactly.

use std::collections::BTreeSet;

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    BackupCurationHistory, BackupProcedureHistory, BackupRestoredDerivedAssetReport,
    CURATION_HISTORY_SCHEMA, PROCEDURE_HISTORY_SCHEMA, WORK_HISTORY_CHUNK_ROWS,
    read_restored_derived_json,
};
use crate::db::DbConnection;
use crate::models::DomainError;

const LIFECYCLE_TABLES: &[&str] = &[
    "curation_candidates",
    "curation_ttl_policies",
    "procedures",
    "procedure_events",
];

pub(super) struct LifecycleExpectation {
    workspace_id: String,
    rows: Rows,
    reviewed_candidates: BTreeSet<String>,
    reviewed_procedures: BTreeSet<String>,
}

impl LifecycleExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let mut expected = Self {
            workspace_id: workspace_id.to_owned(),
            rows: Rows::default(),
            reviewed_candidates: BTreeSet::new(),
            reviewed_procedures: BTreeSet::new(),
        };
        let mut source_workspace: Option<String> = None;
        for kind in ["curation_history", "procedure_history"] {
            let count = assets.iter().filter(|asset| asset.kind == kind).count();
            let mut slots = BTreeSet::new();
            for asset in assets.iter().filter(|asset| asset.kind == kind) {
                let value = read_restored_derived_json(asset)?;
                let (source, index, declared_count, lengths) = if kind == "curation_history" {
                    let chunk: BackupCurationHistory = serde_json::from_value(value)
                        .map_err(|_| recovery_error("Invalid recovered curation history"))?;
                    if chunk.schema != CURATION_HISTORY_SCHEMA || chunk.backup_id != backup_id {
                        return Err(recovery_error("Substituted recovered curation history"));
                    }
                    let lengths = [chunk.candidates.len(), chunk.policies.len()];
                    for entry in chunk.candidates {
                        let mut row = entry.candidate;
                        check_scope(&row.workspace_id, &chunk.workspace_id)?;
                        row.workspace_id = workspace_id.to_owned();
                        if entry.requires_fresh_review
                            && matches!(row.status.as_str(), "pending" | "approved")
                        {
                            expected.reviewed_candidates.insert(row.id.clone());
                            row.status = "pending".to_owned();
                            row.review_state = "needs_evidence".to_owned();
                            row.state_entered_at = None;
                            row.last_action_at = None;
                            row.snoozed_until = None;
                            row.merged_into_candidate_id = None;
                            row.ttl_policy_id = None;
                        }
                        expected.rows.insert("curation_candidates", &row.id, &row)?;
                    }
                    for row in chunk.policies {
                        expected
                            .rows
                            .insert("curation_ttl_policies", &row.id, &row)?;
                    }
                    (
                        chunk.workspace_id,
                        chunk.chunk_index,
                        chunk.chunk_count,
                        lengths,
                    )
                } else {
                    let chunk: BackupProcedureHistory = serde_json::from_value(value)
                        .map_err(|_| recovery_error("Invalid recovered procedure history"))?;
                    if chunk.schema != PROCEDURE_HISTORY_SCHEMA || chunk.backup_id != backup_id {
                        return Err(recovery_error("Substituted recovered procedure history"));
                    }
                    let lengths = [chunk.procedures.len(), chunk.events.len()];
                    for entry in chunk.procedures {
                        let mut row = entry.procedure;
                        check_scope(&row.workspace_id, &chunk.workspace_id)?;
                        row.workspace_id = workspace_id.to_owned();
                        if entry.requires_fresh_review {
                            expected.reviewed_procedures.insert(row.id.clone());
                            if row.maturity != "retired" {
                                row.maturity = "provisional".to_owned();
                            }
                            row.last_validated_at = None;
                            row.last_promoted_at = None;
                            row.updated_at.clear();
                        }
                        expected.rows.insert("procedures", &row.id, &row)?;
                    }
                    for mut row in chunk.events {
                        check_scope(&row.workspace_id, &chunk.workspace_id)?;
                        row.workspace_id = workspace_id.to_owned();
                        expected.rows.insert("procedure_events", &row.id, &row)?;
                    }
                    (
                        chunk.workspace_id,
                        chunk.chunk_index,
                        chunk.chunk_count,
                        lengths,
                    )
                };
                if declared_count != count
                    || index >= count
                    || !slots.insert(index)
                    || lengths.into_iter().any(|len| len > WORK_HISTORY_CHUNK_ROWS)
                    || source_workspace.as_deref().is_some_and(|id| id != source)
                {
                    return Err(recovery_error(
                        "Incomplete or foreign recovered lifecycle history",
                    ));
                }
                source_workspace = Some(source);
            }
        }
        Ok(expected)
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for mut row in db
            .list_curation_candidates(&self.workspace_id, None, None, None)
            .map_err(storage_error)?
        {
            if self.reviewed_candidates.contains(&row.id) {
                let valid_clock = row.state_entered_at.as_deref().is_some_and(valid_timestamp)
                    && row.state_entered_at == row.last_action_at;
                if !valid_clock {
                    return Err(changed_clock("curation_candidates"));
                }
                row.state_entered_at = None;
                row.last_action_at = None;
            }
            actual.insert("curation_candidates", &row.id, &row)?;
        }
        for row in db.list_curation_ttl_policies().map_err(storage_error)? {
            actual.insert("curation_ttl_policies", &row.id, &row)?;
        }
        for mut row in db
            .list_procedures_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            if self.reviewed_procedures.contains(&row.id) {
                if !valid_timestamp(&row.updated_at) {
                    return Err(changed_clock("procedures"));
                }
                row.updated_at.clear();
            }
            actual.insert("procedures", &row.id, &row)?;
        }
        for row in db
            .list_procedure_events_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert("procedure_events", &row.id, &row)?;
        }
        self.rows.verify(&actual, LIFECYCLE_TABLES)
    }
}

fn check_scope(actual: &str, source: &str) -> Result<(), DomainError> {
    if actual != source {
        return Err(recovery_error("Foreign recovered lifecycle row"));
    }
    Ok(())
}

fn valid_timestamp(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

fn changed_clock(table: &str) -> DomainError {
    recovery_error(format!(
        "Restored durable content differs for {table} recovery clocks; the restored store was not published"
    ))
}

#[cfg(test)]
#[path = "backup_lifecycle_recovery_tests.rs"]
mod tests;
