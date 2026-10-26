//! Bulk withdrawal of the retained CASS sources in an addressed session.

use std::path::Path;

use serde::Serialize;

use crate::models::{DomainError, SessionId};
use super::{validate_reason, with_withdrawal_store, withdrawal_error};

pub const EVIDENCE_SESSION_WITHDRAW_SCHEMA_V1: &str = "ee.evidence.withdraw_session.v1";

#[derive(Clone, Debug)]
pub struct EvidenceSessionWithdrawOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub session_id: &'a str,
    pub reason: &'a str,
    pub actor: Option<&'a str>,
    pub dry_run: bool,
}

/// Counts and receipts only: no transcript, reason or raw source locator.
/// `withdrawn_count` describes this commit; `would_withdraw_count` also
/// describes previews. This operation does not disable future imports.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSessionWithdrawReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub session_id: String,
    pub scope: &'static str,
    pub future_imports: &'static str,
    pub status: &'static str,
    pub dry_run: bool,
    pub persisted: bool,
    pub changed: bool,
    pub would_change: bool,
    pub retained_evidence_count: usize,
    pub cass_evidence_count: usize,
    pub excluded_producer_count: usize,
    pub already_withdrawn_count: usize,
    pub would_withdraw_count: usize,
    pub withdrawn_count: usize,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
    pub index_status: &'static str,
}

/// Withdraw every currently retained CASS source in one session atomically.
/// Other producers, original bytes, historical packs and independently curated
/// memory remain unchanged. Existing source denials survive refresh; new
/// evidence imported later is not denied by this operation.
pub fn withdraw_cass_session_evidence(
    options: &EvidenceSessionWithdrawOptions<'_>,
) -> Result<EvidenceSessionWithdrawReport, DomainError> {
    validate_reason(options.reason)?;
    if !options.session_id.parse::<SessionId>()
        .is_ok_and(|id| id.to_string() == options.session_id)
    {
        return Err(DomainError::Usage {
            message: "Session withdrawal requires a canonical sess_ session ID.".to_owned(),
            repair: Some("Use the session ID from an imported evidence source.".to_owned()),
        });
    }
    with_withdrawal_store(
        options.workspace_path,
        options.database_path,
        options.dry_run,
        |connection, workspace_id| {
            let decision = connection.withdraw_cass_session_in_current_snapshot(
                workspace_id, options.session_id, options.reason, options.actor, options.dry_run,
            ).map_err(|error| withdrawal_error(error, "session", options.session_id))?;
            let would_change = decision.would_withdraw_count != 0;
            Ok(EvidenceSessionWithdrawReport {
                schema: EVIDENCE_SESSION_WITHDRAW_SCHEMA_V1,
                command: "evidence withdraw",
                version: env!("CARGO_PKG_VERSION"),
                workspace_id: workspace_id.to_owned(),
                session_id: decision.session_id,
                scope: "retained_sources",
                future_imports: "unchanged",
                status: if decision.cass_evidence_count == 0 {
                    "no_cass_sources"
                } else if !would_change {
                    "already_withdrawn"
                } else if options.dry_run {
                    "would_withdraw"
                } else {
                    "withdrawn"
                },
                dry_run: options.dry_run,
                persisted: decision.persisted,
                changed: decision.persisted,
                would_change,
                retained_evidence_count: decision.retained_evidence_count,
                cass_evidence_count: decision.cass_evidence_count,
                excluded_producer_count: decision.excluded_producer_count,
                already_withdrawn_count: decision.already_withdrawn_count,
                would_withdraw_count: decision.would_withdraw_count,
                withdrawn_count: if decision.persisted {
                    decision.would_withdraw_count
                } else {
                    0
                },
                audit_id: decision.audit_id,
                index_job_id: decision.index_job_id,
                index_status: if decision.persisted {
                    "queued"
                } else if options.dry_run && would_change {
                    "dry_run_not_queued"
                } else {
                    "not_scheduled"
                },
            })
        },
    )
}

#[cfg(test)]
#[path = "evidence_session_tests.rs"]
mod tests;
