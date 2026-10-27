//! Withdraw the CASS rows retained in one session snapshot, all or nothing.
//! This is a bulk source operation, not a policy disabling future imports.

use super::{
    CassEvidenceWithdrawalError, CreateAuditInput, DbConnection, EvidenceProducerKind,
    StoredEvidenceSpan, audit_actions, generate_audit_id, stored_evidence_security_state_hash,
    withdrawal_metadata,
};

#[derive(Debug)]
pub(crate) struct CassSessionWithdrawal {
    pub session_id: String,
    pub retained_evidence_count: usize,
    pub cass_evidence_count: usize,
    pub excluded_producer_count: usize,
    pub already_withdrawn_count: usize,
    pub would_withdraw_count: usize,
    pub persisted: bool,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
}

fn state_hasher() -> blake3::Hasher {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.evidence.withdraw_session.state.v1\0");
    hash
}

fn append_state(
    hash: &mut blake3::Hasher,
    span: &StoredEvidenceSpan,
) -> Result<(), CassEvidenceWithdrawalError> {
    let state = serde_json::to_vec(&stored_evidence_security_state_hash(span))
        .map_err(|_| CassEvidenceWithdrawalError::InvalidMetadata)?;
    for value in [span.id.as_bytes(), state.as_slice()] {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    Ok(())
}

impl DbConnection {
    /// Select, validate and deny the retained CASS sources of this session.
    /// The caller owns a write transaction, or an enforced read-only snapshot
    /// for preview. No nested transaction, per-source audit or per-source job
    /// is introduced. A single failure rolls back every source and receipt.
    pub(crate) fn withdraw_cass_session_in_current_snapshot(
        &self,
        workspace_id: &str,
        session_id: &str,
        reason: &str,
        actor: Option<&str>,
        dry_run: bool,
    ) -> Result<CassSessionWithdrawal, CassEvidenceWithdrawalError> {
        if reason.trim().is_empty() || reason.len() > 4096 {
            return Err(CassEvidenceWithdrawalError::InvalidReason);
        }
        let session = self
            .get_session(session_id)?
            .filter(|session| session.id == session_id && session.workspace_id == workspace_id)
            .ok_or(CassEvidenceWithdrawalError::NotFound)?;
        let mut spans = self.list_evidence_spans_for_session(session_id)?;
        spans.sort_by(|left, right| left.id.cmp(&right.id));
        let mut outcome = CassSessionWithdrawal {
            session_id: session_id.to_owned(),
            retained_evidence_count: spans.len(),
            cass_evidence_count: 0,
            excluded_producer_count: 0,
            already_withdrawn_count: 0,
            would_withdraw_count: 0,
            persisted: false,
            audit_id: None,
            index_job_id: None,
        };
        let mut prepared = Vec::new();
        // Validate the entire selection before writing its first row. A bad
        // final source must not leave a partly withdrawn session even if the
        // database reports that error before the receipt-writing stage.
        for span in spans {
            if !span.is_bound_to_session(workspace_id, &session) {
                return Err(CassEvidenceWithdrawalError::InvalidMetadata);
            }
            match EvidenceProducerKind::parse(&span.producer_kind) {
                Some(EvidenceProducerKind::CassImport) => {}
                Some(_) => {
                    outcome.excluded_producer_count += 1;
                    continue;
                }
                None => return Err(CassEvidenceWithdrawalError::InvalidMetadata),
            }
            let metadata = withdrawal_metadata(&span)?;
            outcome.cass_evidence_count += 1;
            if span.search_eligibility == "denied" && span.pack_eligibility == "denied" {
                outcome.already_withdrawn_count += 1;
            } else {
                outcome.would_withdraw_count += 1;
            }
            prepared.push((span, metadata));
        }
        if dry_run || outcome.would_withdraw_count == 0 {
            return Ok(outcome);
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut before = state_hasher();
        let mut after = state_hasher();
        for (span, metadata) in prepared {
            append_state(&mut before, &span)?;
            let span = if span.search_eligibility == "denied" && span.pack_eligibility == "denied" {
                span
            } else {
                self.deny_cass_source(span, metadata, &now)?
            };
            append_state(&mut after, &span)?;
        }
        let index_job_id = self.enqueue_withdrawn_session(workspace_id, session_id)?;
        let audit_id = generate_audit_id();
        self.insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: actor.map(|value| crate::policy::redact_public_replay_text(value).content),
                action: audit_actions::EVIDENCE_WITHDRAW.to_owned(),
                target_type: Some("session".to_owned()),
                target_id: Some(session_id.to_owned()),
                details: Some(
                    serde_json::json!({
                        "schema": "ee.audit.evidence_withdraw_session.v1",
                        "scope": "retained_sources",
                        "futureImports": "unchanged",
                        "reason": crate::policy::redact_public_replay_text(reason.trim()).content,
                        "sessionId": session_id,
                        "retainedEvidenceCount": outcome.retained_evidence_count,
                        "cassEvidenceCount": outcome.cass_evidence_count,
                        "excludedProducerCount": outcome.excluded_producer_count,
                        "alreadyWithdrawnCount": outcome.already_withdrawn_count,
                        "withdrawnCount": outcome.would_withdraw_count,
                        "beforeStateHash": format!("blake3:{}", before.finalize().to_hex()),
                        "afterStateHash": format!("blake3:{}", after.finalize().to_hex()),
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
}

#[cfg(test)]
#[path = "evidence_session_withdrawal_tests.rs"]
mod tests;
