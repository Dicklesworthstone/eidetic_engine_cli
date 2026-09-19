//! Verify portable transcript provenance and evidence posture before publication.
//!
//! A pack's historical entity revision must not be recomputed during recovery,
//! so its own ledger cannot prove that the underlying evidence survived. Check
//! the admitted evidence rows independently, including redaction/admission bits.

use super::{Rows, recovery_error, storage_error};
use crate::core::backup::{
    BackupCassEvidenceChunk, BackupCassEvidenceRecord, BackupCassSessionChunk,
    BackupCassSessionRecord, BackupRestoredDerivedAssetReport, read_restored_derived_json,
};
use crate::db::{DbConnection, StoredSession};
use crate::models::DomainError;

const CASS_TABLES: &[&str] = &["sessions", "evidence_spans"];

pub(super) struct CassExpectation {
    workspace_id: String,
    rows: Rows,
}

impl CassExpectation {
    pub(super) fn from_assets(
        assets: &[BackupRestoredDerivedAssetReport],
        workspace_id: &str,
    ) -> Result<Self, DomainError> {
        let mut rows = Rows::default();
        let mut source_workspace: Option<String> = None;
        for kind in ["cass_sessions", "cass_evidence_spans"] {
            let mut chunks = assets
                .iter()
                .filter(|asset| asset.kind == kind)
                .collect::<Vec<_>>();
            chunks.sort_by(|a, b| a.path.cmp(&b.path));
            for (index, asset) in chunks.into_iter().enumerate() {
                let expected_index = u32::try_from(index)
                    .map_err(|_| recovery_error("Too many recovered CASS chunks"))?;
                let value = read_restored_derived_json(asset)?;
                if kind == "cass_sessions" {
                    let chunk: BackupCassSessionChunk = serde_json::from_value(value)
                        .map_err(|_| recovery_error("Invalid recovered session rows"))?;
                    if chunk.schema != "ee.backup.derived.cass_sessions.v1"
                        || chunk.source_locator_policy != "omitted_host_local"
                        || chunk.chunk_index != expected_index
                    {
                        return Err(recovery_error("Invalid recovered session chunk"));
                    }
                    for row in chunk.sessions {
                        check_scope(&mut source_workspace, &row.workspace_id)?;
                        // Only the explicit portable-locator/workspace projection
                        // is permitted. Do not silently resurrect source paths.
                        rows.insert_session(&row.into_restored(workspace_id.to_owned()))?;
                    }
                } else {
                    let chunk: BackupCassEvidenceChunk = serde_json::from_value(value)
                        .map_err(|_| recovery_error("Invalid recovered evidence rows"))?;
                    if chunk.schema != "ee.backup.derived.cass_evidence_spans.v1"
                        || chunk.chunk_index != expected_index
                    {
                        return Err(recovery_error("Invalid recovered evidence chunk"));
                    }
                    for mut row in chunk.evidence_spans {
                        check_scope(&mut source_workspace, &row.workspace_id)?;
                        row.workspace_id = workspace_id.to_owned();
                        rows.insert("evidence_spans", &row.id, &row)?;
                    }
                }
            }
        }
        Ok(Self {
            workspace_id: workspace_id.to_owned(),
            rows,
        })
    }

    pub(super) fn verify_connection(&self, db: &DbConnection) -> Result<(), DomainError> {
        let mut actual = Rows::default();
        for row in db.list_sessions(&self.workspace_id).map_err(storage_error)? {
            actual.insert_session(&row)?;
        }
        // Include denied and quarantined rows, not just search-admitted evidence.
        // Re-screening here would erase the history this fence must preserve.
        for span in db
            .list_evidence_spans_for_workspace(&self.workspace_id)
            .map_err(storage_error)?
        {
            let row = BackupCassEvidenceRecord::from_stored(&span);
            actual.insert("evidence_spans", &row.id, &row)?;
        }
        self.rows.verify(&actual, CASS_TABLES)
    }
}

fn check_scope(expected: &mut Option<String>, actual: &str) -> Result<(), DomainError> {
    if expected.as_deref().is_some_and(|id| id != actual) {
        return Err(recovery_error("Foreign recovered CASS row"));
    }
    if expected.is_none() {
        *expected = Some(actual.to_owned());
    }
    Ok(())
}

impl Rows {
    fn insert_session(&mut self, row: &StoredSession) -> Result<(), DomainError> {
        // The export DTO intentionally omits these three host-facing columns.
        // Include their exact restored values too: portable DTO equality alone
        // would hide a source_path injection or altered locator metadata.
        self.insert(
            "sessions",
            &row.id,
            &(
                BackupCassSessionRecord::from_stored(row),
                &row.cass_session_id,
                &row.source_path,
                &row.metadata_json,
            ),
        )
    }
}
