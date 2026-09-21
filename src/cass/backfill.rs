//! Append newly observed CASS evidence without rewriting retained history.
//!
//! The initial import transaction deduplicates sessions, not their changing
//! transcripts. A complete bounded view must still be reconciled for an
//! existing session. Keep old evidence identities, linkage and security posture
//! exactly intact; publication work and redaction audits commit with new rows.

use std::collections::{BTreeMap, BTreeSet};

use crate::db::{CreateAuditInput, SearchIndexJobStatus, StoredEvidenceSpan};
use crate::models::AuditId;

use super::{
    CassSessionInfo, CassViewSpanForImport, DbConnection, DbError, DbOperation,
    cass_redaction_audit_input, evidence_input, saturating_len, search_index_job_input,
    stable_cass_redaction_audit_id, stable_evidence_id, stable_search_index_job_id, stable_uuid,
    with_import_session_transaction,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct BackfillResult {
    /// Only rows committed by this attempt, in source-line order.
    pub inserted_lines: Vec<u32>,
    /// A new or unfinished job for this exact observed evidence snapshot.
    pub index_job_id: Option<String>,
}

pub(super) fn backfill_session(
    connection: &DbConnection,
    workspace_id: &str,
    session_id: &str,
    session: &CassSessionInfo,
    spans: &[CassViewSpanForImport],
) -> Result<BackfillResult, DbError> {
    let mut incoming = BTreeMap::new();
    for span in spans {
        if span.start_line == 0
            || span.end_line != span.start_line
            || span.cass_span_id != format!("{}:{}", session.source_path, span.start_line)
            || span.content_hash != digest(&span.excerpt)
            || incoming.insert(span.start_line, span).is_some()
        {
            return Err(backfill_error("Invalid or repeated CASS backfill evidence"));
        }
    }
    let snapshot_hash = snapshot_hash(&incoming);
    let index_job_id = stable_search_index_job_id(
        workspace_id,
        &format!("{session_id}:cass-evidence-snapshot:{snapshot_hash}"),
    );

    with_import_session_transaction(connection, || {
        let stored_session = connection
            .get_session(session_id)?
            .ok_or_else(|| backfill_error("CASS backfill session disappeared"))?;
        if stored_session.workspace_id != workspace_id
            || stored_session.cass_session_id != session.source_path
        {
            return Err(backfill_error("CASS backfill session scope changed"));
        }

        let mut present = BTreeSet::new();
        for stored in connection.list_evidence_spans_for_session(session_id)? {
            if stored.workspace_id != workspace_id || stored.session_id != session_id {
                return Err(backfill_error("Foreign CASS backfill evidence"));
            }
            let source_line = incoming.get(&stored.start_line).copied();
            if !matches!(
                stored.producer_kind.as_str(),
                "cass_import" | "legacy_unknown"
            ) {
                // A different producer is not an import checkpoint. Do not
                // mint a second interpretation for its occupied source slot.
                if source_line.is_some_and(|span| same_upstream_reference(&stored, span)) {
                    return Err(backfill_error(
                        "CASS backfill source slot belongs to another producer",
                    ));
                }
                continue;
            }
            let Some(span) = source_line else {
                return Err(backfill_error(
                    "CASS backfill would omit retained evidence; retry with the complete transcript",
                ));
            };
            if stored.end_line != span.end_line
                || !same_upstream_reference(&stored, span)
                || stored.excerpt != span.excerpt
                || !present.insert(stored.start_line)
            {
                return Err(backfill_error(
                    "CASS backfill would rewrite retained evidence; explicit review is required",
                ));
            }
            // Do not reinsert or rescreen this row. In particular, legacy or
            // manually denied evidence must not regain authority on re-import.
        }

        let missing = incoming
            .iter()
            .filter(|(line, _)| !present.contains(*line))
            .map(|(_, span)| *span)
            .collect::<Vec<_>>();
        let existing_job = connection.get_search_index_job(&index_job_id)?;
        if let Some(job) = &existing_job {
            if job.workspace_id != workspace_id
                || job.document_source.as_deref() != Some("session")
                || job.document_id.as_deref() != Some(session_id)
                || job.status_enum().is_none()
            {
                return Err(backfill_error(
                    "CASS backfill publication job does not match its source",
                ));
            }
            if !missing.is_empty() {
                // This snapshot's rows and job are one transaction. A retained
                // job with missing rows is inconsistent recovery state, not
                // permission to reuse a completed job for new publication.
                return Err(backfill_error(
                    "CASS backfill checkpoint is inconsistent with retained evidence",
                ));
            }
        }
        if missing.is_empty() {
            return Ok(BackfillResult {
                inserted_lines: Vec::new(),
                index_job_id: existing_job
                    .filter(|job| job.status_enum() != Some(SearchIndexJobStatus::Completed))
                    .map(|_| index_job_id.clone()),
            });
        }

        let mut inserted_lines = Vec::with_capacity(missing.len());
        for span in missing {
            let evidence_id = stable_evidence_id(session_id, &span.cass_span_id);
            connection.insert_evidence_span(
                &evidence_id,
                &evidence_input(workspace_id, session_id, span),
            )?;
            if span.redacted {
                connection.insert_audit(
                    &stable_cass_redaction_audit_id(&evidence_id),
                    &cass_redaction_audit_input(workspace_id, session_id, &evidence_id, span),
                )?;
            }
            inserted_lines.push(span.end_line);
        }
        connection.insert_search_index_job(
            &index_job_id,
            &search_index_job_input(workspace_id, session_id),
        )?;
        connection.insert_audit(
            &AuditId::from_uuid(stable_uuid(&format!("audit:cass-backfill:{index_job_id}")))
                .to_string(),
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some("ee import cass".to_owned()),
                action: "cass.session.backfilled".to_owned(),
                target_type: Some("session".to_owned()),
                target_id: Some(session_id.to_owned()),
                details: Some(
                    serde_json::json!({
                        "schema": "ee.cass.backfill_audit.v1",
                        "sessionId": session_id,
                        "snapshotHash": snapshot_hash,
                        "spansAdded": saturating_len(inserted_lines.len()),
                        "spansObserved": saturating_len(incoming.len()),
                        "firstAddedLine": inserted_lines.first(),
                        "lastAddedLine": inserted_lines.last(),
                        "sourceObservationHash": session.content_hash.as_deref().map(digest),
                        "indexJobId": index_job_id,
                    })
                    .to_string(),
                ),
            },
        )?;
        // Session metadata describes its initial import snapshot. Preserve it
        // together with historical citations; this audit binds the newer
        // observation without silently revising a source already used by packs.
        Ok(BackfillResult {
            inserted_lines,
            index_job_id: Some(index_job_id.clone()),
        })
    })
}

fn same_upstream_reference(stored: &StoredEvidenceSpan, span: &CassViewSpanForImport) -> bool {
    // The live insertion boundary stores the reference hash, not the host path.
    // Older imports can retain the original locator. Neither form changes IDs.
    stored.cass_span_id == digest(&span.cass_span_id) || stored.cass_span_id == span.cass_span_id
}

fn snapshot_hash(incoming: &BTreeMap<u32, &CassViewSpanForImport>) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.backfill.snapshot.v1\0");
    for (line, span) in incoming {
        // Every component is fixed-width. No source strings or ambiguous
        // delimiter concatenations become persistent job/audit identities.
        hash.update(&line.to_le_bytes());
        hash.update(blake3::hash(span.cass_span_id.as_bytes()).as_bytes());
        hash.update(blake3::hash(span.excerpt.as_bytes()).as_bytes());
    }
    format!("blake3:{}", hash.finalize().to_hex())
}

fn digest(value: &str) -> String {
    format!("blake3:{}", blake3::hash(value.as_bytes()).to_hex())
}

fn backfill_error(message: &'static str) -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Execute,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::super::{
        SessionImportPersistResult, parse_view_line_value, persist_session_import_if_absent,
        session_input,
    };
    use super::*;
    use crate::db::CreateWorkspaceInput;
    use crate::models::WorkspaceId;

    fn span(session: &CassSessionInfo, line: u32, text: &str) -> CassViewSpanForImport {
        parse_view_line_value(
            &serde_json::json!({
                "line": line,
                "content": serde_json::json!({
                    "type": "assistant",
                    "message": {"role": "assistant", "content": text},
                }).to_string(),
            }),
            &session.source_path,
        )
        .unwrap()
    }

    fn fixture(lines: &[u32]) -> (DbConnection, String, String, CassSessionInfo) {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(7301)).to_string();
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: "/cass-backfill-fixture".to_owned(),
                name: None,
            },
        )
        .unwrap();
        let session = CassSessionInfo::new("/private/cass-backfill/session.jsonl");
        let spans = lines
            .iter()
            .map(|line| span(&session, *line, &format!("Observation {line}.")))
            .collect::<Vec<_>>();
        let SessionImportPersistResult::Imported { session_id, .. } =
            persist_session_import_if_absent(&db, &workspace, &session, &spans).unwrap()
        else {
            panic!("fixture must create a new session");
        };
        (db, workspace, session_id, session)
    }

    fn transcript(session: &CassSessionInfo, count: u32) -> Vec<CassViewSpanForImport> {
        (1..=count)
            .map(|line| span(session, line, &format!("Observation {line}.")))
            .collect()
    }

    fn counts(db: &DbConnection) -> Vec<i64> {
        [
            "sessions",
            "evidence_spans",
            "search_index_jobs",
            "audit_log",
        ]
        .into_iter()
        .map(|table| db.count_table_rows(table).unwrap())
        .collect()
    }

    #[test]
    fn appends_and_backfills_holes_without_rewriting_saved_evidence() {
        let (db, workspace, id, session) = fixture(&[1, 3]);
        db.execute_raw(
            "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied'",
        )
        .unwrap();
        let before = db.list_evidence_spans_for_session(&id).unwrap();
        let source_before = db.get_session(&id).unwrap();
        let result =
            backfill_session(&db, &workspace, &id, &session, &transcript(&session, 4)).unwrap();
        assert_eq!(result.inserted_lines, [2, 4]);
        assert!(result.index_job_id.is_some());
        for old in before {
            assert_eq!(db.get_evidence_span(&old.id).unwrap(), Some(old));
        }
        assert_eq!(db.get_session(&id).unwrap(), source_before);
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 4);
        assert_eq!(db.count_table_rows("sessions").unwrap(), 1);
        let audits = db.list_audit_by_target("session", &id, None).unwrap();
        assert_eq!(audits.len(), 1);
        assert!(
            audits[0]
                .details
                .as_deref()
                .unwrap()
                .contains("\"spansAdded\":2")
        );
        assert!(!audits[0].details.as_deref().unwrap().contains("/private/"));
    }

    #[test]
    fn metadata_only_import_can_acquire_its_transcript_later() {
        let (db, workspace, id, session) = fixture(&[]);
        let result =
            backfill_session(&db, &workspace, &id, &session, &transcript(&session, 3)).unwrap();
        assert_eq!(result.inserted_lines, [1, 2, 3]);
        assert_eq!(db.count_table_rows("sessions").unwrap(), 1);
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 3);
    }

    #[test]
    fn unchanged_import_does_not_mint_rows_audits_or_jobs() {
        let (db, workspace, id, session) = fixture(&[1, 2]);
        let before = counts(&db);
        let result =
            backfill_session(&db, &workspace, &id, &session, &transcript(&session, 2)).unwrap();
        assert_eq!(result, BackfillResult::default());
        assert_eq!(counts(&db), before);
    }

    #[test]
    fn completed_initial_job_cannot_hide_a_new_transcript_snapshot() {
        let (db, workspace, id, session) = fixture(&[1]);
        db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
            .unwrap();
        let spans = transcript(&session, 2);
        let first = backfill_session(&db, &workspace, &id, &session, &spans).unwrap();
        let job_id = first.index_job_id.as_ref().unwrap();
        assert_ne!(job_id, &stable_search_index_job_id(&workspace, &id));
        assert_eq!(
            db.get_search_index_job(job_id)
                .unwrap()
                .unwrap()
                .status_enum(),
            Some(SearchIndexJobStatus::Pending)
        );
        let before = counts(&db);
        let retry = backfill_session(&db, &workspace, &id, &session, &spans).unwrap();
        assert!(retry.inserted_lines.is_empty());
        assert_eq!(retry.index_job_id, first.index_job_id);
        assert_eq!(counts(&db), before);
        db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
            .unwrap();
        assert_eq!(
            backfill_session(&db, &workspace, &id, &session, &spans).unwrap(),
            BackfillResult::default()
        );
    }

    #[test]
    fn rewritten_or_shortened_transcripts_fail_without_partial_appends() {
        let (db, workspace, id, session) = fixture(&[1, 2]);
        let before = counts(&db);
        for spans in [
            transcript(&session, 1),
            vec![
                span(&session, 1, "PRIVATE_REWRITE_SENTINEL"),
                span(&session, 2, "Observation 2."),
                span(&session, 3, "Observation 3."),
            ],
        ] {
            let error = backfill_session(&db, &workspace, &id, &session, &spans)
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("/private/"));
            assert!(!error.contains("PRIVATE_REWRITE_SENTINEL"));
            assert_eq!(counts(&db), before);
        }
    }

    #[test]
    fn late_insert_failure_rolls_back_the_entire_backfill_batch() {
        let (db, workspace, id, session) = fixture(&[1]);
        let spans = transcript(&session, 3);
        let other = CassSessionInfo::new("/private/cass-backfill/other.jsonl");
        let other_id = super::super::stable_session_id(&workspace, &other.source_path);
        db.insert_session(&other_id, &session_input(&workspace, &other))
            .unwrap();
        let collision_id = stable_evidence_id(&id, &spans[2].cass_span_id);
        db.insert_evidence_span(
            &collision_id,
            &evidence_input(&workspace, &other_id, &spans[2]),
        )
        .unwrap();
        let before = counts(&db);
        assert!(backfill_session(&db, &workspace, &id, &session, &spans).is_err());
        assert_eq!(counts(&db), before);
        assert!(
            db.get_evidence_span(&stable_evidence_id(&id, &spans[1].cass_span_id))
                .unwrap()
                .is_none()
        );
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 1);
    }

    #[test]
    fn malformed_input_and_scope_changes_cannot_mutate_the_store() {
        let (db, workspace, id, session) = fixture(&[1]);
        let before = counts(&db);
        let mut repeated = transcript(&session, 2);
        repeated.push(repeated[1].clone());
        assert!(backfill_session(&db, &workspace, &id, &session, &repeated).is_err());
        let mut invalid_hash = transcript(&session, 2);
        invalid_hash[1].content_hash = "not-a-content-hash".to_owned();
        assert!(backfill_session(&db, &workspace, &id, &session, &invalid_hash).is_err());
        let mut changed_source = session.clone();
        changed_source.source_path = "/private/different-session.jsonl".to_owned();
        assert!(
            backfill_session(
                &db,
                &workspace,
                &id,
                &changed_source,
                &transcript(&changed_source, 2)
            )
            .is_err()
        );
        assert!(
            backfill_session(
                &db,
                "foreign-workspace",
                &id,
                &session,
                &transcript(&session, 2)
            )
            .is_err()
        );
        assert_eq!(counts(&db), before);
    }
}
