//! Extend a previously imported CASS session without rewriting its evidence.
//!
//! Discovery is a snapshot, not proof that an already-known session is finished.
//! Compare the complete newly screened transcript with the retained CASS rows,
//! preserve every existing identity/link/admission decision, and capture missing
//! spans with their normal DB screening. Metadata, new evidence, audits and a
//! revision-specific index job commit in one writer-fenced transaction.

use std::collections::{BTreeMap, BTreeSet};

use crate::db::{
    CreateAuditInput, CreateSessionInput, DbConnection, DbError, DbOperation, SearchIndexJobStatus,
    StoredSession,
};
use crate::models::AuditId;

use super::{
    CassSessionInfo, CassViewSpanForImport, cass_redaction_audit_input, evidence_input,
    search_index_job_input, session_input, stable_cass_redaction_audit_id, stable_evidence_id,
    stable_search_index_job_id, stable_uuid, with_import_session_transaction,
};

const CHECKPOINT_KEY: &str = "eeCassImportCheckpoint";
const CHECKPOINT_SCHEMA: &str = "ee.cass.session_checkpoint.v1";

type SessionMetadata = serde_json::Map<String, serde_json::Value>;

struct MetadataRefresh {
    values: SessionMetadata,
    checkpoint: Option<Checkpoint>,
    fields_changed: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Checkpoint {
    schema: String,
    snapshot_revision: String,
    index_job_id: String,
    previous_index_job_id: String,
}

pub(super) struct RefreshReport {
    pub changed: bool,
    pub added_lines: Vec<u32>,
    pub index_job_id: Option<String>,
}

/// The caller obtained a complete bounded `cass view` snapshot before entering
/// here. A failure never partially updates this session; earlier successfully
/// imported sessions remain committed, as in the ordinary import path.
pub(super) fn refresh_session(
    connection: &DbConnection,
    workspace_id: &str,
    session_id: &str,
    discovered: &CassSessionInfo,
    spans: &[CassViewSpanForImport],
) -> Result<RefreshReport, DbError> {
    with_import_session_transaction(connection, || {
        let stored = connection
            .get_session(session_id)?
            .ok_or_else(|| refusal("cass_refresh_session_missing"))?;
        if stored.workspace_id != workspace_id || stored.cass_session_id != discovered.source_path {
            return Err(refusal("cass_refresh_scope_mismatch"));
        }

        // Keep the raw-reference order for the existing checkpoint contract and
        // stable evidence IDs. The live DB boundary stores BLAKE3(raw reference)
        // in cass_span_id, so retained-row lookup needs a separate projection.
        // Hash incoming raw references exactly once, never already-stored keys.
        let mut incoming = BTreeMap::new();
        let mut by_stored_reference = BTreeMap::new();
        let mut lines = BTreeSet::new();
        for span in spans {
            // Reconciliation is a persistence boundary too. Do not let an
            // inconsistent source locator or payload create an unrefreshable
            // checkpoint, even when a future caller bypasses the view parser.
            if span.start_line == 0
                || span.end_line != span.start_line
                || span.cass_span_id != format!("{}:{}", discovered.source_path, span.start_line)
                || span.content_hash
                    != format!("blake3:{}", blake3::hash(span.excerpt.as_bytes()).to_hex())
            {
                return Err(refusal("cass_refresh_invalid_span"));
            }
            let stored_reference = format!(
                "blake3:{}",
                blake3::hash(span.cass_span_id.as_bytes()).to_hex()
            );
            if incoming.insert(span.cass_span_id.as_str(), span).is_some()
                || by_stored_reference.insert(stored_reference, span).is_some()
                || !lines.insert(span.start_line)
            {
                return Err(refusal("cass_refresh_duplicate_span"));
            }
        }
        let previous = connection.list_evidence_spans_for_session(session_id)?;
        let mut retained = BTreeSet::new();
        for row in &previous {
            if row.workspace_id != workspace_id || row.session_id != session_id {
                return Err(refusal("cass_refresh_scope_mismatch"));
            }
            // Existing databases may retain the original locator rather than
            // its privacy projection. Match either representation to the raw
            // incoming identity without rewriting or re-admitting the row.
            let observed = by_stored_reference
                .get(row.cass_span_id.as_str())
                .or_else(|| incoming.get(row.cass_span_id.as_str()));
            if !matches!(row.producer_kind.as_str(), "cass_import" | "legacy_unknown") {
                // Other producers may attach their own evidence to a session.
                // They cannot confer authority on a second interpretation of
                // an occupied CASS source slot, even under a different row ID.
                if observed.is_some() {
                    return Err(refusal("cass_refresh_producer_conflict"));
                }
                continue;
            }
            let Some(span) = observed else {
                return Err(refusal("cass_refresh_history_missing"));
            };
            // Retention membership is still in the raw incoming identity space,
            // just like additions and stable_evidence_id below. No stored row is
            // rewritten or re-admitted merely because its source is recognized.
            if !retained.insert(span.cass_span_id.as_str())
                || row.start_line != span.start_line
                || row.end_line != span.end_line
                || row.span_kind != span.span_kind.as_str()
                || row.role.as_deref() != span.role.map(|role| role.as_str())
                || row.excerpt != span.excerpt
                || row.content_hash != span.content_hash
            {
                return Err(refusal("cass_refresh_history_changed"));
            }
        }

        let mut input = session_input(workspace_id, discovered);
        let merged = merged_metadata(&stored, &input, workspace_id, session_id)?;
        let metadata_changed = !same_metadata(&stored, &input) || merged.fields_changed;
        let checkpoint = merged.checkpoint;
        let mut metadata = merged.values;
        let additions: Vec<_> = incoming
            .values()
            .copied()
            .filter(|span| !retained.contains(span.cass_span_id.as_str()))
            .collect();
        let revision = snapshot_revision(workspace_id, session_id, &input, &incoming);
        let changed = metadata_changed
            || !additions.is_empty()
            || checkpoint
                .as_ref()
                .is_some_and(|saved| saved.snapshot_revision != revision);
        let previous_job = checkpoint.as_ref().map_or_else(
            || stable_search_index_job_id(workspace_id, session_id),
            |saved| saved.index_job_id.clone(),
        );

        if !changed {
            // The checkpoint survives a failed/cancelled publication. Missing
            // durable work can be reconstructed, as for the original import.
            let index_job_id =
                pending_job(connection, workspace_id, session_id, &previous_job, true)?;
            return Ok(RefreshReport {
                changed: false,
                added_lines: Vec::new(),
                index_job_id,
            });
        }

        // Bind each transition to its predecessor, not only the new payload.
        // A legitimate A -> B -> A -> B history must get new publication work
        // rather than reusing the completed job from the first B snapshot.
        let job_id = refresh_job_id(workspace_id, session_id, &previous_job, &revision);
        if connection.get_search_index_job(&job_id)?.is_some() {
            return Err(refusal("cass_refresh_revision_already_exists"));
        }
        let checkpoint = Checkpoint {
            schema: CHECKPOINT_SCHEMA.to_owned(),
            snapshot_revision: revision.clone(),
            index_job_id: job_id.clone(),
            previous_index_job_id: previous_job,
        };
        metadata.insert(
            CHECKPOINT_KEY.to_owned(),
            serde_json::to_value(&checkpoint)
                .map_err(|_| refusal("cass_refresh_checkpoint_invalid"))?,
        );
        input.metadata_json = Some(serde_json::Value::Object(metadata).to_string());

        let mut added_lines = Vec::with_capacity(additions.len());
        for span in additions {
            let evidence_id = stable_evidence_id(session_id, &span.cass_span_id);
            if connection.get_evidence_span(&evidence_id)?.is_some() {
                return Err(refusal("cass_refresh_evidence_identity_exists"));
            }
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
            added_lines.push(span.end_line);
        }
        // Persist the checkpoint even when discovery fields did not change.
        // Its job and every captured row belong to this same transaction.
        update_metadata(connection, workspace_id, session_id, &input)?;
        connection
            .insert_search_index_job(&job_id, &search_index_job_input(workspace_id, session_id))?;
        // Audit identity follows the transition too: revisiting the same
        // payload is another recorded refresh, not the old audit row.
        let audit_id = AuditId::from_uuid(stable_uuid(&format!(
            "audit:cass-refresh:{session_id}:{job_id}"
        )))
        .to_string();
        connection.insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some("ee import cass".to_owned()),
                action: "cass.session.refreshed".to_owned(),
                target_type: Some("session".to_owned()),
                target_id: Some(session_id.to_owned()),
                details: Some(
                    serde_json::json!({
                        "schema": "ee.cass.refresh_audit.v1",
                        "sessionId": session_id,
                        "snapshotRevision": revision,
                        "addedSpanCount": added_lines.len(),
                        "retainedCassSpanCount": retained.len(),
                        "metadataChanged": metadata_changed,
                    })
                    .to_string(),
                ),
            },
        )?;
        added_lines.sort_unstable();
        Ok(RefreshReport {
            changed: true,
            added_lines,
            index_job_id: Some(job_id),
        })
    })
}

fn same_metadata(stored: &StoredSession, input: &CreateSessionInput) -> bool {
    stored.agent_name == input.agent_name
        && stored.started_at == input.started_at
        && stored.ended_at == input.ended_at
        && stored.message_count == input.message_count
        && stored.token_count == input.token_count
        && stored.content_hash == input.content_hash
}

fn refresh_job_id(
    workspace_id: &str,
    session_id: &str,
    previous_job: &str,
    revision: &str,
) -> String {
    stable_search_index_job_id(
        workspace_id,
        &format!("refresh:{session_id}:{previous_job}:{revision}"),
    )
}

/// Only the importer's discovery fields are refreshed. Preserve unrelated
/// session annotations, and keep the resumable checkpoint out of the observed
/// metadata comparison so an unchanged retry does not create another write.
fn merged_metadata(
    stored: &StoredSession,
    input: &CreateSessionInput,
    workspace_id: &str,
    session_id: &str,
) -> Result<MetadataRefresh, DbError> {
    fn object(value: Option<&str>) -> Result<SessionMetadata, DbError> {
        match value {
            None => Ok(serde_json::Map::new()),
            Some(value) => serde_json::from_str::<serde_json::Value>(value)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .ok_or_else(|| refusal("cass_refresh_metadata_invalid")),
        }
    }
    let mut stored_metadata = object(stored.metadata_json.as_deref())?;
    let checkpoint: Option<Checkpoint> = stored_metadata
        .remove(CHECKPOINT_KEY)
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| refusal("cass_refresh_checkpoint_invalid"))?;
    if let Some(saved) = &checkpoint {
        let valid_revision = saved
            .snapshot_revision
            .strip_prefix("blake3:")
            .is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            });
        if saved.schema != CHECKPOINT_SCHEMA
            || !valid_revision
            || saved.index_job_id == saved.previous_index_job_id
            || saved.index_job_id
                != refresh_job_id(
                    workspace_id,
                    session_id,
                    &saved.previous_index_job_id,
                    &saved.snapshot_revision,
                )
        {
            return Err(refusal("cass_refresh_checkpoint_invalid"));
        }
    }
    let incoming = object(input.metadata_json.as_deref())?;
    let changed = incoming
        .iter()
        .any(|(key, value)| stored_metadata.get(key) != Some(value));
    stored_metadata.extend(incoming);
    Ok(MetadataRefresh {
        values: stored_metadata,
        checkpoint,
        fields_changed: changed,
    })
}

/// Model attribution and locator authority are not supplied by discovery and
/// must not be erased on refresh. V097 invalidates material session updates.
fn update_metadata(
    connection: &DbConnection,
    workspace_id: &str,
    session_id: &str,
    input: &CreateSessionInput,
) -> Result<(), DbError> {
    // This public DbConnection surface accepts complete SQL, not parameters.
    // Encode text as UTF-8 hex literals: even quotes, NUL and delimiter-bearing
    // metadata cannot become SQL. Do not propagate SQL-bearing driver errors.
    let query = format!(
        "UPDATE sessions SET agent_name = {}, started_at = {}, ended_at = {}, message_count = {}, token_count = {}, content_hash = {}, metadata_json = {} WHERE id = {} AND workspace_id = {}",
        optional_text(input.agent_name.as_deref()),
        optional_text(input.started_at.as_deref()),
        optional_text(input.ended_at.as_deref()),
        input.message_count,
        input
            .token_count
            .map_or_else(|| "NULL".to_owned(), |count| count.to_string()),
        sql_text(&input.content_hash),
        optional_text(input.metadata_json.as_deref()),
        sql_text(session_id),
        sql_text(workspace_id),
    );
    connection
        .execute_raw(&query)
        .map(|_| ())
        .map_err(|_| refusal("cass_refresh_metadata_write_failed"))
}

fn optional_text(value: Option<&str>) -> String {
    value.map_or_else(|| "NULL".to_owned(), sql_text)
}

fn sql_text(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(value.len().saturating_mul(2));
    for byte in value.bytes() {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 15)]));
    }
    format!("CAST(X'{hex}' AS TEXT)")
}

fn snapshot_revision(
    workspace_id: &str,
    session_id: &str,
    input: &CreateSessionInput,
    spans: &BTreeMap<&str, &CassViewSpanForImport>,
) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.refresh_snapshot.v1\0");
    let metadata = serde_json::json!([
        workspace_id,
        session_id,
        input.agent_name,
        input.started_at,
        input.ended_at,
        input.message_count,
        input.token_count,
        input.content_hash,
        input.metadata_json
    ])
    .to_string();
    hash.update(&(metadata.len() as u64).to_le_bytes());
    hash.update(metadata.as_bytes());
    for (reference, span) in spans {
        let identity = serde_json::json!([
            reference,
            span.start_line,
            span.end_line,
            span.span_kind.as_str(),
            span.role.map(|role| role.as_str()),
            span.content_hash,
            span.redacted,
            span.redacted_reasons
        ])
        .to_string();
        hash.update(&(identity.len() as u64).to_le_bytes());
        hash.update(identity.as_bytes());
    }
    format!("blake3:{}", hash.finalize().to_hex())
}

fn pending_job(
    connection: &DbConnection,
    workspace_id: &str,
    session_id: &str,
    job_id: &str,
    create_missing: bool,
) -> Result<Option<String>, DbError> {
    match connection.get_search_index_job(job_id)? {
        Some(job) => {
            if job.workspace_id != workspace_id
                || job.document_source.as_deref() != Some("session")
                || job.document_id.as_deref() != Some(session_id)
            {
                return Err(refusal("cass_refresh_index_job_scope_mismatch"));
            }
            Ok((job.status_enum() != Some(SearchIndexJobStatus::Completed))
                .then(|| job_id.to_owned()))
        }
        None if create_missing => {
            connection.insert_search_index_job(
                job_id,
                &search_index_job_input(workspace_id, session_id),
            )?;
            Ok(Some(job_id.to_owned()))
        }
        None => Err(refusal("cass_refresh_index_job_missing")),
    }
}

fn refusal(code: &str) -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Execute,
        message: format!(
            "{code}: CASS session refresh was not committed; retry a complete stable transcript, or inspect the original session before replacing retained history"
        ),
    }
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;

#[cfg(test)]
mod canonical_reference_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::cass::CassAgent;
    use crate::db::CreateWorkspaceInput;
    use crate::models::WorkspaceId;
    use serde_json::json;

    fn span(session: &CassSessionInfo, line: u32) -> CassViewSpanForImport {
        super::super::parse_view_line_value(
            &json!({"line": line, "content": format!("Build observation {line}. 調査完了")}),
            &session.source_path,
        )
        .unwrap()
    }

    fn fixture(source: &str, count: u32) -> (DbConnection, String, String, CassSessionInfo) {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(8711)).to_string();
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: "/canonical-refresh-workspace".to_owned(),
                name: None,
            },
        )
        .unwrap();
        let session = CassSessionInfo::new(source).with_agent(CassAgent::Codex);
        let spans: Vec<_> = (1..=count).map(|line| span(&session, line)).collect();
        let result =
            super::super::persist_session_import_if_absent(&db, &workspace, &session, &spans)
                .unwrap();
        let super::super::SessionImportPersistResult::Imported { session_id, .. } = result else {
            panic!("new fixture must import");
        };
        (db, workspace, session_id, session)
    }

    #[test]
    fn live_storage_projection_survives_retries_and_repeated_growth() {
        let (db, workspace, id, session) = fixture("/private/canonical-session.jsonl", 1);
        let first = span(&session, 1);
        let first_id = stable_evidence_id(&id, &first.cass_span_id);
        let before = db.get_evidence_span(&first_id).unwrap().unwrap();
        let digest = format!(
            "blake3:{}",
            blake3::hash(first.cass_span_id.as_bytes()).to_hex()
        );
        assert_eq!(before.cass_span_id, digest);
        assert_ne!(before.cass_span_id, first.cass_span_id);
        assert_eq!(before.upstream_ref_hash.as_deref(), Some(digest.as_str()));
        let original_job = stable_search_index_job_id(&workspace, &id);
        let retry = refresh_session(&db, &workspace, &id, &session, &[first]).unwrap();
        assert!(!retry.changed);
        assert_eq!(retry.index_job_id.as_deref(), Some(original_job.as_str()));

        for count in [2, 12] {
            let mut spans: Vec<_> = (1..=count).map(|line| span(&session, line)).collect();
            let report = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
            assert!(report.changed);
            let first_added = if count == 2 { 2 } else { 3 };
            assert_eq!(
                report.added_lines,
                (first_added..=count).collect::<Vec<_>>()
            );
            let job = report.index_job_id.unwrap();
            assert_ne!(job, original_job);
            let stored = db.get_session(&id).unwrap().unwrap();
            let metadata: serde_json::Value =
                serde_json::from_str(stored.metadata_json.as_deref().unwrap()).unwrap();
            let raw: BTreeMap<_, _> = spans
                .iter()
                .map(|span| (span.cass_span_id.as_str(), span))
                .collect();
            assert_eq!(
                metadata[CHECKPOINT_KEY]["snapshotRevision"],
                snapshot_revision(&workspace, &id, &session_input(&workspace, &session), &raw)
            );
            spans.reverse();
            let retry = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
            assert!(!retry.changed);
            assert!(retry.added_lines.is_empty());
            assert_eq!(retry.index_job_id.as_deref(), Some(job.as_str()));
            assert_eq!(db.get_session(&id).unwrap().unwrap(), stored);
            assert_eq!(db.get_evidence_span(&first_id).unwrap().unwrap(), before);
            assert_eq!(
                db.list_evidence_spans_for_session(&id).unwrap().len(),
                count as usize
            );
            for incoming in &spans {
                let expected_id = stable_evidence_id(&id, &incoming.cass_span_id);
                let row = db.get_evidence_span(&expected_id).unwrap().unwrap();
                assert_eq!(row.excerpt, incoming.excerpt);
            }
        }
    }

    #[test]
    fn backfilled_rows_are_retained_on_the_next_refresh() {
        let (db, workspace, id, session) = fixture("/private/backfill-session.jsonl", 0);
        for count in [2, 3] {
            let spans: Vec<_> = (1..=count).map(|line| span(&session, line)).collect();
            let report = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
            assert_eq!(
                report.added_lines,
                if count == 2 { vec![1, 2] } else { vec![3] }
            );
            let rows = db.list_evidence_spans_for_session(&id).unwrap();
            assert_eq!(rows.len(), count as usize);
            let retry = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
            assert!(!retry.changed);
            assert_eq!(db.list_evidence_spans_for_session(&id).unwrap(), rows);
        }
    }

    #[test]
    fn unicode_and_digest_looking_upstream_references_are_hashed_exactly_once() {
        for source in [
            "/private/資料:session.jsonl".to_owned(),
            format!("blake3:{}", "a".repeat(64)),
        ] {
            let (db, workspace, id, session) = fixture(&source, 1);
            let incoming = span(&session, 1);
            let expected_id = stable_evidence_id(&id, &incoming.cass_span_id);
            let before = db.get_evidence_span(&expected_id).unwrap().unwrap();
            let report = refresh_session(&db, &workspace, &id, &session, &[incoming]).unwrap();
            assert!(!report.changed);
            assert_eq!(db.get_evidence_span(&expected_id).unwrap().unwrap(), before);
        }
    }

    #[test]
    fn canonical_lookup_preserves_denial_but_refuses_substituted_reference() {
        let (db, workspace, id, session) = fixture("/private/denied-session.jsonl", 1);
        let first = span(&session, 1);
        let first_id = stable_evidence_id(&id, &first.cass_span_id);
        db.execute_raw(
            "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied'",
        )
        .unwrap();
        let denied = db.get_evidence_span(&first_id).unwrap().unwrap();
        let incoming = vec![first, span(&session, 2)];
        let grown = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
        assert_eq!(grown.added_lines, vec![2]);
        assert_eq!(db.get_evidence_span(&first_id).unwrap().unwrap(), denied);
        assert!(
            db.get_search_admitted_evidence_span(&first_id, &workspace)
                .unwrap()
                .is_none()
        );

        let substituted = format!("blake3:{}", blake3::hash(b"other source:1").to_hex());
        db.execute_raw(&format!(
            "UPDATE evidence_spans SET cass_span_id = {} WHERE id = {}",
            sql_text(&substituted),
            sql_text(&first_id)
        ))
        .unwrap();
        let before = db.get_session(&id).unwrap().unwrap();
        let mut newer = incoming;
        newer.push(span(&session, 3));
        let error = refresh_session(&db, &workspace, &id, &session, &newer)
            .err()
            .unwrap();
        assert!(error.to_string().contains("cass_refresh_history_missing"));
        assert!(!error.to_string().contains(&session.source_path));
        assert_eq!(db.get_session(&id).unwrap().unwrap(), before);
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 2);
    }

    fn row_counts(db: &DbConnection) -> [i64; 4] {
        [
            "sessions",
            "evidence_spans",
            "search_index_jobs",
            "audit_log",
        ]
        .map(|table| db.count_table_rows(table).unwrap())
    }

    fn assert_refresh_refused_without_writes(
        db: &DbConnection,
        workspace: &str,
        id: &str,
        session: &CassSessionInfo,
        spans: &[CassViewSpanForImport],
        code: &str,
    ) {
        let before_counts = row_counts(db);
        let before_session = db.get_session(id).unwrap();
        let before_evidence = db.list_evidence_spans_for_session(id).unwrap();
        let error = refresh_session(db, workspace, id, session, spans)
            .err()
            .expect("invalid retained history must refuse refresh")
            .to_string();
        assert!(error.contains(code), "{error}");
        assert!(!error.contains(&session.source_path), "{error}");
        assert!(!error.contains("PRIVATE_PAYLOAD_SENTINEL"), "{error}");
        assert_eq!(row_counts(db), before_counts);
        assert_eq!(db.get_session(id).unwrap(), before_session);
        assert_eq!(
            db.list_evidence_spans_for_session(id).unwrap(),
            before_evidence
        );
    }

    #[test]
    fn migrated_history_can_grow_without_regaining_admission_or_changing_ids() {
        for producer in ["cass_import", "legacy_unknown"] {
            for raw_reference in [false, true] {
                let (db, workspace, id, session) = fixture("/private/migrated.jsonl", 2);
                let first = span(&session, 1);
                let first_id = stable_evidence_id(&id, &first.cass_span_id);
                db.execute_raw(&format!(
                    "UPDATE evidence_spans SET producer_kind = {}, search_eligibility = 'denied', pack_eligibility = 'denied' WHERE id = {}",
                    sql_text(producer),
                    sql_text(&first_id),
                ))
                .unwrap();
                if raw_reference {
                    db.execute_raw(&format!(
                        "UPDATE evidence_spans SET cass_span_id = {} WHERE id = {}",
                        sql_text(&first.cass_span_id),
                        sql_text(&first_id),
                    ))
                    .unwrap();
                }
                let retained = db.list_evidence_spans_for_session(&id).unwrap();
                db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
                    .unwrap();
                let old_job = stable_search_index_job_id(&workspace, &id);
                let mut incoming: Vec<_> = (1..=4).map(|line| span(&session, line)).collect();
                let grown = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
                assert!(grown.changed);
                assert_eq!(grown.added_lines, vec![3, 4]);
                let job = grown.index_job_id.unwrap();
                assert_ne!(job, old_job);
                assert_eq!(
                    db.get_search_index_job(&job)
                        .unwrap()
                        .unwrap()
                        .status_enum(),
                    Some(SearchIndexJobStatus::Pending)
                );
                for row in &retained {
                    assert_eq!(db.get_evidence_span(&row.id).unwrap().as_ref(), Some(row));
                }
                assert!(
                    db.get_search_admitted_evidence_span(&first_id, &workspace)
                        .unwrap()
                        .is_none()
                );
                for fresh in &incoming[2..] {
                    let fresh_id = stable_evidence_id(&id, &fresh.cass_span_id);
                    let row = db
                        .get_search_admitted_evidence_span(&fresh_id, &workspace)
                        .unwrap()
                        .expect("only newly captured evidence receives normal admission");
                    assert_eq!(row.excerpt, fresh.excerpt);
                }
                let before_retry = row_counts(&db);
                let saved = db.get_session(&id).unwrap();
                incoming.reverse();
                let retry = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
                assert!(!retry.changed);
                assert!(retry.added_lines.is_empty());
                assert_eq!(retry.index_job_id.as_deref(), Some(job.as_str()));
                assert_eq!(row_counts(&db), before_retry);
                assert_eq!(db.get_session(&id).unwrap(), saved);
                db.execute_raw("UPDATE search_index_jobs SET status = 'completed'")
                    .unwrap();
                let completed = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
                assert!(!completed.changed);
                assert!(completed.index_job_id.is_none());
                assert_eq!(row_counts(&db), before_retry);
                for row in retained {
                    assert_eq!(db.get_evidence_span(&row.id).unwrap(), Some(row));
                }
            }
        }
    }

    #[test]
    fn migrated_history_cannot_be_truncated_or_rewritten_during_growth() {
        let (db, workspace, id, session) = fixture("/private/migrated-history.jsonl", 2);
        db.execute_raw(
            "UPDATE evidence_spans SET producer_kind = 'legacy_unknown', search_eligibility = 'denied', pack_eligibility = 'denied'",
        )
        .unwrap();
        let mut incoming: Vec<_> = (1..=3).map(|line| span(&session, line)).collect();
        let mut missing = incoming.clone();
        missing.remove(0);
        assert_refresh_refused_without_writes(
            &db,
            &workspace,
            &id,
            &session,
            &missing,
            "cass_refresh_history_missing",
        );
        incoming[0] = super::super::parse_view_line_value(
            &json!({"line": 1, "content": "PRIVATE_PAYLOAD_SENTINEL changed history"}),
            &session.source_path,
        )
        .unwrap();
        assert_refresh_refused_without_writes(
            &db,
            &workspace,
            &id,
            &session,
            &incoming,
            "cass_refresh_history_changed",
        );
    }

    #[test]
    fn other_producer_cannot_be_reimported_under_a_fresh_cass_identity() {
        let (db, workspace, id, session) = fixture("/private/producer-conflict.jsonl", 1);
        let other_id = stable_evidence_id(&id, "other-producer-identity");
        db.execute_raw(&format!(
            "UPDATE evidence_spans SET id = {}, producer_kind = 'journal_distill', search_eligibility = 'denied', pack_eligibility = 'denied'",
            sql_text(&other_id),
        ))
        .unwrap();
        let incoming = [span(&session, 1), span(&session, 2)];
        assert_refresh_refused_without_writes(
            &db,
            &workspace,
            &id,
            &session,
            &incoming,
            "cass_refresh_producer_conflict",
        );
    }

    #[test]
    fn migrated_evidence_keeps_its_historical_identity_during_growth() {
        let (db, workspace, id, session) = fixture("/private/historical-identity.jsonl", 1);
        let original = span(&session, 1);
        let current_id = stable_evidence_id(&id, &original.cass_span_id);
        let historical_id = stable_evidence_id(&id, "historical-evidence-identity");
        db.execute_raw(&format!(
            "UPDATE evidence_spans SET id = {}, producer_kind = 'legacy_unknown', cass_span_id = {}, search_eligibility = 'denied', pack_eligibility = 'denied'",
            sql_text(&historical_id),
            sql_text(&original.cass_span_id),
        ))
        .unwrap();
        let retained = db.get_evidence_span(&historical_id).unwrap().unwrap();
        let incoming = [original, span(&session, 2)];
        let report = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
        assert_eq!(report.added_lines, vec![2]);
        assert_eq!(
            db.get_evidence_span(&historical_id).unwrap(),
            Some(retained)
        );
        assert!(db.get_evidence_span(&current_id).unwrap().is_none());
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 2);
        let again = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
        assert!(!again.changed);
        assert!(again.added_lines.is_empty());
        assert_eq!(again.index_job_id, report.index_job_id);
    }

    #[test]
    fn unrelated_producer_evidence_is_preserved_without_blocking_cass_growth() {
        let (db, workspace, id, session) = fixture("/private/mixed-producers.jsonl", 1);
        let other_id = stable_evidence_id(&id, "journal-entry-identity");
        db.execute_raw(&format!(
            "UPDATE evidence_spans SET id = {}, producer_kind = 'journal_distill', cass_span_id = 'journal:unrelated-entry', search_eligibility = 'denied', pack_eligibility = 'denied'",
            sql_text(&other_id),
        ))
        .unwrap();
        let retained = db.get_evidence_span(&other_id).unwrap().unwrap();
        let incoming = [span(&session, 1), span(&session, 2)];
        let report = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
        assert_eq!(report.added_lines, vec![1, 2]);
        assert_eq!(db.get_evidence_span(&other_id).unwrap(), Some(retained));
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 3);
        let again = refresh_session(&db, &workspace, &id, &session, &incoming).unwrap();
        assert!(!again.changed);
        assert!(again.added_lines.is_empty());
    }

    #[test]
    fn invalid_new_evidence_cannot_poison_a_durable_refresh_checkpoint() {
        let (db, workspace, id, session) = fixture("/private/invalid-refresh.jsonl", 1);
        for malformed in 0..5 {
            let mut incoming = vec![span(&session, 1), span(&session, 2)];
            match malformed {
                0 => incoming[1].start_line = 0,
                1 => incoming[1].end_line = 3,
                2 => incoming[1].cass_span_id = "/private/other-session.jsonl:2".to_owned(),
                3 => incoming[1].content_hash = "not-the-excerpt-digest".to_owned(),
                _ => incoming[1].excerpt = "PRIVATE_PAYLOAD_SENTINEL substituted".to_owned(),
            }
            assert_refresh_refused_without_writes(
                &db,
                &workspace,
                &id,
                &session,
                &incoming,
                "cass_refresh_invalid_span",
            );
        }
    }
}
