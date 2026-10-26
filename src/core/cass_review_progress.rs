//! Durable scheduling progress for bounded import-to-review batches.
//!
//! Receipts change review order only. They never authorize evidence, apply a
//! candidate, skip the canonical reviewer, or certify proposal quality. A
//! repeated import prioritizes unseen session revisions, then rechecks older
//! ones with spare capacity. Dry runs do not even open a database.

use std::path::Path;

use crate::cass::import::{CassImportReport, ImportSessionStatus};
use crate::db::{CreateAuditInput, DbConnection, StoredSession};
use crate::models::{AuditId, SessionId};
use serde_json::json;

use super::{MAX_IMPORT_REVIEW_CANDIDATES, MIN_IMPORT_REVIEW_CONFIDENCE};

const ACTION: &str = "cass.session.proposal_reviewed";
const SCHEMA: &str = "ee.cass.proposal_review_progress.v1";
// Bump when the import-review proposal policy changes. Receipt reuse is a
// scheduling hint only; every selected session still goes through live review.
const POLICY: &str = "ee.cass.import_review_policy.v1";
const UNAVAILABLE: &str = "Proposal review progress could not be verified or recorded; committed evidence and candidates were preserved. Repeat the import or review this session explicitly.";
const CHANGED: &str = "The session changed while proposal review was running; no completion receipt was recorded. Repeat the import to review its current revision.";

type ProgressResult<T> = Result<T, &'static str>;

pub(super) struct Entry {
    pub(super) session_index: usize,
    pub(super) previously_reviewed: bool,
    receipt: Option<Receipt>,
}

impl Entry {
    fn unknown(session_index: usize) -> Self {
        Self {
            session_index,
            previously_reviewed: false,
            receipt: None,
        }
    }

    /// Record only after the canonical reviewer returned successfully. This
    /// separate transaction cannot roll back an already committed proposal.
    pub(super) fn record(&self, import: &CassImportReport) -> ProgressResult<bool> {
        if import.dry_run {
            return Err(UNAVAILABLE);
        }
        let receipt = self.receipt.as_ref().ok_or(UNAVAILABLE)?;
        let path = import.database_path.as_deref().ok_or(UNAVAILABLE)?;
        // Never create a replacement store when the explicitly bound one has
        // disappeared. Ordinary open/path admission remains the DB's job.
        if !Path::new(path).is_file() {
            return Err(UNAVAILABLE);
        }
        let connection = DbConnection::open_file(path).map_err(|_| UNAVAILABLE)?;
        receipt.record(&connection)
    }
}

/// Metadata-only scheduling. Unknown sources go after verified sources, but
/// remain visible to the canonical reviewer instead of being silently dropped.
/// Refused/planned rows and invalid IDs are never queried as stored sessions.
pub(super) fn ordered(import: &CassImportReport) -> Vec<Entry> {
    let mut entries: Vec<_> = (0..import.sessions.len()).map(Entry::unknown).collect();
    if import.dry_run {
        return entries;
    }
    let Some(path) = import.database_path.as_deref().filter(|path| !path.is_empty()) else {
        return entries;
    };
    let Ok(connection) = DbConnection::open_file_read_only(path) else {
        return entries;
    };
    for entry in &mut entries {
        let source = &import.sessions[entry.session_index];
        if !matches!(source.status, ImportSessionStatus::Imported | ImportSessionStatus::Skipped) {
            continue;
        }
        let Some(id) = source.session_id.as_deref().filter(|id| {
            id.parse::<SessionId>().is_ok_and(|parsed| parsed.to_string() == *id)
        }) else {
            continue;
        };
        let Ok(Some(session)) = connection.get_session(id) else {
            continue;
        };
        let receipt = Receipt::new(&session);
        let Ok(marked) = receipt.is_recorded(&connection) else {
            // A malformed marker does not become an authorization or a
            // completed review. The canonical operation may still be retried.
            continue;
        };
        entry.previously_reviewed = marked;
        entry.receipt = Some(receipt);
    }
    sort_entries(&mut entries);
    entries
}

fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_key(|entry| {
        let priority = if entry.receipt.is_none() {
            2
        } else {
            usize::from(entry.previously_reviewed)
        };
        (priority, entry.session_index)
    });
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Receipt {
    id: String,
    workspace_id: String,
    session_id: String,
    revision: String,
}

impl Receipt {
    fn new(session: &StoredSession) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"ee.cass.proposal_review_progress.v1\0");
        for field in [
            POLICY,
            session.workspace_id.as_str(),
            session.id.as_str(),
            session.content_hash.as_str(),
            session.updated_at.as_str(),
            crate::core::cass_error_recall::CASS_ERROR_RECALL_DERIVATION,
        ] {
            hash.update(&(field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
        hash.update(&session.message_count.to_le_bytes());
        hash.update(&MAX_IMPORT_REVIEW_CANDIDATES.to_le_bytes());
        hash.update(&MIN_IMPORT_REVIEW_CONFIDENCE.to_bits().to_le_bytes());
        hash.update(&crate::db::EVIDENCE_SCREENING_VERSION.to_le_bytes());
        hash.update(&crate::db::EVIDENCE_SECURITY_POLICY_EPOCH.to_le_bytes());
        let digest = hash.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Self {
            id: AuditId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string(),
            workspace_id: session.workspace_id.clone(),
            session_id: session.id.clone(),
            revision: format!("blake3:{}", digest.to_hex()),
        }
    }

    fn details(&self) -> String {
        json!({
            "schema": SCHEMA,
            "sessionId": self.session_id,
            "sourceRevision": self.revision,
            "policy": POLICY,
            "candidateLimit": MAX_IMPORT_REVIEW_CANDIDATES,
            "minConfidence": MIN_IMPORT_REVIEW_CONFIDENCE,
            "schedulingOnly": true,
        })
        .to_string()
    }

    fn is_recorded(&self, connection: &DbConnection) -> ProgressResult<bool> {
        let Some(audit) = connection.get_audit(&self.id).map_err(|_| UNAVAILABLE)? else {
            return Ok(false);
        };
        if audit.workspace_id.as_deref() != Some(self.workspace_id.as_str())
            || audit.action != ACTION
            || audit.target_type.as_deref() != Some("session")
            || audit.target_id.as_deref() != Some(self.session_id.as_str())
            || audit.details.as_deref() != Some(self.details().as_str())
        {
            return Err(UNAVAILABLE);
        }
        Ok(true)
    }

    fn record(&self, connection: &DbConnection) -> ProgressResult<bool> {
        // The source revision is re-read under the write transaction. A
        // concurrent refresh must not lend a receipt to an unreviewed revision.
        let mut failure = UNAVAILABLE;
        connection.with_transaction(|| {
            let session = connection.get_session(&self.session_id)?.ok_or_else(|| {
                failure = CHANGED;
                invalid_progress()
            })?;
            if Self::new(&session) != *self {
                failure = CHANGED;
                return Err(invalid_progress());
            }
            if self.is_recorded(connection).map_err(|_| invalid_progress())? {
                return Ok(false);
            }
            connection.insert_audit(&self.id, &CreateAuditInput {
                workspace_id: Some(self.workspace_id.clone()),
                actor: Some("ee import cass --propose".to_owned()),
                action: ACTION.to_owned(),
                target_type: Some("session".to_owned()),
                target_id: Some(self.session_id.clone()),
                details: Some(self.details()),
            })?;
            Ok(true)
        }).map_err(|_| failure)
    }
}

fn invalid_progress() -> crate::db::DbError {
    crate::db::DbError::MalformedRow {
        operation: crate::db::DbOperation::Execute,
        message: UNAVAILABLE.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateSessionInput, CreateWorkspaceInput};

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const WORKSPACE: &str = "wsp_01234567890123456789012345";

    fn insert(connection: &DbConnection, number: u128) -> Result<StoredSession, crate::db::DbError> {
        let id = SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        connection.insert_session(&id, &CreateSessionInput {
            workspace_id: WORKSPACE.to_owned(),
            cass_session_id: format!("/private/sessions/{number}.jsonl"),
            source_path: None,
            agent_name: Some("codex".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 1,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(&number.to_le_bytes()).to_hex()),
            metadata_json: None,
        })?;
        connection.get_session(&id)?.ok_or_else(invalid_progress)
    }

    fn database() -> Result<DbConnection, crate::db::DbError> {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        connection.insert_workspace(WORKSPACE, &CreateWorkspaceInput {
            path: "/workspace/review-progress".to_owned(),
            name: None,
        })?;
        Ok(connection)
    }

    #[test]
    fn source_bound_receipt_is_idempotent_and_contains_no_transcript_or_path() -> TestResult {
        let connection = database()?;
        let session = insert(&connection, 1)?;
        let receipt = Receipt::new(&session);
        assert!(!receipt.is_recorded(&connection)?);
        assert!(receipt.record(&connection)?);
        assert!(!receipt.record(&connection)?);
        assert!(receipt.is_recorded(&connection)?);
        assert_eq!(connection.get_session(&session.id)?, Some(session));
        let details = receipt.details();
        assert!(!details.contains("/private/"));
        assert!(!details.contains("codex"));
        assert!(details.contains("\"schedulingOnly\":true"));
        Ok(())
    }

    #[test]
    fn changed_content_counts_workspace_or_revision_get_fresh_receipts() -> TestResult {
        let connection = database()?;
        let session = insert(&connection, 2)?;
        let original = Receipt::new(&session);
        for field in 0..5 {
            let mut changed = session.clone();
            match field {
                0 => changed.content_hash.push('x'),
                1 => changed.message_count += 1,
                2 => changed.updated_at.push('x'),
                3 => changed.workspace_id.push('x'),
                _ => changed.id.push('x'),
            }
            assert_ne!(Receipt::new(&changed), original);
        }
        Ok(())
    }

    #[test]
    fn bounded_repeated_batches_reach_every_unchanged_source() -> TestResult {
        let connection = database()?;
        let sessions = (1..=35).map(|n| insert(&connection, n)).collect::<Result<Vec<_>, _>>()?;
        let mut first_visits = std::collections::BTreeSet::new();
        for expected_fresh in [16, 16, 3] {
            let mut entries = sessions.iter().enumerate().map(|(index, session)| {
                let receipt = Receipt::new(session);
                Entry { session_index: index, previously_reviewed: receipt.is_recorded(&connection).unwrap(), receipt: Some(receipt) }
            }).collect::<Vec<_>>();
            sort_entries(&mut entries);
            let fresh = entries.iter().take(super::super::MAX_IMPORT_REVIEW_SESSIONS)
                .filter(|entry| !entry.previously_reviewed).count();
            assert_eq!(fresh, expected_fresh);
            for entry in entries.iter().take(super::super::MAX_IMPORT_REVIEW_SESSIONS) {
                if entry.receipt.as_ref().unwrap().record(&connection)? {
                    assert!(first_visits.insert(entry.session_index));
                }
            }
        }
        assert_eq!(first_visits.len(), sessions.len());
        Ok(())
    }

    #[test]
    fn foreign_or_malformed_receipt_is_not_completion() -> TestResult {
        let connection = database()?;
        let session = insert(&connection, 3)?;
        let receipt = Receipt::new(&session);
        connection.insert_audit(&receipt.id, &CreateAuditInput {
            workspace_id: Some(WORKSPACE.to_owned()),
            actor: None,
            action: "different.action".to_owned(),
            target_type: Some("session".to_owned()),
            target_id: Some(session.id),
            details: Some(receipt.details()),
        })?;
        assert!(receipt.is_recorded(&connection).is_err());
        assert!(receipt.record(&connection).is_err());
        Ok(())
    }

    #[test]
    fn unreviewed_sources_precede_marked_and_unverifiable_entries() -> TestResult {
        let connection = database()?;
        let first = Receipt::new(&insert(&connection, 4)?);
        let second = Receipt::new(&insert(&connection, 5)?);
        let mut entries = vec![
            Entry::unknown(0),
            Entry { session_index: 1, previously_reviewed: true, receipt: Some(first) },
            Entry { session_index: 2, previously_reviewed: false, receipt: Some(second) },
        ];
        sort_entries(&mut entries);
        assert_eq!(entries.iter().map(|entry| entry.session_index).collect::<Vec<_>>(), [2, 1, 0]);
        Ok(())
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;
    use crate::cass::import::ImportedCassSession;
    use crate::core::cass_review::review_imported_sessions;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput,
        EvidenceAdmissionTally, EvidenceProducerKind,
    };
    use crate::models::EvidenceId;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const LESSON: &str = "Always run cargo fmt --check before cutting a release tag.";

    fn source(
        connection: &DbConnection,
        workspace: &str,
        number: u128,
    ) -> Result<StoredSession, crate::db::DbError> {
        let id = SessionId::from_uuid(uuid::Uuid::from_u128(8000 + number)).to_string();
        connection.insert_session(
            &id,
            &CreateSessionInput {
                workspace_id: workspace.to_owned(),
                cass_session_id: format!("/sessions/review-{number}.jsonl"),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: format!(
                    "blake3:{}",
                    blake3::hash(&number.to_le_bytes()).to_hex()
                ),
                metadata_json: None,
            },
        )?;
        connection.get_session(&id)?.ok_or_else(invalid_progress)
    }

    fn evidence(
        connection: &DbConnection,
        session: &StoredSession,
        number: u128,
        role: &str,
    ) -> Result<(), crate::db::DbError> {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(9000 + number)).to_string();
        let excerpt = json!({
            "type": role,
            "message": {"role": role, "content": LESSON},
        })
        .to_string();
        connection.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: session.workspace_id.clone(),
                session_id: session.id.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{}:1", session.cass_session_id),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some(role.to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        Ok(())
    }

    #[test]
    fn real_review_batches_reach_a_lesson_after_thirty_three_abstentions() -> TestResult {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let workspace_path = root.join("workspace");
        std::fs::create_dir(&workspace_path)?;
        let database_path = root.join("explicit-review-store.db");
        let workspace = crate::core::workspace::stable_workspace_id(&workspace_path);
        let connection = DbConnection::open_file(&database_path)?;
        connection.migrate()?;
        connection.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: workspace_path.display().to_string(),
                name: None,
            },
        )?;
        let mut imported = Vec::new();
        for number in 1..=34_u128 {
            let session = source(&connection, &workspace, number)?;
            // These first 33 rows deliberately contain the same directive as
            // the final row, but as system records that cannot become lessons.
            // A candidate-only progress check would starve the final source.
            evidence(
                &connection,
                &session,
                number,
                if number == 34 { "assistant" } else { "system" },
            )?;
            imported.push(ImportedCassSession {
                source_path: session.cass_session_id,
                session_id: Some(session.id),
                index_job_id: None,
                status: ImportSessionStatus::Skipped,
                spans_imported: 0,
                message_count: Some(1),
                missing_metadata: Vec::new(),
            });
        }
        let last_id = imported[33].session_id.clone().ok_or("missing last session")?;
        let sources_before = connection.list_evidence_spans_for_session(&last_id)?;
        connection.close()?;
        let import = CassImportReport {
            schema: crate::models::IMPORT_CASS_SCHEMA_V1,
            workspace_path: workspace_path.display().to_string(),
            database_path: Some(database_path.display().to_string()),
            source_id: "review-resume-regression".to_owned(),
            ledger_id: None,
            dry_run: false,
            since: None,
            sessions_discovered: 34,
            sessions_imported: 0,
            sessions_skipped: 34,
            spans_imported: 0,
            index_jobs_queued: 0,
            index_required_action: None,
            status: "completed".to_owned(),
            sessions: imported,
            evidence_admission: EvidenceAdmissionTally::default(),
        };
        let first = review_imported_sessions(&import);
        assert_eq!(first.sessions_failed, 0, "{first:?}");
        assert_eq!(first.candidates_persisted, 0);
        assert_eq!(first.sessions_newly_reviewed, 16);
        assert_eq!(first.sessions_pending_review, 18);
        assert!(first.durable_mutation);

        let second = review_imported_sessions(&import);
        assert_eq!(second.sessions_failed, 0, "{second:?}");
        assert_eq!(second.candidates_persisted, 0);
        assert_eq!(second.sessions_previously_reviewed, 16);
        assert_eq!(second.sessions_newly_reviewed, 16);
        assert_eq!(second.sessions_pending_review, 2);

        let third = review_imported_sessions(&import);
        assert_eq!(third.sessions_failed, 0, "{third:?}");
        assert_eq!(third.sessions_previously_reviewed, 32);
        assert_eq!(third.sessions_newly_reviewed, 2);
        assert_eq!(third.sessions_pending_review, 0);
        assert_eq!(third.candidates_persisted, 1);
        let last = third.sessions.iter().find(|entry| entry.session_index == 33)
            .ok_or("tail review missing")?;
        assert_eq!(last.status, "reviewed");
        assert_eq!(last.session_id.as_deref(), Some(last_id.as_str()));
        assert_eq!(last.candidates.len(), 1);
        assert!(last.candidates[0].proposed_content.contains(LESSON.trim_end_matches('.')));
        assert!(third.sessions.windows(2).all(|pair| {
            pair[0].session_index < pair[1].session_index
        }));

        let repeat = review_imported_sessions(&import);
        assert_eq!(repeat.sessions_failed, 0, "{repeat:?}");
        assert_eq!(repeat.sessions_previously_reviewed, 34);
        assert_eq!(repeat.sessions_newly_reviewed, 0);
        assert_eq!(repeat.sessions_pending_review, 0);
        assert_eq!(repeat.candidates_persisted, 0);
        assert!(!repeat.durable_mutation);
        assert!(!repeat.degradations().iter().any(|entry| {
            entry.code == "cass_import_review_deferred"
        }));
        assert!(!workspace_path.join(".ee").join("ee.db").exists());
        let connection = DbConnection::open_file(&database_path)?;
        assert_eq!(connection.list_curation_candidates(&workspace, None, None, None)?.len(), 1);
        assert!(connection.list_memories(&workspace, None, false)?.is_empty());
        let sources_after = connection.list_evidence_spans_for_session(&last_id)?;
        assert_eq!(sources_after.len(), sources_before.len());
        for (before, after) in sources_before.iter().zip(&sources_after) {
            assert_eq!(after.excerpt, before.excerpt);
            assert_eq!(after.content_hash, before.content_hash);
            assert_eq!(after.canonical_provenance_uri(), before.canonical_provenance_uri());
            assert!(after.memory_id.is_none());
        }
        connection.close()?;
        Ok(())
    }

    #[test]
    fn a_refresh_between_planning_and_acknowledgement_cannot_complete_old_work() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        let workspace = "wsp_01234567890123456789012345";
        connection.insert_workspace(workspace, &CreateWorkspaceInput {
            path: "/workspace/progress-race".to_owned(),
            name: None,
        })?;
        let session = source(&connection, workspace, 35)?;
        let old = Receipt::new(&session);
        // Generated typed ID and fixed SQL constants only, never user input.
        connection.execute_raw(&format!(
            "UPDATE sessions SET message_count = 2, updated_at = '2030-01-01T00:00:00Z' WHERE id = '{}'",
            session.id,
        ))?;
        assert_eq!(old.record(&connection), Err(CHANGED));
        assert!(connection.get_audit(&old.id)?.is_none());
        let changed = connection.get_session(&session.id)?.ok_or("session missing")?;
        let current = Receipt::new(&changed);
        assert_ne!(current.id, old.id);
        assert!(current.record(&connection)?);
        assert!(!current.record(&connection)?);
        assert_eq!(connection.get_session(&session.id)?, Some(changed));
        Ok(())
    }

    #[test]
    fn failed_progress_audit_rolls_back_without_marking_or_rewriting_the_source() -> TestResult {
        let connection = DbConnection::open_memory()?;
        connection.migrate()?;
        let workspace = "wsp_01234567890123456789012345";
        connection.insert_workspace(workspace, &CreateWorkspaceInput {
            path: "/workspace/progress-rollback".to_owned(),
            name: None,
        })?;
        let session = source(&connection, workspace, 36)?;
        let receipt = Receipt::new(&session);
        connection.execute_raw(
            "CREATE TRIGGER reject_proposal_progress BEFORE INSERT ON audit_log WHEN NEW.action = 'cass.session.proposal_reviewed' BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END;",
        )?;
        assert_eq!(receipt.record(&connection), Err(UNAVAILABLE));
        assert!(!receipt.is_recorded(&connection)?);
        assert_eq!(connection.get_session(&session.id)?, Some(session));
        assert!(connection.list_memories(workspace, None, false)?.is_empty());
        Ok(())
    }
}
