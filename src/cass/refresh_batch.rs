//! Bounded writes for the growing tail of an already imported CASS session.
//!
//! The refresh owner has already checked the complete source history and owns
//! the writer-fenced transaction. A failure here must propagate to that owner:
//! evidence, admission verdicts, audits, checkpoint and publication job either
//! all commit, or none of them do.

use crate::db::{DbConnection, DbError, EvidenceAdmissionTally, StoredSession};

use super::{
    CassViewSpanForImport, cass_redaction_audit_input, evidence_input, refusal,
    stable_cass_redaction_audit_id, stable_evidence_id,
};

// Bound temporary copies independently of the complete retained transcript.
// The database writer further splits each batch at its SQL parameter limit.
const REFRESH_WRITE_BATCH_ROWS: usize = 128;

pub(super) fn insert_additions(
    connection: &DbConnection,
    workspace_id: &str,
    session: &StoredSession,
    additions: &[&CassViewSpanForImport],
) -> Result<(Vec<u32>, EvidenceAdmissionTally), DbError> {
    let mut added_lines = Vec::with_capacity(additions.len());
    let mut admission = EvidenceAdmissionTally::default();
    for batch in additions.chunks(REFRESH_WRITE_BATCH_ROWS) {
        let rows = batch
            .iter()
            .map(|span| {
                (
                    stable_evidence_id(&session.id, &span.cass_span_id),
                    evidence_input(workspace_id, &session.id, span),
                )
            })
            .collect::<Vec<_>>();
        // Preserve the refresh-specific identity refusal. An occupied identity
        // anywhere in the database cannot be overwritten or re-admitted.
        for (id, _) in &rows {
            if connection.get_evidence_span(id)?.is_some() {
                return Err(refusal("cass_refresh_evidence_identity_exists"));
            }
        }
        admission.merge(&connection.insert_evidence_spans_in_session(&rows, session)?);
        for (span, (evidence_id, _)) in batch.iter().zip(&rows) {
            if span.redacted {
                connection.insert_audit(
                    &stable_cass_redaction_audit_id(evidence_id),
                    &cass_redaction_audit_input(workspace_id, &session.id, evidence_id, span),
                )?;
            }
            added_lines.push(span.end_line);
        }
    }
    Ok((added_lines, admission))
}

#[cfg(test)]
#[path = "refresh_batch_regressions.rs"]
mod batch_regressions;
