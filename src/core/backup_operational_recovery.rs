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
    audit_ids: BTreeSet<String>,
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
            audit_ids: BTreeSet::new(),
        };
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
                            expected.audit_ids.insert(row.id.clone());
                            expected.rows.insert("audit_log", &row.id, &row)?;
                        }
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
        for asset in assets.iter().filter(|asset| asset.kind == "lab_episode") {
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
        self.rows.verify(&actual, &["audit_log"])
    }
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
