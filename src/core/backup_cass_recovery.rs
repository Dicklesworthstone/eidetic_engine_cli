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
        for row in db
            .list_sessions(&self.workspace_id)
            .map_err(storage_error)?
        {
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
        // These readers are workspace-scoped. Equality alone cannot detect
        // extra rows hidden in another workspace in this isolated side store.
        // Recheck the complete population in the caller's pinned snapshot,
        // including the second publication fence after derived-state rebuild.
        self.rows.verify_complete(&actual, db, CASS_TABLES)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, EvidenceProducerKind,
    };
    use crate::models::{EvidenceId, SessionId, WorkspaceId};

    fn workspace(db: &DbConnection, n: u128) -> String {
        let id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(n)).to_string();
        db.insert_workspace(
            &id,
            &CreateWorkspaceInput {
                path: format!("/recovery-population/{n}"),
                name: None,
            },
        )
        .unwrap();
        id
    }

    fn session(db: &DbConnection, workspace_id: &str, n: u128) -> String {
        let id = SessionId::from_uuid(uuid::Uuid::from_u128(n)).to_string();
        db.insert_session(
            &id,
            &CreateSessionInput {
                workspace_id: workspace_id.to_owned(),
                cass_session_id: format!("recovery-session-{n}"),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"session").to_hex()),
                metadata_json: None,
            },
        )
        .unwrap();
        id
    }

    fn evidence(db: &DbConnection, workspace_id: &str, session_id: &str) -> String {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(1)).to_string();
        let body = "Recovery transcript evidence.";
        db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace_id.to_owned(),
                session_id: session_id.to_owned(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: "recovery-span".to_owned(),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: body.to_owned(),
                content_hash: format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex()),
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )
        .unwrap();
        id
    }

    fn capture(db: &DbConnection, workspace_id: &str) -> CassExpectation {
        let mut rows = Rows::default();
        for row in db.list_sessions(workspace_id).unwrap() {
            rows.insert_session(&row).unwrap();
        }
        for span in db.list_evidence_spans_for_workspace(workspace_id).unwrap() {
            let row = BackupCassEvidenceRecord::from_stored(&span);
            rows.insert("evidence_spans", &row.id, &row).unwrap();
        }
        CassExpectation {
            workspace_id: workspace_id.to_owned(),
            rows,
        }
    }

    #[test]
    fn empty_cass_population_is_valid() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        CassExpectation::from_assets(&[], &target)
            .unwrap()
            .verify_connection(&db)
            .unwrap();
    }

    #[test]
    fn complete_cass_population_preserves_denied_evidence_without_rescreening() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let source = session(&db, &target, 1);
        evidence(&db, &target, &source);
        db.execute_raw(
            "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied'",
        )
        .unwrap();
        let expected = capture(&db, &target);
        expected.verify_connection(&db).unwrap();
        let span = db.list_evidence_spans_for_workspace(&target).unwrap();
        assert_eq!(span.len(), 1);
        let row = BackupCassEvidenceRecord::from_stored(&span[0]);
        assert_eq!(row.search_eligibility, "denied");
        assert_eq!(row.pack_eligibility, "denied");
    }

    #[test]
    fn publication_recheck_rejects_foreign_session_hidden_by_scoped_reader() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let foreign = workspace(&db, 2);
        session(&db, &target, 1);
        let expected = capture(&db, &target);
        expected.verify_connection(&db).unwrap();

        session(&db, &foreign, 2);
        assert_eq!(db.list_sessions(&target).unwrap().len(), 1);
        assert_eq!(db.count_table_rows("sessions").unwrap(), 2);
        let error = expected.verify_connection(&db).err().unwrap();
        let message = error.message();
        assert!(message.contains("Restored durable population differs for sessions"));
        assert!(!message.contains(&foreign));
        assert!(!message.contains("recovery-session"));
    }

    #[test]
    fn evidence_population_fence_rejects_rows_outside_the_selected_workspace() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let foreign = workspace(&db, 2);
        let source = session(&db, &foreign, 2);
        evidence(&db, &foreign, &source);
        assert!(
            db.list_evidence_spans_for_workspace(&target)
                .unwrap()
                .is_empty()
        );
        let scoped = Rows::default();
        let expected = Rows::default();
        // Scoped row equality alone passes even though the side store contains
        // a transcript that was never part of the selected workspace.
        expected.verify(&scoped, &["evidence_spans"]).unwrap();
        let error = expected
            .verify_complete(&scoped, &db, &["evidence_spans"])
            .err()
            .unwrap();
        let message = error.message();
        assert!(message.contains("Restored durable population differs for evidence_spans"));
        assert!(!message.contains(&foreign));
        assert!(!message.contains("Recovery transcript evidence"));
    }
}
