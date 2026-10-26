//! Operator control of imported evidence admission without erasing history.

use std::path::Path;

use serde::Serialize;

use crate::db::{CassEvidenceWithdrawal, CassEvidenceWithdrawalError, DbConnection, DbError};
use crate::models::{DomainError, EvidenceId};

#[path = "evidence_session.rs"]
mod session;
pub use session::{
    EVIDENCE_SESSION_WITHDRAW_SCHEMA_V1, EvidenceSessionWithdrawOptions,
    EvidenceSessionWithdrawReport, withdraw_cass_session_evidence,
};

pub const EVIDENCE_WITHDRAW_SCHEMA_V1: &str = "ee.evidence.withdraw.v1";

#[derive(Clone, Debug)]
pub struct EvidenceWithdrawOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub evidence_id: &'a str,
    pub reason: &'a str,
    pub actor: Option<&'a str>,
    pub dry_run: bool,
}

/// Metadata only. Eligibility describes the requested final posture; the
/// separate mutation flags distinguish a preview, a commit and a repeat.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceWithdrawReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub evidence_id: String,
    pub session_id: String,
    pub status: &'static str,
    pub dry_run: bool,
    pub persisted: bool,
    pub changed: bool,
    pub would_change: bool,
    pub previous_search_eligibility: String,
    pub previous_pack_eligibility: String,
    pub search_eligibility: String,
    pub pack_eligibility: String,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
    pub index_status: &'static str,
}

impl EvidenceWithdrawReport {
    fn from_decision(
        workspace_id: String,
        decision: CassEvidenceWithdrawal,
        dry_run: bool,
    ) -> Self {
        Self {
            schema: EVIDENCE_WITHDRAW_SCHEMA_V1,
            command: "evidence withdraw",
            version: env!("CARGO_PKG_VERSION"),
            workspace_id,
            evidence_id: decision.evidence_id,
            session_id: decision.session_id,
            status: if !decision.would_change {
                "already_withdrawn"
            } else if dry_run {
                "would_withdraw"
            } else {
                "withdrawn"
            },
            dry_run,
            persisted: decision.persisted,
            changed: decision.persisted,
            would_change: decision.would_change,
            previous_search_eligibility: decision.previous_search_eligibility,
            previous_pack_eligibility: decision.previous_pack_eligibility,
            search_eligibility: "denied".to_owned(),
            pack_eligibility: "denied".to_owned(),
            audit_id: decision.audit_id,
            index_job_id: decision.index_job_id,
            index_status: if decision.persisted {
                "queued"
            } else if dry_run && decision.would_change {
                "dry_run_not_queued"
            } else {
                "not_scheduled"
            },
        }
    }
}

fn storage_error(error: DbError) -> DomainError {
    super::db_failure_or_storage_panic(
        &error,
        DomainError::Storage {
            message: format!("Failed to withdraw imported evidence: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        },
    )
}

struct WithdrawalTransactionError(DomainError);

impl From<DbError> for WithdrawalTransactionError {
    fn from(error: DbError) -> Self {
        Self(storage_error(error))
    }
}

fn validate_reason(reason: &str) -> Result<(), DomainError> {
    if reason.trim().is_empty() || reason.len() > 4096 {
        return Err(DomainError::Usage {
            message: "Evidence withdrawal requires a nonempty reason of at most 4096 bytes."
                .to_owned(),
            repair: Some("Pass --reason with the reason for withdrawing this source.".to_owned()),
        });
    }
    Ok(())
}

fn withdrawal_error(error: CassEvidenceWithdrawalError, resource: &str, id: &str) -> DomainError {
    match error {
        CassEvidenceWithdrawalError::Database(error) => storage_error(error),
        CassEvidenceWithdrawalError::NotFound => DomainError::NotFound {
            resource: resource.to_owned(),
            id: id.to_owned(),
            repair: Some("Address the workspace that owns this source ID.".to_owned()),
        },
        CassEvidenceWithdrawalError::UnsupportedProducer => DomainError::Usage {
            message: "Evidence withdrawal currently supports CASS-imported sources only."
                .to_owned(),
            repair: None,
        },
        CassEvidenceWithdrawalError::InvalidMetadata => DomainError::Storage {
            message: "The evidence security metadata is inconsistent; withdrawal was not applied."
                .to_owned(),
            repair: Some("ee doctor --json".to_owned()),
        },
        CassEvidenceWithdrawalError::InvalidReason => DomainError::Usage {
            message: "Evidence withdrawal requires a nonempty reason of at most 4096 bytes."
                .to_owned(),
            repair: None,
        },
    }
}

/// Deny one CASS evidence source for future search, packs and derivation.
///
/// The source text, historical packs and independently curated memories/rules
/// are retained. Current-source incident-card proof propagates the denial to
/// dependent summaries. Only an indexing job is scheduled here; publication
/// follows the existing index worker lifecycle.
pub fn withdraw_cass_evidence(
    options: &EvidenceWithdrawOptions<'_>,
) -> Result<EvidenceWithdrawReport, DomainError> {
    validate_reason(options.reason)?;
    if !options
        .evidence_id
        .parse::<EvidenceId>()
        .is_ok_and(|id| id.to_string() == options.evidence_id)
    {
        return Err(DomainError::Usage {
            message: "Evidence withdrawal requires a canonical ev_ evidence ID.".to_owned(),
            repair: Some("Use the evidence ID from a search result or source citation.".to_owned()),
        });
    }
    with_withdrawal_store(
        options.workspace_path,
        options.database_path,
        options.dry_run,
        |connection, workspace_id| {
            let decision = connection
                .withdraw_cass_evidence_in_current_snapshot(
                    workspace_id,
                    options.evidence_id,
                    options.reason,
                    options.actor,
                    options.dry_run,
                )
                .map_err(|error| withdrawal_error(error, "evidence", options.evidence_id))?;
            Ok(EvidenceWithdrawReport::from_decision(
                workspace_id.to_owned(),
                decision,
                options.dry_run,
            ))
        },
    )
}

/// Share the addressed-store, migration, owner and transaction boundaries
/// across single-source and session-wide operations. Previews never create,
/// migrate or open a writable store. Receipts are returned only after commit.
fn with_withdrawal_store<T>(
    workspace_path: &Path,
    database_path: Option<&Path>,
    dry_run: bool,
    apply: impl Fn(&DbConnection, &str) -> Result<T, DomainError>,
) -> Result<T, DomainError> {
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee/ee.db"));
    super::ensure_addressed_database_exists(&database_path)?;
    let connection = if dry_run {
        DbConnection::open_file_read_only(&database_path)
    } else {
        DbConnection::open_file(&database_path)
    }
    .map_err(storage_error)?;

    let decide = || {
        match connection.needs_migration() {
            Ok(false) => {}
            Ok(true) => {
                return Err(DomainError::MigrationRequired {
                    message: "Migrate the existing database before withdrawing evidence. The store was not changed.".to_owned(),
                    repair: Some(format!(
                        "ee migrate run --workspace {} --database {} --json",
                        super::shell_quote_repair_arg(&workspace_path.to_string_lossy()),
                        super::shell_quote_repair_arg(&database_path.to_string_lossy()),
                    )),
                });
            }
            Err(error @ DbError::MigrationDrift { .. }) => {
                return Err(DomainError::MigrationDrift {
                    message: format!("Failed to inspect evidence database schema: {error}"),
                    repair: Some("ee migrate status --json".to_owned()),
                });
            }
            Err(error) => return Err(storage_error(error)),
        }
        let workspace = super::workspace::addressed_workspace_row(
            &connection,
            workspace_path,
            &database_path,
        )?
        .ok_or_else(|| DomainError::NotFound {
            resource: "workspace".to_owned(),
            id: workspace_path.display().to_string(),
            repair: Some("Address the workspace that owns the imported evidence.".to_owned()),
        })?;
        apply(&connection, &workspace.id)
    };

    if dry_run {
        connection.begin_read_snapshot().map_err(storage_error)?;
        match decide() {
            Ok(report) => {
                connection.commit_read_snapshot().map_err(storage_error)?;
                Ok(report)
            }
            Err(error) => {
                let _ = connection.rollback_read_snapshot();
                Err(error)
            }
        }
    } else {
        connection
            .with_transaction_error(|| decide().map_err(WithdrawalTransactionError))
            .map_err(|error: WithdrawalTransactionError| error.0)
    }
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
