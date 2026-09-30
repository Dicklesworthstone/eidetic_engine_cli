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
        // Workspace filters (and the seal-to-memory join) cannot establish
        // that these are the only trust rows in the unpublished side store.
        // Recheck all four populations in the same snapshot, including after
        // derived-state rebuilding, without releasing or re-verifying a row.
        self.rows.verify_complete(&actual, db, TRUST_TABLES)
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

#[cfg(test)]
mod population_tests {
    use super::*;
    use crate::db::{
        CreateWorkspaceInput, StoredAgent, StoredCertificateRecord, StoredTrustQuarantine,
    };
    use crate::models::{
        MEMORY_SEAL_PLACEHOLDER_CONTENT, MemoryId, MemorySeal, WorkspaceId, memory_seal_commitment,
    };
    use uuid::Uuid;

    type TestResult = Result<(), String>;

    // Return the exact admitted row alongside its actual durable insertion.
    // Each case uses a separate database, so every table is independently
    // capable of rejecting a hidden population rather than masking later ones.
    fn seed(db: &DbConnection, workspace: &str, table: &str) -> Result<Rows, String> {
        let time = "2026-09-01T00:00:00Z";
        let mut rows = Rows::default();
        match table {
            "memory_seals" => {
                let id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
                db.execute_raw(&format!(
                    "UPDATE memories SET content = '{MEMORY_SEAL_PLACEHOLDER_CONTENT}' WHERE id = '{id}'"
                ))
                .map_err(|error| error.to_string())?;
                let row = MemorySeal {
                    memory_id: id,
                    content_commitment: memory_seal_commitment(b"PRIVATE_TRUST_SENTINEL"),
                    sealed_at: time.to_owned(),
                    revealed_at: None,
                    reveal_verified: None,
                };
                db.insert_memory_seal_for_recovery(&row)
                    .map_err(|error| error.to_string())?;
                rows.insert("memory_seals", &row.memory_id, &row)
                    .map_err(|error| error.message())?;
            }
            "trust_quarantine" => {
                let row = StoredTrustQuarantine {
                    workspace_id: workspace.to_owned(),
                    source_uri: "ee-test://PRIVATE_TRUST_SENTINEL".to_owned(),
                    first_event_at: time.to_owned(),
                    last_event_at: time.to_owned(),
                    harmful_event_count: 7,
                    quarantined_until: Some("2099-01-01T00:00:00Z".to_owned()),
                    reason: "PRIVATE_TRUST_SENTINEL".to_owned(),
                    status: "released".to_owned(),
                    created_at: time.to_owned(),
                    updated_at: time.to_owned(),
                };
                db.insert_trust_quarantine_for_recovery(&row)
                    .map_err(|error| error.to_string())?;
                rows.insert(
                    "trust_quarantine",
                    &(&row.workspace_id, &row.source_uri),
                    &row,
                )
                .map_err(|error| error.message())?;
            }
            "certificates" => {
                let row = StoredCertificateRecord {
                    id: "cert_population_guard".to_owned(),
                    workspace_id: workspace.to_owned(),
                    target_kind: "pack".to_owned(),
                    target_id: "pack_historical".to_owned(),
                    hash_algo: "blake3".to_owned(),
                    content_hash: crate::core::backup::hash_bytes(b"PRIVATE_TRUST_SENTINEL"),
                    signature: None,
                    signature_algorithm: None,
                    signer: None,
                    signed_at: None,
                    verified_at: None,
                    status: "pending".to_owned(),
                    manifest_path: None,
                    payload_path: None,
                    metadata_json: "{}".to_owned(),
                    created_at: time.to_owned(),
                    updated_at: time.to_owned(),
                };
                db.insert_certificate_for_recovery(&row)
                    .map_err(|error| error.to_string())?;
                rows.insert("certificates", &row.id, &row)
                    .map_err(|error| error.message())?;
            }
            "agents" => {
                let row = StoredAgent {
                    id: format!("agt_{:026}", 23),
                    workspace_id: workspace.to_owned(),
                    name: "PRIVATE_TRUST_SENTINEL".to_owned(),
                    model: Some("historical-model".to_owned()),
                    created_at: time.to_owned(),
                    last_seen_at: time.to_owned(),
                };
                db.insert_agent_for_recovery(&row)
                    .map_err(|error| error.to_string())?;
                rows.insert("agents", &row.id, &row)
                    .map_err(|error| error.message())?;
            }
            _ => return Err("unhandled trust population fixture".to_owned()),
        }
        Ok(rows)
    }

    #[test]
    fn every_trust_table_rejects_hidden_rows_and_accepts_exact_history() -> TestResult {
        for &table in TRUST_TABLES {
            let (_root, _, database) =
                crate::core::backup::tests::fixture().map_err(|error| error.message())?;
            let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
            let source = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
            let target = WorkspaceId::from_uuid(Uuid::from_u128(99)).to_string();
            db.insert_workspace(
                &target,
                &CreateWorkspaceInput {
                    path: "/trust-recovery-population/target".to_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
            let empty = TrustExpectation::from_assets(&[], "empty-backup", &target)
                .map_err(|error| error.message())?;
            empty
                .verify_connection(&db)
                .map_err(|error| error.message())?;
            let exact = TrustExpectation {
                rows: seed(&db, &source, table)?,
                workspace_id: source.clone(),
            };
            exact
                .verify_connection(&db)
                .map_err(|error| error.message())?;
            assert!(
                db.list_memory_seals_for_recovery(&target)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
            assert!(
                db.list_trust_quarantine(&target, false)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
            assert!(
                db.list_certificates_for_recovery(&target)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
            assert!(
                db.list_agents_for_recovery(&target)
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
                .expect_err("hidden trust rows must prevent publication");
            let message = error.message();
            assert!(message.contains(&format!("Restored durable population differs for {table}")));
            assert!(!message.contains(&source));
            assert!(!message.contains("PRIVATE_TRUST_SENTINEL"));
            // Refusal is read-only: it must not release quarantines, reveal
            // sealed results, verify certificates or rewrite agent identity.
            exact
                .verify_connection(&db)
                .map_err(|error| error.message())?;
            db.close().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
