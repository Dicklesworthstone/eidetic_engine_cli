//! Explicit CASS source withdrawal. Retention and admission are separate:
//! withdrawal preserves transcript bytes while denying future use as evidence.

use super::{
    CreateAuditInput, CreateSearchIndexJobInput, DbConnection, DbError, DbOperation,
    EvidenceProducerKind, SearchIndexJobType, StoredEvidenceSpan, Value, audit_actions,
    generate_audit_id, stored_evidence_security_state_hash,
};

#[path = "evidence_session_withdrawal.rs"]
mod session;

/// Metadata from the exact source snapshot used for a withdrawal decision.
#[derive(Debug)]
pub(crate) struct CassEvidenceWithdrawal {
    pub evidence_id: String,
    pub session_id: String,
    pub previous_search_eligibility: String,
    pub previous_pack_eligibility: String,
    pub would_change: bool,
    pub persisted: bool,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
}

#[derive(Debug)]
pub(crate) enum CassEvidenceWithdrawalError {
    Database(DbError),
    NotFound,
    UnsupportedProducer,
    InvalidMetadata,
    InvalidReason,
}

impl From<DbError> for CassEvidenceWithdrawalError {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

/// Validate the original security envelope before preparing either kind of
/// withdrawal. Never repair malformed authority metadata by overwriting it.
fn withdrawal_metadata(span: &StoredEvidenceSpan) -> Result<String, CassEvidenceWithdrawalError> {
    if EvidenceProducerKind::parse(&span.producer_kind) != Some(EvidenceProducerKind::CassImport) {
        return Err(CassEvidenceWithdrawalError::UnsupportedProducer);
    }
    let classes: Vec<String> = serde_json::from_str(&span.redaction_classes_json)
        .map_err(|_| CassEvidenceWithdrawalError::InvalidMetadata)?;
    if !span.security_metadata_matches(EvidenceProducerKind::CassImport, &classes) {
        return Err(CassEvidenceWithdrawalError::InvalidMetadata);
    }
    let mut metadata: serde_json::Value = serde_json::from_str(
        span.metadata_json
            .as_deref()
            .ok_or(CassEvidenceWithdrawalError::InvalidMetadata)?,
    )
    .map_err(|_| CassEvidenceWithdrawalError::InvalidMetadata)?;
    let object = metadata
        .as_object_mut()
        .ok_or(CassEvidenceWithdrawalError::InvalidMetadata)?;
    object.insert("searchEligibility".to_owned(), "denied".into());
    object.insert("packEligibility".to_owned(), "denied".into());
    Ok(metadata.to_string())
}

impl DbConnection {
    /// Decide and optionally withdraw one source in the caller's snapshot.
    ///
    /// Applying callers MUST own `with_transaction`/`with_transaction_error`;
    /// previews use an enforced read-only connection and a read snapshot.
    /// Workspace binding, this source read, its denial, the audit and the
    /// indexing job therefore all describe one transaction. There is no
    /// re-admission operation: import refresh must preserve the stored denial.
    pub(crate) fn withdraw_cass_evidence_in_current_snapshot(
        &self,
        workspace_id: &str,
        evidence_id: &str,
        reason: &str,
        actor: Option<&str>,
        dry_run: bool,
    ) -> Result<CassEvidenceWithdrawal, CassEvidenceWithdrawalError> {
        if reason.trim().is_empty() || reason.len() > 4096 {
            return Err(CassEvidenceWithdrawalError::InvalidReason);
        }
        let mut joined = self.get_evidence_spans_with_sessions(&[evidence_id])?;
        let Some(joined) = joined.pop() else {
            return Err(CassEvidenceWithdrawalError::NotFound);
        };
        let span = joined.span;
        let Some(session) = joined.session else {
            return Err(CassEvidenceWithdrawalError::NotFound);
        };
        if span.id != evidence_id || !span.is_bound_to_session(workspace_id, &session) {
            return Err(CassEvidenceWithdrawalError::NotFound);
        }
        // Other producers intentionally store `denied` while still supporting
        // explicit curation. Reusing that value cannot revoke their authority.
        let metadata_json = withdrawal_metadata(&span)?;
        let would_change = span.search_eligibility != "denied" || span.pack_eligibility != "denied";
        let mut outcome = CassEvidenceWithdrawal {
            evidence_id: span.id.clone(),
            session_id: span.session_id.clone(),
            previous_search_eligibility: span.search_eligibility.clone(),
            previous_pack_eligibility: span.pack_eligibility.clone(),
            would_change,
            persisted: false,
            audit_id: None,
            index_job_id: None,
        };
        if dry_run || !would_change {
            return Ok(outcome);
        }

        let before_hash = stored_evidence_security_state_hash(&span);
        let withdrawn = self.deny_cass_source(
            span,
            metadata_json,
            &chrono::Utc::now().to_rfc3339(),
        )?;
        let index_job_id = self.enqueue_withdrawn_session(workspace_id, &withdrawn.session_id)?;
        let audit_id = generate_audit_id();
        self.insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: actor.map(|value| crate::policy::redact_public_replay_text(value).content),
                action: audit_actions::EVIDENCE_WITHDRAW.to_owned(),
                target_type: Some("evidence_span".to_owned()),
                target_id: Some(withdrawn.id.clone()),
                details: Some(
                    serde_json::json!({
                        "schema": "ee.audit.evidence_withdraw.v1",
                        "reason": crate::policy::redact_public_replay_text(reason.trim()).content,
                        "sessionId": withdrawn.session_id,
                        "producerKind": EvidenceProducerKind::CassImport.as_str(),
                        "previousSearchEligibility": outcome.previous_search_eligibility,
                        "previousPackEligibility": outcome.previous_pack_eligibility,
                        "searchEligibility": "denied",
                        "packEligibility": "denied",
                        "beforeStateHash": before_hash,
                        "afterStateHash": stored_evidence_security_state_hash(&withdrawn),
                        "indexJobId": index_job_id,
                        "indexStatus": "queued",
                        "sourceBytesRetained": true,
                    })
                    .to_string(),
                ),
            },
        )?;
        outcome.persisted = true;
        outcome.audit_id = Some(audit_id);
        outcome.index_job_id = Some(index_job_id);
        Ok(outcome)
    }

    /// Shared mutation boundary. Callers already validated the owner and
    /// security envelope and own the transaction containing the receipts.
    fn deny_cass_source(
        &self,
        mut span: StoredEvidenceSpan,
        metadata_json: String,
        now: &str,
    ) -> Result<StoredEvidenceSpan, CassEvidenceWithdrawalError> {
        let affected = self.execute_for(
            DbOperation::Execute,
            "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied', metadata_json = ?1, updated_at = ?2 WHERE id = ?3 AND workspace_id = ?4 AND session_id = ?5 AND producer_kind = 'cass_import' AND search_eligibility = ?6 AND pack_eligibility = ?7",
            &[
                Value::Text(metadata_json.clone()),
                Value::Text(now.to_owned()),
                Value::Text(span.id.clone()),
                Value::Text(span.workspace_id.clone()),
                Value::Text(span.session_id.clone()),
                Value::Text(span.search_eligibility.clone()),
                Value::Text(span.pack_eligibility.clone()),
            ],
        )?;
        if affected != 1 {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Execute,
                message: "Evidence withdrawal did not update its selected source".to_owned(),
            }
            .into());
        }
        span.search_eligibility = "denied".to_owned();
        span.pack_eligibility = "denied".to_owned();
        span.metadata_json = Some(metadata_json);
        span.updated_at = now.to_owned();
        Ok(span)
    }

    fn enqueue_withdrawn_session(
        &self,
        workspace_id: &str,
        session_id: &str,
    ) -> Result<String, CassEvidenceWithdrawalError> {
        // One session job reconciles all of its excluded spans. Live admission
        // denies them immediately; this receipt does not claim publication.
        let id = format!(
            "sidx_{}",
            crate::models::MemoryId::now()
                .to_string()
                .trim_start_matches("mem_")
        );
        self.insert_search_index_job(
            &id,
            &CreateSearchIndexJobInput {
                workspace_id: workspace_id.to_owned(),
                job_type: SearchIndexJobType::Incremental,
                document_source: Some("session".to_owned()),
                document_id: Some(session_id.to_owned()),
                documents_total: 1,
            },
        )?;
        Ok(id)
    }
}
