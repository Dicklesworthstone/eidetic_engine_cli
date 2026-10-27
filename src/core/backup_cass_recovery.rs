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
        self.verify_observed(db, |_| Ok(()))
    }

    fn verify_observed(
        &self,
        db: &DbConnection,
        mut after_row: impl FnMut(&'static str) -> Result<(), DomainError>,
    ) -> Result<(), DomainError> {
        // The authenticated inventory already names every expected primary
        // key. Visit those exact rows rather than hydrating the workspace's
        // complete transcript (and then retaining a second digest map).
        // Primary-key equality also avoids batch IN/full-scan plans in the
        // pinned engine. This is bounded body retention, not constant work.
        for &table in CASS_TABLES {
            let expected = self.rows.0.get(table);
            for (key, hash) in expected.into_iter().flatten() {
                recovery_checkpoint()?;
                let id: String = serde_json::from_slice(key).map_err(storage_error)?;
                let bytes = match table {
                    "sessions" => {
                        let row = db
                            .get_session(&id)
                            .map_err(storage_error)?
                            .filter(|row| row.workspace_id == self.workspace_id)
                            .ok_or_else(|| content_error(table))?;
                        serde_json::to_vec(&session_projection(&row)).map_err(storage_error)?
                    }
                    _ => {
                        // Use the complete source row, never an admitted-only
                        // reader. Denied/quarantined evidence is durable history.
                        let span = db
                            .get_evidence_span(&id)
                            .map_err(storage_error)?
                            .filter(|span| span.workspace_id == self.workspace_id)
                            .ok_or_else(|| content_error(table))?;
                        serde_json::to_vec(&BackupCassEvidenceRecord::from_stored(&span))
                            .map_err(storage_error)?
                    }
                };
                recovery_checkpoint()?;
                if blake3::hash(&bytes) != *hash {
                    return Err(content_error(table));
                }
                drop(bytes);
                after_row(table)?;
                recovery_checkpoint()?;
            }
            // Exact expected-row equality plus the COMPLETE table population
            // rules out extra foreign/orphan rows and same-count substitutions.
            // The caller owns the snapshot at both publication fences.
            recovery_checkpoint()?;
            let count = db.count_table_rows(table).map_err(storage_error)?;
            recovery_checkpoint()?;
            if usize::try_from(count).ok()
                != Some(expected.map_or(0, std::collections::BTreeMap::len))
            {
                return Err(recovery_error(format!(
                    "Restored durable population differs for {table}; the restored store was not published"
                )));
            }
        }
        Ok(())
    }
}

fn content_error(table: &'static str) -> DomainError {
    recovery_error(format!(
        "Restored durable content differs for {table}; the restored store was not published"
    ))
}

fn recovery_checkpoint() -> Result<(), DomainError> {
    if let Some(cx) = asupersync::Cx::current() {
        cx.checkpoint().map_err(storage_error)?;
    }
    Ok(())
}

fn session_projection(row: &StoredSession) -> impl serde::Serialize + '_ {
    (
        BackupCassSessionRecord::from_stored(row),
        &row.cass_session_id,
        &row.source_path,
        &row.metadata_json,
    )
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
        self.insert("sessions", &row.id, &session_projection(row))
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

    fn transcript(db: &DbConnection, workspace_id: &str, session_id: &str, n: u128) -> String {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(n)).to_string();
        let text = format!("Recovery transcript {n}: {}", "界".repeat(1024));
        db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace_id.to_owned(),
                session_id: session_id.to_owned(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("recovery-source-{n}"),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex()),
                excerpt: text,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )
        .unwrap();
        id
    }

    #[test]
    fn exact_key_verification_visits_large_denied_transcripts_without_a_corpus_list() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let source = session(&db, &target, 1);
        for n in (1..=257).rev() {
            transcript(&db, &target, &source, n);
        }
        db.execute_raw(
            "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied'",
        )
        .unwrap();
        let expected = capture(&db, &target);
        let before = db.list_evidence_spans_for_workspace(&target).unwrap();
        let mut sessions = 0;
        let mut spans = 0;
        expected
            .verify_observed(&db, |table| {
                match table {
                    "sessions" => sessions += 1,
                    "evidence_spans" => spans += 1,
                    _ => panic!("unexpected family"),
                }
                Ok(())
            })
            .unwrap();
        assert_eq!((sessions, spans), (1, 257));
        assert_eq!(
            db.list_evidence_spans_for_workspace(&target).unwrap(),
            before
        );
        assert!(before.iter().all(|row| row.pack_eligibility == "denied"));
    }

    #[test]
    fn late_content_drift_refuses_the_fence_after_releasing_earlier_bodies() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let source = session(&db, &target, 1);
        for n in 1..=129 {
            transcript(&db, &target, &source, n);
        }
        let expected = capture(&db, &target);
        let last = EvidenceId::from_uuid(uuid::Uuid::from_u128(129)).to_string();
        db.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'PRIVATE_CHANGED_TRANSCRIPT' WHERE id = '{last}'"
        ))
        .unwrap();
        let mut visited = 0;
        let error = expected
            .verify_observed(&db, |table| {
                if table == "evidence_spans" {
                    visited += 1;
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(visited, 128);
        assert!(
            error
                .message()
                .contains("content differs for evidence_spans")
        );
        assert!(!error.message().contains("PRIVATE"));
        assert!(!error.message().contains(&last));
        assert_eq!(db.count_table_rows("evidence_spans").unwrap(), 129);
    }

    #[test]
    fn same_count_foreign_rebinding_and_replaced_keys_cannot_pass_point_reads() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let foreign = workspace(&db, 2);
        let source = session(&db, &target, 1);
        let id = transcript(&db, &target, &source, 1);
        let expected = capture(&db, &target);
        for mutation in [
            format!("UPDATE evidence_spans SET workspace_id = '{foreign}' WHERE id = '{id}'"),
            format!(
                "UPDATE evidence_spans SET cass_span_id = 'PRIVATE_REPLACEMENT_LOCATOR' WHERE id = '{id}'"
            ),
            format!("UPDATE sessions SET source_path = '/PRIVATE_HOST_PATH' WHERE id = '{source}'"),
        ] {
            db.execute_raw("BEGIN IMMEDIATE").unwrap();
            db.execute_raw(&mutation).unwrap();
            let error = expected.verify_connection(&db).unwrap_err();
            assert!(error.message().contains("content differs"));
            assert!(!error.message().contains("PRIVATE"));
            assert!(!error.message().contains(&foreign));
            assert_eq!(db.count_table_rows("evidence_spans").unwrap(), 1);
            assert!(
                db.begin_read_snapshot().is_err(),
                "the caller still owns its transaction"
            );
            db.execute_raw("ROLLBACK").unwrap();
            expected.verify_connection(&db).unwrap();
        }
    }

    #[test]
    fn a_late_storage_failure_keeps_the_callers_transaction_retryable() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let source = session(&db, &target, 1);
        transcript(&db, &target, &source, 1);
        let expected = capture(&db, &target);
        db.execute_raw("BEGIN IMMEDIATE").unwrap();
        let mut visits = 0;
        let error = expected
            .verify_observed(&db, |_| {
                visits += 1;
                db.execute_raw("ALTER TABLE evidence_spans RENAME TO unavailable_recovery_evidence")
                    .map_err(storage_error)
            })
            .unwrap_err();
        assert_eq!(visits, 1);
        assert!(!error.message().contains("unavailable_recovery_evidence"));
        assert!(db.begin_read_snapshot().is_err());
        db.execute_raw("ROLLBACK").unwrap();
        expected.verify_connection(&db).unwrap();
    }

    #[test]
    fn cancellation_after_a_verified_row_stops_before_the_next_source_read() {
        crate::core::run_cli_with_cx(std::time::Duration::from_secs(10), |cx| async move {
            let db = DbConnection::open_memory().unwrap();
            db.migrate().unwrap();
            let target = workspace(&db, 1);
            let source = session(&db, &target, 1);
            transcript(&db, &target, &source, 1);
            let expected = capture(&db, &target);
            db.execute_raw("BEGIN IMMEDIATE").unwrap();
            let mut visited = 0;
            let error = expected
                .verify_observed(&db, |_| {
                    visited += 1;
                    cx.cancel_with(
                        asupersync::CancelKind::User,
                        Some("cancel recovery comparison"),
                    );
                    Ok(())
                })
                .unwrap_err();
            assert_eq!(visited, 1);
            assert!(error.message().contains("Could not reconcile"));
            let _uncancelled = asupersync::Cx::set_current(None);
            assert!(db.begin_read_snapshot().is_err());
            db.execute_raw("ROLLBACK").unwrap();
            expected.verify_connection(&db).unwrap();
        })
        .unwrap();
    }

    #[test]
    fn empty_expected_evidence_still_checks_the_whole_population() {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let target = workspace(&db, 1);
        let source = session(&db, &target, 1);
        let expected = capture(&db, &target);
        transcript(&db, &target, &source, 1);
        let mut spans = 0;
        let error = expected
            .verify_observed(&db, |table| {
                if table == "evidence_spans" {
                    spans += 1;
                }
                Ok(())
            })
            .unwrap_err();
        assert_eq!(spans, 0);
        assert!(
            error
                .message()
                .contains("population differs for evidence_spans")
        );
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
