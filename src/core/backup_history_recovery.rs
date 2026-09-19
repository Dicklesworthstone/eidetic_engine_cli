//! Compare admitted learned state with rows read from the unpublished store.
//!
//! Artifact byte hashes intentionally change between backups (new timestamps,
//! IDs and authentication). They are not evidence that restored rule authority,
//! feedback application state or learned agent weights survived. Keep a digest
//! of every typed row instead, keyed by its exact primary key. Expected digests
//! are captured before recovery writes; live digests are read in the same
//! snapshot as the publication fence's table counts.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::super::{
    BackupLearningHistory, BackupRecordedHistory, BackupRestoredDerivedAssetReport,
    BackupRuleSource, BackupRuleTag, LEARNING_HISTORY_SCHEMA, RECORDED_HISTORY_SCHEMA,
    WORK_HISTORY_CHUNK_ROWS, read_restored_derived_json,
};
use super::{recovery_error, storage_error};
use crate::db::DbConnection;
use crate::models::DomainError;

#[path = "backup_cass_recovery.rs"]
mod cass;
#[path = "backup_lifecycle_recovery.rs"]
mod lifecycle;
#[path = "backup_maintenance_recovery.rs"]
mod maintenance;
#[path = "backup_pack_recovery.rs"]
mod packs;
#[path = "backup_publication_recovery.rs"]
mod publication;
#[path = "backup_signal_recovery.rs"]
mod signals;
#[path = "backup_trust_recovery.rs"]
mod trust;

const LEARNING_TABLES: &[&str] = &[
    "procedural_rules",
    "rule_source_memories",
    "rule_tags",
    "feedback_events",
    "agent_context_profiles",
];

const RECORDED_TABLES: &[&str] = &["recorder_runs", "recorder_events", "rch_verify_runs"];

/// Digests bound to primary keys, not row order. Composite keys are serialized
/// as tuples: delimiter-bearing identities cannot alias one another. Only
/// hashes and keys are retained, not a second copy of the learned text corpus.
#[derive(Default)]
struct Rows(BTreeMap<&'static str, BTreeMap<Vec<u8>, blake3::Hash>>);

impl Rows {
    fn insert<K: Serialize, T: Serialize>(
        &mut self,
        table: &'static str,
        key: &K,
        row: &T,
    ) -> Result<(), DomainError> {
        let encode = || recovery_error(format!("Cannot encode recovered rows for {table}"));
        let key = serde_json::to_vec(key).map_err(|_| encode())?;
        let bytes = serde_json::to_vec(row).map_err(|_| encode())?;
        if self
            .0
            .entry(table)
            .or_default()
            .insert(key, blake3::hash(&bytes))
            .is_some()
        {
            return Err(recovery_error(format!(
                "Recovered history repeats an identity in {table}"
            )));
        }
        Ok(())
    }

    fn verify(&self, actual: &Self, tables: &[&str]) -> Result<(), DomainError> {
        for &table in tables {
            if self.0.get(table) != actual.0.get(table) {
                // Table names are binary-owned. Never expose private row keys,
                // text, provenance, agent names or authentication in errors.
                return Err(recovery_error(format!(
                    "Restored durable content differs for {table}; the restored store was not published"
                )));
            }
        }
        Ok(())
    }

    /// Scoped readers and joins must not hide extra foreign or orphan rows in
    /// this isolated recovery database, especially at the post-rebuild fence.
    fn verify_complete(
        &self,
        actual: &Self,
        db: &DbConnection,
        tables: &[&str],
    ) -> Result<(), DomainError> {
        self.verify(actual, tables)?;
        for &table in tables {
            let expected = self.0.get(table).map_or(0, BTreeMap::len);
            let count = db.count_table_rows(table).map_err(storage_error)?;
            if usize::try_from(count).ok() != Some(expected) {
                return Err(recovery_error(format!(
                    "Restored durable population differs for {table}; the restored store was not published"
                )));
            }
        }
        Ok(())
    }
}

pub(in crate::core::backup) struct HistoryExpectation {
    maintenance: maintenance::MaintenanceExpectation,
    workspace_id: String,
    learning: Rows,
    recorded: Rows,
    packs: packs::PackExpectation,
    cass: cass::CassExpectation,
    trust: trust::TrustExpectation,
    signals: signals::SignalExpectation,
    lifecycle: lifecycle::LifecycleExpectation,
}

impl HistoryExpectation {
    /// The caller has admitted manifest/asset hashes. Individual recovery
    /// writers must still verify family MACs and relationships before this
    /// expectation can be used to authorize publication.
    pub(in crate::core::backup) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let mut expected = Self {
            maintenance: maintenance::MaintenanceExpectation::from_assets(
                assets,
                backup_id,
                workspace_id,
            )?,
            workspace_id: workspace_id.to_owned(),
            learning: Rows::default(),
            recorded: Rows::default(),
            packs: packs::PackExpectation::from_assets(assets, backup_id, workspace_id)?,
            cass: cass::CassExpectation::from_assets(assets, workspace_id)?,
            trust: trust::TrustExpectation::from_assets(assets, backup_id, workspace_id)?,
            signals: signals::SignalExpectation::from_assets(assets, backup_id, workspace_id)?,
            lifecycle: lifecycle::LifecycleExpectation::from_assets(
                assets,
                backup_id,
                workspace_id,
            )?,
        };
        expected.capture_recorded_history(assets, backup_id)?;
        for asset in assets
            .iter()
            .filter(|asset| asset.kind == "learning_history")
        {
            let chunk: BackupLearningHistory =
                serde_json::from_value(read_restored_derived_json(asset)?)
                    .map_err(|_| recovery_error("Invalid recovered learning-history rows"))?;
            if chunk.schema != LEARNING_HISTORY_SCHEMA || chunk.backup_id != backup_id {
                return Err(recovery_error("Substituted recovered learning history"));
            }
            for mut row in chunk.rules {
                if row.workspace_id != chunk.workspace_id {
                    return Err(recovery_error("Foreign recovered rule"));
                }
                row.workspace_id.clone_from(&expected.workspace_id);
                expected
                    .learning
                    .insert("procedural_rules", &row.id, &row)?;
            }
            for row in chunk.sources {
                expected.learning.insert(
                    "rule_source_memories",
                    &(&row.rule_id, &row.memory_id),
                    &row,
                )?;
            }
            for row in chunk.tags {
                expected
                    .learning
                    .insert("rule_tags", &(&row.rule_id, &row.tag), &row)?;
            }
            for mut row in chunk.feedback {
                if row.workspace_id != chunk.workspace_id {
                    return Err(recovery_error("Foreign recovered feedback"));
                }
                row.workspace_id.clone_from(&expected.workspace_id);
                expected.learning.insert("feedback_events", &row.id, &row)?;
            }
            for mut row in chunk.agent_profiles {
                if row.workspace_id != chunk.workspace_id {
                    return Err(recovery_error("Foreign recovered agent profile"));
                }
                row.workspace_id.clone_from(&expected.workspace_id);
                expected.learning.insert(
                    "agent_context_profiles",
                    &(&row.agent_name, &row.memory_id),
                    &row,
                )?;
            }
        }
        Ok(expected)
    }

    fn capture_recorded_history(
        &mut self,
        assets: &[BackupRestoredDerivedAssetReport],
        backup_id: &str,
    ) -> Result<(), DomainError> {
        let count = assets
            .iter()
            .filter(|asset| asset.kind == "recorded_history")
            .count();
        let mut slots = BTreeSet::new();
        let mut source_workspace: Option<String> = None;
        for asset in assets
            .iter()
            .filter(|asset| asset.kind == "recorded_history")
        {
            let chunk: BackupRecordedHistory =
                serde_json::from_value(read_restored_derived_json(asset)?)
                    .map_err(|_| recovery_error("Invalid recovered recorded history"))?;
            if chunk.schema != RECORDED_HISTORY_SCHEMA
                || chunk.backup_id != backup_id
                || chunk.chunk_count != count
                || chunk.chunk_index >= count
                || !slots.insert(chunk.chunk_index)
                || source_workspace
                    .as_deref()
                    .is_some_and(|id| id != chunk.workspace_id)
                || [
                    chunk.runs.len(),
                    chunk.events.len(),
                    chunk.verification.len(),
                ]
                .into_iter()
                .any(|n| n > WORK_HISTORY_CHUNK_ROWS)
            {
                return Err(recovery_error("Incomplete or substituted recorded history"));
            }
            for mut row in chunk.runs {
                if let Some(workspace) = row.workspace_id.as_mut() {
                    if workspace.as_str() != chunk.workspace_id {
                        return Err(recovery_error("Foreign recovered recorder run"));
                    }
                    workspace.clone_from(&self.workspace_id);
                }
                // No recording process survives restore. This is the only
                // allowed lifecycle change; broken chains stay broken and
                // unfinished runs must not turn into successful recordings.
                if row.status == "active" {
                    row.status = "abandoned".to_owned();
                }
                self.recorded.insert("recorder_runs", &row.run_id, &row)?;
            }
            for row in chunk.events {
                self.recorded
                    .insert("recorder_events", &row.event_id, &row)?;
            }
            for entry in chunk.verification {
                let mut row = entry.row;
                if row.workspace_id != chunk.workspace_id {
                    return Err(recovery_error("Foreign recovered verification run"));
                }
                row.workspace_id.clone_from(&self.workspace_id);
                self.recorded.insert("rch_verify_runs", &row.id, &row)?;
            }
            source_workspace = Some(chunk.workspace_id);
        }
        Ok(())
    }

    fn verify_recorded_history(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for row in db
            .list_recorder_runs_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            for event in db
                .list_recorder_events(&row.run_id)
                .map_err(storage_error)?
            {
                actual.insert("recorder_events", &event.event_id, &event)?;
            }
            actual.insert("recorder_runs", &row.run_id, &row)?;
        }
        // The last argument only controls ordering, not membership. A fixed
        // instant and identity-keyed fingerprints avoid wall-clock dependence.
        for row in db
            .query_rch_verify_runs(&self.workspace_id, None, None, "1970-01-01T00:00:00Z")
            .map_err(storage_error)?
        {
            actual.insert("rch_verify_runs", &row.id, &row)?;
        }
        self.recorded.verify_complete(&actual, db, RECORDED_TABLES)
    }

    /// The caller owns one read snapshot spanning row counts and these reads.
    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for row in db
            .list_procedural_rules(&self.workspace_id, None, None, true)
            .map_err(storage_error)?
        {
            actual.insert("procedural_rules", &row.id, &row)?;
        }
        for (rule_id, ids) in db
            .list_rule_source_memory_ids_for_workspace(&self.workspace_id)
            .map_err(storage_error)?
        {
            for memory_id in ids {
                let row = BackupRuleSource {
                    rule_id: rule_id.clone(),
                    memory_id,
                };
                actual.insert(
                    "rule_source_memories",
                    &(&row.rule_id, &row.memory_id),
                    &row,
                )?;
            }
        }
        for (rule_id, tags) in db
            .list_rule_tags_for_workspace(&self.workspace_id)
            .map_err(storage_error)?
        {
            for tag in tags {
                let row = BackupRuleTag {
                    rule_id: rule_id.clone(),
                    tag,
                };
                actual.insert("rule_tags", &(&row.rule_id, &row.tag), &row)?;
            }
        }
        for row in db
            .list_feedback_events(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert("feedback_events", &row.id, &row)?;
        }
        for row in db
            .list_agent_context_profiles_for_recovery(&self.workspace_id)
            .map_err(storage_error)?
        {
            actual.insert(
                "agent_context_profiles",
                &(&row.agent_name, &row.memory_id),
                &row,
            )?;
        }
        self.learning.verify(&actual, LEARNING_TABLES)?;
        self.verify_recorded_history(db)?;
        self.packs.verify_connection(db)?;
        self.cass.verify_connection(db)?;
        self.trust.verify_connection(db)?;
        self.signals.verify_connection(db)?;
        self.lifecycle.verify_connection(db)?;
        self.maintenance.verify_connection(db)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn row_fingerprints_ignore_iteration_order_but_not_values_or_identities() {
        let mut expected = Rows::default();
        expected.insert("procedural_rules", &"a", &1_u32).unwrap();
        expected.insert("procedural_rules", &"b", &2_u32).unwrap();
        let mut reordered = Rows::default();
        reordered.insert("procedural_rules", &"b", &2_u32).unwrap();
        reordered.insert("procedural_rules", &"a", &1_u32).unwrap();
        expected.verify(&reordered, LEARNING_TABLES).unwrap();
        let mut swapped = Rows::default();
        swapped.insert("procedural_rules", &"b", &1_u32).unwrap();
        swapped.insert("procedural_rules", &"a", &2_u32).unwrap();
        assert!(expected.verify(&swapped, LEARNING_TABLES).is_err());
        assert!(reordered.insert("procedural_rules", &"a", &1_u32).is_err());
    }

    #[test]
    fn composite_fingerprints_do_not_alias_and_diagnostics_hide_private_values() {
        let mut expected = Rows::default();
        expected
            .insert(
                "agent_context_profiles",
                &("secret:a", "b"),
                &"private-text",
            )
            .unwrap();
        let mut other = Rows::default();
        other
            .insert(
                "agent_context_profiles",
                &("secret", "a:b"),
                &"private-text",
            )
            .unwrap();
        let message = expected
            .verify(&other, LEARNING_TABLES)
            .err()
            .unwrap()
            .message();
        assert!(message.contains("agent_context_profiles"));
        assert!(!message.contains("secret"));
        assert!(!message.contains("private-text"));
    }
}
