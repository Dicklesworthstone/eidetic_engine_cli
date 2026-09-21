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

        // Use upstream references only inside the private import transaction.
        // Never select a version by input order, or let a duplicate reference
        // silently replace one candidate in the map.
        let mut incoming = BTreeMap::new();
        let mut lines = BTreeSet::new();
        for span in spans {
            if incoming.insert(span.cass_span_id.as_str(), span).is_some()
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
            if row.producer_kind != "cass_import" {
                // Other producers may attach their own evidence to a session.
                // An upstream transcript cannot replace or remove those rows.
                continue;
            }
            let Some(span) = incoming.get(row.cass_span_id.as_str()) else {
                return Err(refusal("cass_refresh_history_missing"));
            };
            if !retained.insert(row.cass_span_id.as_str())
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
