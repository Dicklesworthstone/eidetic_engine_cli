//! Ingest and query helpers for the RCH verifier evidence ledger (bd-17awb).
//!
//! Parses `ee.rch.verify.v1` proof JSON produced by `scripts/rch_verify.sh` (or
//! equivalent external tooling) into a `NormalizedRchVerifyRow` matching the
//! `rch_verify_runs` schema landed under V061 by bd-22p8c. Tails are bounded,
//! hashes are stripped of source-specific prefixes, and the canonical
//! 64-character hex constraint is enforced before any database write.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::path::Path;

use chrono::Utc;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::db::{
    DbConnection, DbError, RchVerifyIngestOutcome, StoredRchVerifyRun, rch_verify_run_id,
};

pub const RCH_VERIFY_LEDGER_SCHEMA_V1: &str = "ee.rch.verify.v1";
pub const RCH_VERIFY_LEDGER_INGEST_REPORT_SCHEMA_V1: &str = "ee.rch.verify.ingest.v1";
pub const RCH_VERIFY_LEDGER_RUNS_REPORT_SCHEMA_V1: &str = "ee.rch.verify.runs.v1";
pub const RCH_VERIFY_LEDGER_BLOCKERS_REPORT_SCHEMA_V1: &str = "ee.rch.verify.blockers.v1";
pub const RCH_VERIFY_LEDGER_STATUS_SCHEMA_V1: &str = "ee.rch.verify.ledger_status.v1";
pub const RCH_VERIFY_LEDGER_TAIL_MAX_BYTES: usize = 8192;
pub const RCH_VERIFY_LEDGER_COMMAND_TEXT_MAX_BYTES: usize = 4096;
pub const RCH_VERIFY_LEDGER_DEGRADED_JSON_MAX_BYTES: usize = 4096;
pub const RCH_VERIFY_LEDGER_STATUS_MAX_BLOCKER_REFS: usize = 8;

/// Allowed `status` values for the `rch_verify_runs.status` column.
pub const RCH_VERIFY_LEDGER_STATUSES: &[&str] = &[
    "passed",
    "failed",
    "blocked",
    "interrupted",
    "fallback_detected",
    "unknown",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedRchVerifyRow {
    pub schema_id: String,
    pub command_text: Option<String>,
    pub command_hash: String,
    pub command_kind: String,
    pub bead_id: Option<String>,
    pub git_head: Option<String>,
    pub git_tree: Option<String>,
    pub source_state_hash: String,
    pub dirty_status_hash: Option<String>,
    pub verification_attribution: String,
    pub remote_required: bool,
    pub worker_id: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub degraded_codes: Vec<String>,
    pub degraded_codes_json: Option<String>,
    pub stdout_tail_hash: Option<String>,
    pub stderr_tail_hash: Option<String>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub blocker_fingerprint: Option<String>,
    pub remediation_bead: Option<String>,
    pub retry_after: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyRunView {
    pub id: String,
    pub workspace_id: String,
    #[serde(rename = "schema")]
    pub schema_id: String,
    pub command_text: Option<String>,
    pub command_hash: String,
    pub command_kind: String,
    pub bead_id: Option<String>,
    pub git_head: Option<String>,
    pub git_tree: Option<String>,
    pub source_state_hash: String,
    pub dirty_status_hash: Option<String>,
    pub verification_attribution: String,
    pub remote_required: bool,
    pub worker_id: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub degraded_codes: Vec<String>,
    pub stdout_tail_hash: Option<String>,
    pub stderr_tail_hash: Option<String>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub blocker_fingerprint: Option<String>,
    pub remediation_bead: Option<String>,
    pub retry_after: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyIngestReport {
    pub schema: &'static str,
    pub outcome: &'static str,
    pub inserted_count: u64,
    pub duplicate_count: u64,
    pub run: RchVerifyRunView,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyRunsReport {
    pub schema: &'static str,
    pub runs: Vec<RchVerifyRunView>,
    pub run_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyBlockersReport {
    pub schema: &'static str,
    pub blockers: Vec<RchVerifyRunView>,
    pub blocker_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyLedgerStatusReport {
    pub schema: &'static str,
    pub status: &'static str,
    pub ledger_available: bool,
    pub active_blocker_count: usize,
    pub local_fallback_refused: bool,
    pub local_fallback_refused_count: usize,
    pub oldest_retry_after: Option<String>,
    pub newest_retry_after: Option<String>,
    pub blocker_refs: Vec<RchVerifyLedgerBlockerRef>,
    pub recovery_actions: Vec<RchVerifyLedgerRecoveryAction>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyLedgerBlockerRef {
    pub command_hash: String,
    pub status: String,
    pub blocker_fingerprint: String,
    pub remediation_bead: Option<String>,
    pub retry_after: Option<String>,
    pub degraded_codes: Vec<String>,
    pub verification_attribution: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyLedgerRecoveryAction {
    pub priority: u8,
    pub kind: &'static str,
    pub command: &'static str,
    pub message: &'static str,
}

impl RchVerifyLedgerStatusReport {
    #[must_use]
    pub fn not_inspected() -> Self {
        Self::unavailable(
            "not_inspected",
            "ee status --workspace . --json",
            "Run status or doctor from an initialized workspace to inspect RCH verifier blockers.",
        )
    }

    #[must_use]
    pub fn not_initialized() -> Self {
        Self::unavailable(
            "not_initialized",
            "ee init --workspace .",
            "Initialize the workspace before relying on durable RCH verifier blockers.",
        )
    }

    #[must_use]
    pub fn unavailable(status: &'static str, command: &'static str, message: &'static str) -> Self {
        Self {
            schema: RCH_VERIFY_LEDGER_STATUS_SCHEMA_V1,
            status,
            ledger_available: false,
            active_blocker_count: 0,
            local_fallback_refused: false,
            local_fallback_refused_count: 0,
            oldest_retry_after: None,
            newest_retry_after: None,
            blocker_refs: Vec::new(),
            recovery_actions: vec![RchVerifyLedgerRecoveryAction {
                priority: 1,
                kind: "inspect_verification_ledger",
                command,
                message,
            }],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RchVerifyLedgerParseError {
    NotAnObject,
    MissingSchema,
    UnexpectedSchema { found: String },
    MissingCommandText,
    MissingCommandKind,
    MissingSourceState,
    MissingVerificationAttribution,
    CommandTextTooLong { bytes: usize, max: usize },
}

impl fmt::Display for RchVerifyLedgerParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "rch verify proof must be a JSON object"),
            Self::MissingSchema => write!(f, "rch verify proof is missing schema"),
            Self::UnexpectedSchema { found } => write!(
                f,
                "expected schema {RCH_VERIFY_LEDGER_SCHEMA_V1}, found {found}"
            ),
            Self::MissingCommandText => {
                write!(f, "rch verify proof is missing command_text")
            }
            Self::MissingCommandKind => {
                write!(f, "rch verify proof is missing command_kind")
            }
            Self::MissingSourceState => {
                write!(f, "rch verify proof is missing source_state object")
            }
            Self::MissingVerificationAttribution => write!(
                f,
                "rch verify proof source_state is missing verification_attribution"
            ),
            Self::CommandTextTooLong { bytes, max } => write!(
                f,
                "command_text is {bytes} bytes; schema bounds it to {max}"
            ),
        }
    }
}

impl Error for RchVerifyLedgerParseError {}

#[derive(Debug)]
pub enum RchVerifyLedgerError {
    Parse(RchVerifyLedgerParseError),
    Storage(DbError),
}

impl fmt::Display for RchVerifyLedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(source) => write!(f, "{source}"),
            Self::Storage(source) => write!(f, "{source}"),
        }
    }
}

impl Error for RchVerifyLedgerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(source) => Some(source),
            Self::Storage(source) => Some(source),
        }
    }
}

impl From<RchVerifyLedgerParseError> for RchVerifyLedgerError {
    fn from(source: RchVerifyLedgerParseError) -> Self {
        Self::Parse(source)
    }
}

impl From<DbError> for RchVerifyLedgerError {
    fn from(source: DbError) -> Self {
        Self::Storage(source)
    }
}

/// Parse an `ee.rch.verify.v1` proof JSON object into a normalized row ready
/// for insertion into `rch_verify_runs`. Computes deterministic hashes,
/// classifies the run status, bounds tail payloads, and extracts known
/// blocker fingerprint/retry guidance.
pub fn parse_rch_verify_v1(
    value: &JsonValue,
) -> Result<NormalizedRchVerifyRow, RchVerifyLedgerParseError> {
    if !value.is_object() {
        return Err(RchVerifyLedgerParseError::NotAnObject);
    }

    let schema = string_field(value, "schema").ok_or(RchVerifyLedgerParseError::MissingSchema)?;
    if schema != RCH_VERIFY_LEDGER_SCHEMA_V1 {
        return Err(RchVerifyLedgerParseError::UnexpectedSchema { found: schema });
    }

    let command_text =
        string_field(value, "command_text").ok_or(RchVerifyLedgerParseError::MissingCommandText)?;
    if command_text.len() > RCH_VERIFY_LEDGER_COMMAND_TEXT_MAX_BYTES {
        return Err(RchVerifyLedgerParseError::CommandTextTooLong {
            bytes: command_text.len(),
            max: RCH_VERIFY_LEDGER_COMMAND_TEXT_MAX_BYTES,
        });
    }
    let command_kind =
        string_field(value, "command_kind").ok_or(RchVerifyLedgerParseError::MissingCommandKind)?;

    let source_state = value
        .get("source_state")
        .and_then(JsonValue::as_object)
        .ok_or(RchVerifyLedgerParseError::MissingSourceState)?;
    let verification_attribution = source_state
        .get("verification_attribution")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(RchVerifyLedgerParseError::MissingVerificationAttribution)?;
    let git_head =
        string_from_object(source_state, "git_head").and_then(|raw| sanitize_git_oid(&raw));
    let git_tree =
        string_from_object(source_state, "git_tree").and_then(|raw| sanitize_git_oid(&raw));
    let dirty_status_hash = string_from_object(source_state, "dirty_status_hash")
        .map(|raw| strip_hash_prefix(&raw))
        .filter(|hash| is_64_hex(hash));

    let source_state_hash = compute_source_state_hash(
        git_head.as_deref(),
        git_tree.as_deref(),
        dirty_status_hash.as_deref(),
        &verification_attribution,
    );

    let success = value
        .get("success")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let exit_code = value
        .get("exit_code")
        .and_then(JsonValue::as_i64)
        .and_then(|v| i32::try_from(v).ok());
    let degraded_codes = degraded_codes_from(value);
    let degraded_codes_json = serialize_degraded_codes(&degraded_codes);
    let status = classify_status(success, exit_code, &degraded_codes).to_owned();

    let command_hash = blake3_hex(command_text.as_bytes());

    let (stdout_tail, stdout_tail_hash) =
        bounded_tail_pair(string_field(value, "stdout_tail").as_deref());
    let (stderr_tail, stderr_tail_hash) =
        bounded_tail_pair(string_field(value, "stderr_tail").as_deref());

    let known_blocker = value.get("known_blocker").and_then(JsonValue::as_object);
    let blocker_fingerprint = known_blocker
        .and_then(|obj| obj.get("blocker_fingerprint"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let remediation_bead = known_blocker
        .and_then(|obj| obj.get("remediation_bead"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let retry_after = known_blocker
        .and_then(|obj| obj.get("retry_after"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let remote_required = value
        .get("remote_required")
        .and_then(JsonValue::as_bool)
        .unwrap_or(true);

    Ok(NormalizedRchVerifyRow {
        schema_id: schema,
        command_text: Some(command_text),
        command_hash,
        command_kind,
        bead_id: string_field(value, "bead_id"),
        git_head,
        git_tree,
        source_state_hash,
        dirty_status_hash,
        verification_attribution,
        remote_required,
        worker_id: string_field(value, "worker_id"),
        status,
        exit_code,
        degraded_codes,
        degraded_codes_json,
        stdout_tail_hash,
        stderr_tail_hash,
        stdout_tail,
        stderr_tail,
        blocker_fingerprint,
        remediation_bead,
        retry_after,
    })
}

/// Parse and ingest one RCH verifier proof into the durable ledger.
///
/// The caller supplies `workspace_id` and `created_at` so CLI and tests can
/// keep workspace resolution and clock control outside this pure service
/// boundary. Re-ingesting the same proof returns a deterministic duplicate
/// outcome from the V061 unique index instead of creating a second row.
pub fn ingest_rch_verify_v1(
    connection: &DbConnection,
    workspace_id: &str,
    value: &JsonValue,
    created_at: &str,
) -> Result<RchVerifyIngestReport, RchVerifyLedgerError> {
    let row = parse_rch_verify_v1(value)?;
    let id = rch_verify_run_id(
        &row.command_hash,
        &row.source_state_hash,
        &row.status,
        row.blocker_fingerprint.as_deref(),
    );
    let outcome = connection.insert_rch_verify_run(&id, workspace_id, &row, created_at)?;
    let run = RchVerifyRunView::from_normalized(&id, workspace_id, &row, created_at);
    Ok(RchVerifyIngestReport {
        schema: RCH_VERIFY_LEDGER_INGEST_REPORT_SCHEMA_V1,
        outcome: rch_verify_ingest_outcome_str(outcome),
        inserted_count: if outcome == RchVerifyIngestOutcome::Inserted {
            1
        } else {
            0
        },
        duplicate_count: if outcome == RchVerifyIngestOutcome::Duplicate {
            1
        } else {
            0
        },
        run,
    })
}

/// Query all stored verifier runs for a workspace, optionally filtered by
/// bead id and/or command hash. Ordering is delegated to the repository layer
/// so CLI output and work-packet consumers see one canonical sort.
pub fn list_rch_verify_runs(
    connection: &DbConnection,
    workspace_id: &str,
    bead_id: Option<&str>,
    command_hash: Option<&str>,
    now_rfc3339: &str,
) -> Result<RchVerifyRunsReport, RchVerifyLedgerError> {
    let runs = connection
        .query_rch_verify_runs(workspace_id, bead_id, command_hash, now_rfc3339)?
        .into_iter()
        .map(RchVerifyRunView::from_stored)
        .collect::<Vec<_>>();
    Ok(RchVerifyRunsReport {
        schema: RCH_VERIFY_LEDGER_RUNS_REPORT_SCHEMA_V1,
        run_count: runs.len(),
        runs,
    })
}

/// Query active verifier blockers for a workspace. Expired blockers are
/// intentionally omitted according to the repository layer's `retry_after`
/// cutoff semantics.
pub fn list_rch_verify_blockers(
    connection: &DbConnection,
    workspace_id: &str,
    bead_id: Option<&str>,
    now_rfc3339: &str,
) -> Result<RchVerifyBlockersReport, RchVerifyLedgerError> {
    let all_runs = connection.query_rch_verify_runs(workspace_id, bead_id, None, now_rfc3339)?;
    let active_blockers =
        active_rch_verify_blockers_after_success_supersession(&all_runs, now_rfc3339);
    let blockers = active_blockers
        .into_iter()
        .map(RchVerifyRunView::from_stored)
        .collect::<Vec<_>>();
    Ok(RchVerifyBlockersReport {
        schema: RCH_VERIFY_LEDGER_BLOCKERS_REPORT_SCHEMA_V1,
        blocker_count: blockers.len(),
        blockers,
    })
}

/// Summarize active verifier blockers for status/doctor. The detailed blocker
/// rows are intentionally bounded; the counts remain exact for the query.
pub fn summarize_rch_verify_ledger_status(
    connection: &DbConnection,
    workspace_id: &str,
    now_rfc3339: &str,
) -> Result<RchVerifyLedgerStatusReport, RchVerifyLedgerError> {
    let blockers = list_rch_verify_blockers(connection, workspace_id, None, now_rfc3339)?.blockers;
    let local_fallback_refused_count = blockers
        .iter()
        .filter(|run| rch_verify_run_local_fallback_refused(run))
        .count();
    let mut retry_after_values = blockers
        .iter()
        .filter_map(|run| run.retry_after.clone())
        .collect::<Vec<_>>();
    retry_after_values.sort();
    retry_after_values.dedup();
    let blocker_refs = blockers
        .iter()
        .take(RCH_VERIFY_LEDGER_STATUS_MAX_BLOCKER_REFS)
        .filter_map(RchVerifyLedgerBlockerRef::from_run)
        .collect::<Vec<_>>();
    let recovery_actions = if blockers.is_empty() {
        Vec::new()
    } else {
        vec![RchVerifyLedgerRecoveryAction {
            priority: 1,
            kind: "avoid_duplicate_rch_attempt",
            command: "ee verify rch blockers --workspace . --json",
            message: "Respect retry_after and use static checks or wait before launching duplicate RCH proof.",
        }]
    };

    Ok(RchVerifyLedgerStatusReport {
        schema: RCH_VERIFY_LEDGER_STATUS_SCHEMA_V1,
        status: if blockers.is_empty() {
            "clear"
        } else {
            "active_blockers"
        },
        ledger_available: true,
        active_blocker_count: blockers.len(),
        local_fallback_refused: local_fallback_refused_count > 0,
        local_fallback_refused_count,
        oldest_retry_after: retry_after_values.first().cloned(),
        newest_retry_after: retry_after_values.last().cloned(),
        blocker_refs,
        recovery_actions,
    })
}

/// Summarize active verifier blockers for a workspace path without mutating the
/// database. Missing or unreadable storage returns an unavailable status block
/// instead of failing the caller's status/doctor command.
#[must_use]
pub fn summarize_rch_verify_ledger_status_for_workspace(
    workspace_path: Option<&Path>,
) -> RchVerifyLedgerStatusReport {
    let now = Utc::now().to_rfc3339();
    let Some(workspace_path) = workspace_path else {
        return RchVerifyLedgerStatusReport::not_inspected();
    };
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return RchVerifyLedgerStatusReport::not_initialized();
    }
    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return RchVerifyLedgerStatusReport::unavailable(
            "unreadable",
            "ee doctor --json",
            "RCH verifier ledger database could not be opened.",
        );
    };
    summarize_rch_verify_ledger_status_with_connection(
        Some(workspace_path),
        Some(&connection),
        &now,
    )
}

/// Summarize active verifier blockers using a caller-owned status connection.
/// This keeps `ee status` from opening the same database twice while preserving
/// the same fail-closed, read-only behavior.
#[must_use]
pub fn summarize_rch_verify_ledger_status_with_connection(
    workspace_path: Option<&Path>,
    connection: Option<&DbConnection>,
    now_rfc3339: &str,
) -> RchVerifyLedgerStatusReport {
    let Some(workspace_path) = workspace_path else {
        return RchVerifyLedgerStatusReport::not_inspected();
    };
    let Some(connection) = connection else {
        return RchVerifyLedgerStatusReport::not_initialized();
    };
    let workspace_id = rch_verify_workspace_id(connection, workspace_path);
    summarize_rch_verify_ledger_status(connection, &workspace_id, now_rfc3339).unwrap_or_else(
        |_| {
            RchVerifyLedgerStatusReport::unavailable(
                "query_failed",
                "ee verify rch blockers --workspace . --json",
                "RCH verifier ledger could not query active blockers.",
            )
        },
    )
}

impl RchVerifyRunView {
    fn from_normalized(
        id: &str,
        workspace_id: &str,
        row: &NormalizedRchVerifyRow,
        created_at: &str,
    ) -> Self {
        Self {
            id: id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            schema_id: row.schema_id.clone(),
            command_text: row.command_text.clone(),
            command_hash: row.command_hash.clone(),
            command_kind: row.command_kind.clone(),
            bead_id: row.bead_id.clone(),
            git_head: row.git_head.clone(),
            git_tree: row.git_tree.clone(),
            source_state_hash: row.source_state_hash.clone(),
            dirty_status_hash: row.dirty_status_hash.clone(),
            verification_attribution: row.verification_attribution.clone(),
            remote_required: row.remote_required,
            worker_id: row.worker_id.clone(),
            status: row.status.clone(),
            exit_code: row.exit_code,
            degraded_codes: row.degraded_codes.clone(),
            stdout_tail_hash: row.stdout_tail_hash.clone(),
            stderr_tail_hash: row.stderr_tail_hash.clone(),
            stdout_tail: row.stdout_tail.clone(),
            stderr_tail: row.stderr_tail.clone(),
            blocker_fingerprint: row.blocker_fingerprint.clone(),
            remediation_bead: row.remediation_bead.clone(),
            retry_after: row.retry_after.clone(),
            created_at: created_at.to_owned(),
        }
    }

    fn from_stored(row: StoredRchVerifyRun) -> Self {
        let degraded_codes = degraded_codes_from_json(row.degraded_codes_json.as_deref());
        Self {
            id: row.id,
            workspace_id: row.workspace_id,
            schema_id: row.schema_id,
            command_text: row.command_text,
            command_hash: row.command_hash,
            command_kind: row.command_kind,
            bead_id: row.bead_id,
            git_head: row.git_head,
            git_tree: row.git_tree,
            source_state_hash: row.source_state_hash,
            dirty_status_hash: row.dirty_status_hash,
            verification_attribution: row.verification_attribution,
            remote_required: row.remote_required,
            worker_id: row.worker_id,
            status: row.status,
            exit_code: row.exit_code,
            degraded_codes,
            stdout_tail_hash: row.stdout_tail_hash,
            stderr_tail_hash: row.stderr_tail_hash,
            stdout_tail: row.stdout_tail,
            stderr_tail: row.stderr_tail,
            blocker_fingerprint: row.blocker_fingerprint,
            remediation_bead: row.remediation_bead,
            retry_after: row.retry_after,
            created_at: row.created_at,
        }
    }
}

impl RchVerifyLedgerBlockerRef {
    fn from_run(run: &RchVerifyRunView) -> Option<Self> {
        Some(Self {
            command_hash: run.command_hash.clone(),
            status: run.status.clone(),
            blocker_fingerprint: run.blocker_fingerprint.clone()?,
            remediation_bead: run.remediation_bead.clone(),
            retry_after: run.retry_after.clone(),
            degraded_codes: run.degraded_codes.clone(),
            verification_attribution: run.verification_attribution.clone(),
        })
    }
}

const fn rch_verify_ingest_outcome_str(outcome: RchVerifyIngestOutcome) -> &'static str {
    match outcome {
        RchVerifyIngestOutcome::Inserted => "inserted",
        RchVerifyIngestOutcome::Duplicate => "duplicate",
    }
}

fn active_rch_verify_blockers_after_success_supersession(
    runs: &[StoredRchVerifyRun],
    now_rfc3339: &str,
) -> Vec<StoredRchVerifyRun> {
    let passed_exact_keys = runs
        .iter()
        .filter(|run| run.status == "passed")
        .map(|run| {
            (
                run.command_hash.as_str(),
                run.source_state_hash.as_str(),
                run.created_at.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();

    runs.iter()
        .filter(|run| {
            run.blocker_fingerprint.is_some()
                && run
                    .retry_after
                    .as_deref()
                    .is_none_or(|retry_after| retry_after > now_rfc3339)
                && !passed_exact_keys
                    .iter()
                    .any(|(command_hash, source_state_hash, created_at)| {
                        *command_hash == run.command_hash.as_str()
                            && *source_state_hash == run.source_state_hash.as_str()
                            && *created_at >= run.created_at.as_str()
                    })
        })
        .cloned()
        .collect()
}

fn rch_verify_workspace_id(connection: &DbConnection, workspace_path: &Path) -> String {
    let workspace_path_string = workspace_path.to_string_lossy().into_owned();
    connection
        .get_workspace_by_path(&workspace_path_string)
        .ok()
        .flatten()
        .map(|workspace| workspace.id)
        .unwrap_or_else(|| super::curate::stable_workspace_id(workspace_path))
}

fn rch_verify_run_local_fallback_refused(run: &RchVerifyRunView) -> bool {
    run.degraded_codes
        .iter()
        .any(|code| code == "rch_verify_local_fallback_refused")
}

fn classify_status(
    success: bool,
    exit_code: Option<i32>,
    degraded_codes: &[String],
) -> &'static str {
    if degraded_codes
        .iter()
        .any(|code| code == "rch_verify_local_fallback_detected")
    {
        return "fallback_detected";
    }
    let topology_blocker = degraded_codes.iter().any(|code| {
        matches!(
            code.as_str(),
            "rch_verify_topology_blocked"
                | "rch_verify_local_fallback_refused"
                | "rch_verify_remote_marker_missing"
                | "rch_verify_known_blocker_active"
                | "rch_verify_cargo_path_dependency_version_blocked"
                | "rch_verify_no_worker_capacity"
                | "rch_verify_build_admission_denied"
                | "rch_verify_client_daemon_version_skew"
        )
    });
    if topology_blocker {
        return "blocked";
    }
    if degraded_codes
        .iter()
        .any(|code| code == "rch_verify_remote_command_failed")
        && !success
    {
        return "failed";
    }
    match (success, exit_code) {
        (true, Some(0)) => "passed",
        (true, _) => "passed",
        (false, Some(0)) => "unknown",
        (false, Some(_)) => "failed",
        (false, None) => "blocked",
    }
}

fn degraded_codes_from(value: &JsonValue) -> Vec<String> {
    let mut codes: Vec<String> = value
        .get("degraded_codes")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    codes.sort();
    codes.dedup();
    codes
}

fn serialize_degraded_codes(codes: &[String]) -> Option<String> {
    if codes.is_empty() {
        return Some("[]".to_owned());
    }
    let json = serde_json::to_string(codes).ok()?;
    if json.len() > RCH_VERIFY_LEDGER_DEGRADED_JSON_MAX_BYTES {
        let truncated: Vec<&String> = codes.iter().take(codes.len().min(16)).collect();
        serde_json::to_string(&truncated).ok()
    } else {
        Some(json)
    }
}

fn bounded_tail_pair(tail: Option<&str>) -> (Option<String>, Option<String>) {
    let trimmed = tail.map(str::trim).filter(|value| !value.is_empty());
    let Some(value) = trimmed else {
        return (None, None);
    };
    let hash = Some(blake3_hex(value.as_bytes()));
    if value.len() <= RCH_VERIFY_LEDGER_TAIL_MAX_BYTES {
        (Some(value.to_owned()), hash)
    } else {
        (None, hash)
    }
}

fn compute_source_state_hash(
    git_head: Option<&str>,
    git_tree: Option<&str>,
    dirty_status_hash: Option<&str>,
    verification_attribution: &str,
) -> String {
    let canonical = serde_json::json!({
        "dirty_status_hash": dirty_status_hash,
        "git_head": git_head,
        "git_tree": git_tree,
        "verification_attribution": verification_attribution,
    });
    blake3_hex(canonical.to_string().as_bytes())
}

fn string_field(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn string_from_object(map: &serde_json::Map<String, JsonValue>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn strip_hash_prefix(raw: &str) -> String {
    raw.split_once(':')
        .map(|(_, hex)| hex.trim().to_owned())
        .unwrap_or_else(|| raw.trim().to_owned())
}

fn sanitize_git_oid(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.len() < 7 || lower.len() > 64 {
        return None;
    }
    if !lower.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(lower)
}

fn is_64_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn degraded_codes_from_json(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut codes = serde_json::from_str::<Vec<String>>(raw).unwrap_or_default();
    codes.retain(|code| !code.trim().is_empty());
    for code in &mut codes {
        *code = code.trim().to_owned();
    }
    codes.sort();
    codes.dedup();
    codes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateWorkspaceInput, DbConnection};
    use serde_json::json;

    const TEST_WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
    const TEST_CREATED_AT: &str = "2026-05-23T05:10:00Z";

    fn connection_with_workspace() -> DbConnection {
        let connection = DbConnection::open_memory().expect("open in-memory db");
        connection.migrate().expect("migrate in-memory db");
        connection
            .insert_workspace(
                TEST_WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-rch-verify-ledger".to_owned(),
                    name: Some("verify-ledger-test".to_owned()),
                },
            )
            .expect("insert workspace");
        connection
    }

    fn baseline_success() -> JsonValue {
        json!({
            "schema": "ee.rch.verify.v1",
            "success": true,
            "command": ["cargo", "test", "--lib"],
            "command_text": "cargo test --lib pack",
            "command_kind": "cargo_test",
            "remote_required": true,
            "would_offload": true,
            "worker_id": "worker-01",
            "exit_code": 0,
            "elapsed_ms": 1234,
            "stdout_tail": "passed",
            "stderr_tail": null,
            "degraded_codes": [],
            "source_state": {
                "verification_attribution": "committed_tree",
                "git_head": "0123456789abcdef0123456789abcdef01234567",
                "git_tree": "fedcba9876543210fedcba9876543210fedcba98",
                "dirty_status_hash": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            },
            "known_blocker": null,
            "bead_id": "bd-17awb"
        })
    }

    #[test]
    fn parses_successful_remote_proof() {
        let row = parse_rch_verify_v1(&baseline_success()).expect("parse success");
        assert_eq!(row.schema_id, "ee.rch.verify.v1");
        assert_eq!(row.status, "passed");
        assert_eq!(row.exit_code, Some(0));
        assert_eq!(row.command_kind, "cargo_test");
        assert_eq!(row.command_hash.len(), 64);
        assert_eq!(row.source_state_hash.len(), 64);
        assert!(row.git_head.is_some());
        assert_eq!(
            row.dirty_status_hash.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(row.bead_id.as_deref(), Some("bd-17awb"));
        assert_eq!(row.degraded_codes_json.as_deref(), Some("[]"));
        assert!(row.blocker_fingerprint.is_none());
        assert!(row.remote_required);
    }

    #[test]
    fn classifies_topology_blocker() {
        let mut proof = baseline_success();
        proof["success"] = json!(false);
        proof["exit_code"] = json!(0);
        proof["degraded_codes"] = json!([
            "rch_verify_remote_command_failed",
            "rch_verify_topology_blocked",
            "rch_verify_local_fallback_refused",
            "rch_verify_remote_marker_missing"
        ]);
        proof["known_blocker"] = json!({
            "blocker_fingerprint": "sha256:f7bc698cf3da7706581ae21077954d26b5201f52729e22f71b5df65613b7283f",
            "remediation_bead": "bd-17c65.10.17.1.2",
            "retry_after": "2026-05-23T05:24:43Z"
        });
        let row = parse_rch_verify_v1(&proof).expect("parse blocker");
        assert_eq!(row.status, "blocked");
        assert_eq!(
            row.blocker_fingerprint.as_deref(),
            Some("sha256:f7bc698cf3da7706581ae21077954d26b5201f52729e22f71b5df65613b7283f")
        );
        assert_eq!(row.remediation_bead.as_deref(), Some("bd-17c65.10.17.1.2"));
        assert_eq!(row.retry_after.as_deref(), Some("2026-05-23T05:24:43Z"));
        assert!(
            row.degraded_codes
                .iter()
                .any(|code| code == "rch_verify_topology_blocked")
        );
    }

    #[test]
    fn classifies_no_worker_capacity_as_blocked() {
        let mut proof = baseline_success();
        proof["success"] = json!(false);
        proof["exit_code"] = JsonValue::Null;
        proof["degraded_codes"] = json!(["rch_verify_no_worker_capacity"]);
        let row = parse_rch_verify_v1(&proof).expect("parse");
        assert_eq!(row.status, "blocked");
    }

    #[test]
    fn classifies_local_fallback_refused_as_blocked() {
        let mut proof = baseline_success();
        proof["success"] = json!(false);
        proof["exit_code"] = json!(1);
        proof["degraded_codes"] = json!([
            "rch_verify_local_fallback_refused",
            "rch_verify_remote_command_failed"
        ]);
        let row = parse_rch_verify_v1(&proof).expect("parse");
        assert_eq!(row.status, "blocked");
    }

    #[test]
    fn rejects_wrong_schema() {
        let mut proof = baseline_success();
        proof["schema"] = json!("ee.rch.verify.v2");
        let err = parse_rch_verify_v1(&proof).unwrap_err();
        assert!(matches!(
            err,
            RchVerifyLedgerParseError::UnexpectedSchema { .. }
        ));
    }

    #[test]
    fn rejects_non_object() {
        let err = parse_rch_verify_v1(&json!([])).unwrap_err();
        assert_eq!(err, RchVerifyLedgerParseError::NotAnObject);
    }

    #[test]
    fn rejects_missing_source_state() {
        let mut proof = baseline_success();
        proof.as_object_mut().unwrap().remove("source_state");
        let err = parse_rch_verify_v1(&proof).unwrap_err();
        assert_eq!(err, RchVerifyLedgerParseError::MissingSourceState);
    }

    #[test]
    fn bounds_oversized_stdout_tail_to_hash_only() {
        let mut proof = baseline_success();
        let big = "x".repeat(RCH_VERIFY_LEDGER_TAIL_MAX_BYTES + 1);
        proof["stdout_tail"] = json!(big);
        let row = parse_rch_verify_v1(&proof).expect("parse");
        assert!(row.stdout_tail.is_none());
        assert!(row.stdout_tail_hash.is_some());
        assert_eq!(row.stdout_tail_hash.unwrap().len(), 64);
    }

    #[test]
    fn source_state_hash_is_deterministic_across_calls() {
        let proof = baseline_success();
        let row_a = parse_rch_verify_v1(&proof).expect("parse a");
        let row_b = parse_rch_verify_v1(&proof).expect("parse b");
        assert_eq!(row_a.source_state_hash, row_b.source_state_hash);
        assert_eq!(row_a.command_hash, row_b.command_hash);
    }

    #[test]
    fn dedups_and_sorts_degraded_codes() {
        let mut proof = baseline_success();
        proof["success"] = json!(false);
        proof["exit_code"] = json!(1);
        proof["degraded_codes"] = json!(["b", "a", "b", "c", "a"]);
        let row = parse_rch_verify_v1(&proof).expect("parse");
        assert_eq!(row.degraded_codes, vec!["a", "b", "c"]);
    }

    #[test]
    fn classify_status_table() {
        assert_eq!(classify_status(true, Some(0), &[]), "passed");
        assert_eq!(classify_status(false, Some(1), &[]), "failed");
        assert_eq!(classify_status(false, None, &[]), "blocked");
        assert_eq!(
            classify_status(false, Some(1), &["rch_verify_topology_blocked".to_owned()]),
            "blocked"
        );
        assert_eq!(
            classify_status(
                false,
                Some(0),
                &["rch_verify_local_fallback_detected".to_owned()]
            ),
            "fallback_detected"
        );
    }

    #[test]
    fn ingest_service_inserts_then_dedups_same_proof() {
        let connection = connection_with_workspace();
        let first = ingest_rch_verify_v1(
            &connection,
            TEST_WORKSPACE_ID,
            &baseline_success(),
            TEST_CREATED_AT,
        )
        .expect("first ingest");
        assert_eq!(first.schema, RCH_VERIFY_LEDGER_INGEST_REPORT_SCHEMA_V1);
        assert_eq!(first.outcome, "inserted");
        assert_eq!(first.inserted_count, 1);
        assert_eq!(first.duplicate_count, 0);
        assert_eq!(first.run.workspace_id, TEST_WORKSPACE_ID);
        assert_eq!(first.run.status, "passed");
        assert_eq!(first.run.created_at, TEST_CREATED_AT);

        let second = ingest_rch_verify_v1(
            &connection,
            TEST_WORKSPACE_ID,
            &baseline_success(),
            TEST_CREATED_AT,
        )
        .expect("duplicate ingest");
        assert_eq!(second.outcome, "duplicate");
        assert_eq!(second.inserted_count, 0);
        assert_eq!(second.duplicate_count, 1);
        assert_eq!(second.run.id, first.run.id);
    }

    #[test]
    fn query_services_return_runs_and_active_blockers() {
        let connection = connection_with_workspace();
        ingest_rch_verify_v1(
            &connection,
            TEST_WORKSPACE_ID,
            &baseline_success(),
            TEST_CREATED_AT,
        )
        .expect("ingest success");

        let mut blocked = baseline_success();
        blocked["success"] = json!(false);
        blocked["exit_code"] = json!(1);
        blocked["command_text"] = json!("cargo test --lib blocked-pack");
        blocked["degraded_codes"] = json!([
            "rch_verify_topology_blocked",
            "rch_verify_local_fallback_refused"
        ]);
        blocked["known_blocker"] = json!({
            "blocker_fingerprint": "sha256:f7bc698cf3da7706581ae21077954d26b5201f52729e22f71b5df65613b7283f",
            "remediation_bead": "bd-17c65.10.17.1.2",
            "retry_after": "2026-05-23T06:00:00Z"
        });
        ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &blocked, TEST_CREATED_AT)
            .expect("ingest blocker");

        let runs = list_rch_verify_runs(
            &connection,
            TEST_WORKSPACE_ID,
            Some("bd-17awb"),
            None,
            "2026-05-23T05:30:00Z",
        )
        .expect("query runs");
        assert_eq!(runs.schema, RCH_VERIFY_LEDGER_RUNS_REPORT_SCHEMA_V1);
        assert_eq!(runs.run_count, 2);
        assert_eq!(runs.runs[0].status, "blocked");
        assert!(
            runs.runs[0]
                .degraded_codes
                .iter()
                .any(|code| code == "rch_verify_topology_blocked")
        );

        let blockers = list_rch_verify_blockers(
            &connection,
            TEST_WORKSPACE_ID,
            Some("bd-17awb"),
            "2026-05-23T05:30:00Z",
        )
        .expect("query blockers");
        assert_eq!(blockers.schema, RCH_VERIFY_LEDGER_BLOCKERS_REPORT_SCHEMA_V1);
        assert_eq!(blockers.blocker_count, 1);
        assert_eq!(blockers.blockers[0].status, "blocked");
        assert_eq!(
            blockers.blockers[0].remediation_bead.as_deref(),
            Some("bd-17c65.10.17.1.2")
        );

        let status = summarize_rch_verify_ledger_status(
            &connection,
            TEST_WORKSPACE_ID,
            "2026-05-23T05:30:00Z",
        )
        .expect("status");
        assert_eq!(status.schema, RCH_VERIFY_LEDGER_STATUS_SCHEMA_V1);
        assert_eq!(status.status, "active_blockers");
        assert_eq!(status.active_blocker_count, 1);
        assert!(status.local_fallback_refused);
        assert_eq!(
            status.oldest_retry_after.as_deref(),
            Some("2026-05-23T06:00:00Z")
        );
        assert_eq!(status.blocker_refs.len(), 1);
    }

    #[test]
    fn successful_exact_key_proof_supersedes_active_blocker() {
        let connection = connection_with_workspace();
        let mut blocked = baseline_success();
        blocked["success"] = json!(false);
        blocked["exit_code"] = json!(1);
        blocked["degraded_codes"] = json!([
            "rch_verify_topology_blocked",
            "rch_verify_local_fallback_refused"
        ]);
        blocked["known_blocker"] = json!({
            "blocker_fingerprint": "sha256:f7bc698cf3da7706581ae21077954d26b5201f52729e22f71b5df65613b7283f",
            "remediation_bead": "bd-17c65.10.17.1.2",
            "retry_after": "2026-05-23T06:00:00Z"
        });
        ingest_rch_verify_v1(
            &connection,
            TEST_WORKSPACE_ID,
            &blocked,
            "2026-05-23T05:10:00Z",
        )
        .expect("ingest blocker");
        ingest_rch_verify_v1(
            &connection,
            TEST_WORKSPACE_ID,
            &baseline_success(),
            "2026-05-23T05:20:00Z",
        )
        .expect("ingest success");

        let blockers = list_rch_verify_blockers(
            &connection,
            TEST_WORKSPACE_ID,
            Some("bd-17awb"),
            "2026-05-23T05:30:00Z",
        )
        .expect("query blockers");
        assert_eq!(blockers.blocker_count, 0);

        let status = summarize_rch_verify_ledger_status(
            &connection,
            TEST_WORKSPACE_ID,
            "2026-05-23T05:30:00Z",
        )
        .expect("status");
        assert_eq!(status.status, "clear");
        assert_eq!(status.active_blocker_count, 0);
    }
}
