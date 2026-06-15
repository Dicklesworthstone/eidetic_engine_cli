//! Ingest and query helpers for the RCH verifier evidence ledger (bd-17awb).
//!
//! Parses `ee.rch.verify.v1` proof JSON produced by `scripts/rch_verify.sh` (or
//! equivalent external tooling) into a `NormalizedRchVerifyRow` matching the
//! `rch_verify_runs` schema landed under V061 by bd-22p8c. Tails are bounded,
//! hashes are stripped of source-specific prefixes, and the canonical
//! 64-character hex constraint is enforced before any database write.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use serde::Serialize;
use serde_json::Value as JsonValue;
use toml_edit::{DocumentMut, Item, Table};

use crate::db::{
    DbConnection, DbError, RchVerifyIngestOutcome, StoredRchVerifyRun, rch_verify_run_id,
};

pub const RCH_VERIFY_LEDGER_SCHEMA_V1: &str = "ee.rch.verify.v1";
pub const RCH_VERIFY_LEDGER_INGEST_REPORT_SCHEMA_V1: &str = "ee.rch.verify.ingest.v1";
pub const RCH_VERIFY_LEDGER_RUNS_REPORT_SCHEMA_V1: &str = "ee.rch.verify.runs.v1";
pub const RCH_VERIFY_LEDGER_BLOCKERS_REPORT_SCHEMA_V1: &str = "ee.rch.verify.blockers.v1";
pub const RCH_VERIFY_LEDGER_STATUS_SCHEMA_V1: &str = "ee.rch.verify.ledger_status.v1";
pub const RCH_VERIFY_LEDGER_RECURRENCE_REPORT_SCHEMA_V1: &str = "ee.rch.verify.recurrence.v1";
pub const RCH_VERIFY_TOPOLOGY_CLOSURE_AUDIT_SCHEMA_V1: &str = "ee.rch.topology_closure_audit.v1";
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

/// Read-only recurrence diagnostic for one `ee.rch.verify.v1` proof
/// (bd-b1e4v.1). Classifies whether the run was blocked by the verification
/// environment before Cargo ran on materialized remote source, and whether
/// the active blocker recurs under a remediation bead the tracker already
/// closed. An environment blocker is never evidence about the source bead:
/// `source_closeable` stays false for every status except `passed`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchVerifyRecurrenceReport {
    pub schema: &'static str,
    pub classification: &'static str,
    pub status: String,
    pub recurs_closed_remediation: bool,
    pub closed_remediation_refs: Vec<String>,
    pub active_blocker_fingerprint: Option<String>,
    pub remediation_bead: Option<String>,
    pub retry_after: Option<String>,
    pub source_materialization: Option<String>,
    pub remote_source_materialized: Option<bool>,
    pub selected_worker: Option<String>,
    pub local_fallback_refused: bool,
    pub source_closeable: bool,
    pub blocker_string: Option<String>,
    pub error_codes: Vec<String>,
    pub degraded_codes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyClosureAuditReport {
    pub schema: &'static str,
    pub status: &'static str,
    pub source_state_hash: String,
    pub manifest_hash: String,
    pub path_dependency_count: usize,
    pub root_category_counts: Vec<RchTopologyRootCategoryCount>,
    pub roots: Vec<RchTopologyPathRootAudit>,
    pub unresolved_topology_edges: Vec<RchTopologyUnresolvedEdge>,
    pub source_materialization: Option<String>,
    pub remote_source_materialized: Option<bool>,
    pub local_fallback_refused: bool,
    pub rch_runtime: RchTopologyRuntimeSummary,
    pub refusal: Option<RchTopologyClosureRefusal>,
    pub recovery_actions: Vec<RchTopologyClosureRecoveryAction>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyRootCategoryCount {
    pub category: String,
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyPathRootAudit {
    pub dependency: String,
    pub section: String,
    pub path_hash: String,
    pub path_evidence: String,
    pub root_category: String,
    pub expected_worker_mapping: String,
    pub local_path_exists: Option<bool>,
    pub canonical_escapes_project_root: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyUnresolvedEdge {
    pub code: String,
    pub severity: &'static str,
    pub evidence: String,
    pub recovery_hint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyRuntimeSummary {
    pub status: Option<String>,
    pub client_version: Option<String>,
    pub client_compat: Option<String>,
    pub daemon_version: Option<String>,
    pub daemon_compat: Option<String>,
    pub compatibility: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyClosureRefusal {
    pub code: &'static str,
    pub message: String,
    pub blocker_string: Option<String>,
    pub refused_before_cargo: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyClosureRecoveryAction {
    pub priority: u8,
    pub kind: &'static str,
    pub command: &'static str,
    pub message: &'static str,
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

#[derive(Debug)]
pub enum RchTopologyClosureAuditError {
    Proof(RchVerifyLedgerParseError),
    ManifestParse(String),
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

impl fmt::Display for RchTopologyClosureAuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proof(source) => write!(f, "{source}"),
            Self::ManifestParse(source) => write!(f, "failed to parse Cargo manifest: {source}"),
        }
    }
}

impl Error for RchTopologyClosureAuditError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Proof(source) => Some(source),
            Self::ManifestParse(_) => None,
        }
    }
}

impl From<RchVerifyLedgerParseError> for RchTopologyClosureAuditError {
    fn from(source: RchVerifyLedgerParseError) -> Self {
        Self::Proof(source)
    }
}

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

/// Classify recurrence evidence for one `ee.rch.verify.v1` proof JSON value.
///
/// `closed_remediation_beads` is caller-supplied tracker knowledge: the set
/// of remediation bead ids known to be closed. The detector itself never
/// reads the tracker — bd-b1e4v.1 keeps this surface pure and read-only;
/// joining live tracker/proof-broker state belongs to bd-b1e4v.4. A blocked
/// run whose `known_blocker.remediation_bead` appears in that set is a
/// recurrence of a supposedly remediated blocker, regardless of which error
/// code produced it (RCH-E327 topology and capacity/queue-timeout blockers
/// both recur this way with distinct root causes).
pub fn classify_rch_verify_recurrence(
    value: &JsonValue,
    closed_remediation_beads: &[String],
) -> Result<RchVerifyRecurrenceReport, RchVerifyLedgerParseError> {
    let row = parse_rch_verify_v1(value)?;
    let source_state = value.get("source_state").and_then(JsonValue::as_object);
    let remote_source_materialized = source_state
        .and_then(|obj| obj.get("remote_source_materialized"))
        .and_then(JsonValue::as_bool);
    let source_materialization = source_state
        .and_then(|obj| obj.get("source_materialization"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_owned);

    let probe = value
        .get("selector_admission_probe")
        .and_then(JsonValue::as_object);
    let selected_worker = probe
        .and_then(|obj| obj.get("selected_worker"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_owned)
        .or_else(|| row.worker_id.clone());
    let local_fallback_refused = probe
        .and_then(|obj| obj.get("local_fallback_refused"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
        || row
            .degraded_codes
            .iter()
            .any(|code| code == "rch_verify_local_fallback_refused");

    let error_codes: Vec<String> = value
        .get("error_codes")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let blocker_string = first_line_with_any_code(row.stderr_tail.as_deref(), &error_codes)
        .or_else(|| first_line_with_any_code(row.stdout_tail.as_deref(), &error_codes));

    let classification = match row.status.as_str() {
        "blocked" => {
            if remote_source_materialized == Some(true) {
                "environment_blocked_after_materialization"
            } else {
                "environment_blocked_before_cargo"
            }
        }
        "fallback_detected" => "local_fallback_detected",
        "passed" | "failed" => "source_outcome",
        _ => "indeterminate",
    };
    let closed_remediation_refs = row
        .remediation_bead
        .as_deref()
        .filter(|bead| closed_remediation_beads.iter().any(|closed| closed == bead))
        .map(|bead| vec![bead.to_owned()])
        .unwrap_or_default();
    let recurs_closed_remediation = row.status == "blocked" && !closed_remediation_refs.is_empty();

    Ok(RchVerifyRecurrenceReport {
        schema: RCH_VERIFY_LEDGER_RECURRENCE_REPORT_SCHEMA_V1,
        classification,
        status: row.status.clone(),
        recurs_closed_remediation,
        closed_remediation_refs,
        active_blocker_fingerprint: row.blocker_fingerprint.clone(),
        remediation_bead: row.remediation_bead.clone(),
        retry_after: row.retry_after.clone(),
        source_materialization,
        remote_source_materialized,
        selected_worker,
        local_fallback_refused,
        source_closeable: row.status == "passed",
        blocker_string,
        error_codes,
        degraded_codes: row.degraded_codes,
    })
}

/// Build a bounded, read-only path topology audit for an existing RCH proof.
///
/// The audit never runs Cargo or RCH. It combines the proof's source-state and
/// blocker evidence with path dependencies declared in the supplied manifest so
/// a caller can see whether remote source materialization is blocked by a path
/// root that needs explicit worker topology handling.
pub fn audit_rch_topology_closure(
    value: &JsonValue,
    manifest_text: &str,
    manifest_dir: &Path,
    canonical_project_root: Option<&Path>,
) -> Result<RchTopologyClosureAuditReport, RchTopologyClosureAuditError> {
    let row = parse_rch_verify_v1(value)?;
    let path_deps = manifest_path_dependencies(manifest_text)?;
    let roots = path_deps
        .into_iter()
        .map(|dependency| {
            topology_root_audit_for_dependency(&dependency, manifest_dir, canonical_project_root)
        })
        .collect::<Vec<_>>();
    let root_category_counts = root_category_counts(&roots);
    let source_state = value.get("source_state").and_then(JsonValue::as_object);
    let source_materialization = source_state
        .and_then(|obj| obj.get("source_materialization"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_owned);
    let remote_source_materialized = source_state
        .and_then(|obj| obj.get("remote_source_materialized"))
        .and_then(JsonValue::as_bool);
    let error_codes = error_codes_from(value);
    let blocker_string = first_line_with_any_code(row.stderr_tail.as_deref(), &error_codes)
        .or_else(|| first_line_with_any_code(row.stdout_tail.as_deref(), &error_codes));
    let combined_tail = combined_tail(row.stdout_tail.as_deref(), row.stderr_tail.as_deref());
    let local_fallback_refused = value
        .get("selector_admission_probe")
        .and_then(JsonValue::as_object)
        .and_then(|obj| obj.get("local_fallback_refused"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
        || row
            .degraded_codes
            .iter()
            .any(|code| code == "rch_verify_local_fallback_refused");
    let topology_blocked = row
        .degraded_codes
        .iter()
        .any(|code| code == "rch_verify_topology_blocked")
        || error_codes.iter().any(|code| code == "RCH-E327");
    let unresolved_topology_edges =
        topology_unresolved_edges(&row.degraded_codes, &error_codes, &combined_tail, &roots);
    let risky_root_present = roots
        .iter()
        .any(|root| !matches!(root.root_category.as_str(), "primary_project" | "dp_root"));
    let refused = topology_blocked || !unresolved_topology_edges.is_empty() || risky_root_present;
    let status = if refused {
        "refused_unproven"
    } else {
        "closure_proven"
    };
    let refusal = refused.then(|| RchTopologyClosureRefusal {
        code: "rch_topology_closure_unproven",
        message: "RCH path dependency closure cannot be proven safe from the supplied proof and manifest; do not launch or close source proof until the unresolved topology edge is cleared.".to_owned(),
        blocker_string,
        refused_before_cargo: row.status == "blocked" && remote_source_materialized != Some(true),
    });

    Ok(RchTopologyClosureAuditReport {
        schema: RCH_VERIFY_TOPOLOGY_CLOSURE_AUDIT_SCHEMA_V1,
        status,
        source_state_hash: row.source_state_hash,
        manifest_hash: blake3_hex(manifest_text.as_bytes()),
        path_dependency_count: roots.len(),
        root_category_counts,
        roots,
        unresolved_topology_edges,
        source_materialization,
        remote_source_materialized,
        local_fallback_refused,
        rch_runtime: rch_runtime_summary(value),
        refusal,
        recovery_actions: vec![
            RchTopologyClosureRecoveryAction {
                priority: 1,
                kind: "read_only_topology_audit",
                command: "ee verify rch topology-audit --from-json proof.json --manifest Cargo.toml --json",
                message: "Re-run the bounded audit after RCH topology or manifest evidence changes.",
            },
            RchTopologyClosureRecoveryAction {
                priority: 2,
                kind: "lane_doctor",
                command: "scripts/rch_lane_doctor.sh --json",
                message: "Inspect local RCH root mapping hints without launching Cargo.",
            },
        ],
    })
}

/// Return the first tail line that carries one of the proof's extracted
/// error codes, preserving the line exactly (only the trailing newline is
/// trimmed) so downstream consumers can match the blocker verbatim.
fn first_line_with_any_code(tail: Option<&str>, error_codes: &[String]) -> Option<String> {
    let tail = tail?;
    if error_codes.is_empty() {
        return None;
    }
    tail.lines()
        .find(|line| error_codes.iter().any(|code| line.contains(code.as_str())))
        .map(|line| line.trim_end().to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestPathDependency {
    name: String,
    section: String,
    path: String,
}

fn manifest_path_dependencies(
    manifest_text: &str,
) -> Result<Vec<ManifestPathDependency>, RchTopologyClosureAuditError> {
    let document = manifest_text
        .parse::<DocumentMut>()
        .map_err(|source| RchTopologyClosureAuditError::ManifestParse(source.to_string()))?;
    let mut deps = Vec::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = document.get(section).and_then(Item::as_table) {
            collect_manifest_dependency_table(section, table, &mut deps);
        }
    }
    if let Some(patch) = document.get("patch").and_then(Item::as_table) {
        for (registry, item) in patch.iter() {
            if let Some(table) = item.as_table() {
                let section = format!("patch.{registry}");
                collect_manifest_dependency_table(&section, table, &mut deps);
            }
        }
    }
    if let Some(targets) = document.get("target").and_then(Item::as_table) {
        for (target, item) in targets.iter() {
            if let Some(target_table) = item.as_table() {
                for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    if let Some(table) = target_table.get(section).and_then(Item::as_table) {
                        let target_section = format!("target.{target}.{section}");
                        collect_manifest_dependency_table(&target_section, table, &mut deps);
                    }
                }
            }
        }
    }
    deps.sort_by(|left, right| {
        left.section
            .cmp(&right.section)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.path.cmp(&right.path))
    });
    deps.dedup();
    Ok(deps)
}

fn collect_manifest_dependency_table(
    section: &str,
    table: &Table,
    deps: &mut Vec<ManifestPathDependency>,
) {
    for (name, item) in table.iter() {
        if let Some(path) = dependency_path(item) {
            deps.push(ManifestPathDependency {
                name: name.to_owned(),
                section: section.to_owned(),
                path,
            });
        }
    }
}

fn dependency_path(item: &Item) -> Option<String> {
    item.as_table()
        .and_then(|table| table.get("path"))
        .and_then(Item::as_str)
        .or_else(|| {
            item.as_inline_table()
                .and_then(|table| table.get("path"))
                .and_then(|value| value.as_str())
        })
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_owned)
}

fn topology_root_audit_for_dependency(
    dependency: &ManifestPathDependency,
    manifest_dir: &Path,
    canonical_project_root: Option<&Path>,
) -> RchTopologyPathRootAudit {
    let raw_path = dependency.path.as_str();
    let local_path = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        manifest_dir.join(raw_path)
    };
    let canonical = std::fs::canonicalize(&local_path).ok();
    let canonical_escapes_project_root = canonical_project_root.and_then(|root| {
        canonical
            .as_ref()
            .map(|canonical| !path_starts_with(canonical, root))
    });
    let root_category =
        classify_topology_root(raw_path, canonical.as_deref(), canonical_project_root);
    RchTopologyPathRootAudit {
        dependency: dependency.name.clone(),
        section: dependency.section.clone(),
        path_hash: blake3_hex(raw_path.as_bytes()),
        path_evidence: redacted_path_evidence(raw_path),
        expected_worker_mapping: expected_worker_mapping_for_category(root_category.as_str()),
        root_category,
        local_path_exists: Some(local_path.exists()),
        canonical_escapes_project_root,
    }
}

fn classify_topology_root(
    raw_path: &str,
    canonical: Option<&Path>,
    canonical_project_root: Option<&Path>,
) -> String {
    let normalized = raw_path.replace('\\', "/");
    if normalized.starts_with("/data/projects/") {
        return "absolute_data_projects_root".to_owned();
    }
    if normalized == "/dp" || normalized.starts_with("/dp/") {
        return "dp_root".to_owned();
    }
    if let (Some(canonical), Some(project_root)) = (canonical, canonical_project_root)
        && !path_starts_with(canonical, project_root)
        && normalized.starts_with("../")
    {
        return "symlinked_sibling_escaping_canonical_root".to_owned();
    }
    if normalized.starts_with("../") && franken_stack_sibling(&normalized).is_some() {
        return "franken_stack_sibling".to_owned();
    }
    if normalized.starts_with("../") {
        return "sibling_under_canonical_project_root".to_owned();
    }
    if Path::new(raw_path).is_absolute() {
        return "external_unsupported_root".to_owned();
    }
    "primary_project".to_owned()
}

fn path_starts_with(path: &Path, root: &Path) -> bool {
    let normalize_components = |path: &Path| {
        path.components()
            .filter(|component| !matches!(component, Component::CurDir))
            .collect::<Vec<_>>()
    };
    let path_components = normalize_components(path);
    let root_components = normalize_components(root);
    path_components.starts_with(&root_components)
}

fn franken_stack_sibling(normalized: &str) -> Option<&'static str> {
    let sibling = normalized.trim_start_matches("../").split('/').next()?;
    match sibling {
        "asupersync"
        | "franken_agent_detection"
        | "franken_networkx"
        | "frankensearch"
        | "frankensqlite"
        | "sqlmodel_rust"
        | "toon_rust" => Some(sibling),
        _ => None,
    }
}

fn redacted_path_evidence(raw_path: &str) -> String {
    let normalized = raw_path.replace('\\', "/");
    if normalized.starts_with("/data/projects/") {
        return normalized;
    }
    if normalized == "/dp" || normalized.starts_with("/dp/") {
        return normalized;
    }
    if let Some(sibling) = franken_stack_sibling(&normalized) {
        return format!("relative_parent:{sibling}");
    }
    if normalized.starts_with("../") {
        return "relative_parent:<hashed>".to_owned();
    }
    if Path::new(raw_path).is_absolute() {
        return "absolute_external:<hashed>".to_owned();
    }
    normalized
}

fn expected_worker_mapping_for_category(category: &str) -> String {
    match category {
        "primary_project" => "sync_tree/project_root".to_owned(),
        "dp_root" => "worker_global_dp_root".to_owned(),
        "absolute_data_projects_root" => {
            "worker_absolute_data_projects_or_alias_rewrite_required".to_owned()
        }
        "symlinked_sibling_escaping_canonical_root" => {
            "recreate_link_or_sync_target_inside_worker_projects_root".to_owned()
        }
        "franken_stack_sibling" | "sibling_under_canonical_project_root" => {
            "sibling_root_sync_under_worker_projects_root_required".to_owned()
        }
        _ => "unsupported_without_explicit_rch_topology_admission".to_owned(),
    }
}

fn root_category_counts(roots: &[RchTopologyPathRootAudit]) -> Vec<RchTopologyRootCategoryCount> {
    let mut counts = BTreeMap::<String, usize>::new();
    for root in roots {
        *counts.entry(root.root_category.clone()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(category, count)| RchTopologyRootCategoryCount { category, count })
        .collect()
}

fn error_codes_from(value: &JsonValue) -> Vec<String> {
    let mut codes = value
        .get("error_codes")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    codes.sort();
    codes.dedup();
    codes
}

fn combined_tail(stdout_tail: Option<&str>, stderr_tail: Option<&str>) -> String {
    [
        stdout_tail.unwrap_or_default(),
        stderr_tail.unwrap_or_default(),
    ]
    .join("\n")
}

fn topology_unresolved_edges(
    degraded_codes: &[String],
    error_codes: &[String],
    combined_tail: &str,
    roots: &[RchTopologyPathRootAudit],
) -> Vec<RchTopologyUnresolvedEdge> {
    let lower_tail = combined_tail.to_ascii_lowercase();
    let mut edges = BTreeMap::<String, RchTopologyUnresolvedEdge>::new();
    let topology_blocked = degraded_codes
        .iter()
        .any(|code| code == "rch_verify_topology_blocked")
        || error_codes.iter().any(|code| code == "RCH-E327");

    if topology_blocked
        && roots.iter().any(|root| {
            matches!(
                root.root_category.as_str(),
                "symlinked_sibling_escaping_canonical_root" | "franken_stack_sibling"
            )
        })
    {
        edges.insert(
            "symlinked_sibling_escaping_canonical_root".to_owned(),
            RchTopologyUnresolvedEdge {
                code: "symlinked_sibling_escaping_canonical_root".to_owned(),
                severity: "high",
                evidence: "RCH-E327 topology blocker with franken-stack parent path dependency roots".to_owned(),
                recovery_hint: "Admit the symlink target into the RCH sync closure or recreate the link against worker-local /data/projects roots before running Cargo proof.".to_owned(),
            },
        );
    }

    if topology_blocked
        && roots
            .iter()
            .any(|root| root.root_category == "absolute_data_projects_root")
    {
        edges.insert(
            "absolute_data_projects_root_requires_worker_alias".to_owned(),
            RchTopologyUnresolvedEdge {
                code: "absolute_data_projects_root_requires_worker_alias".to_owned(),
                severity: "high",
                evidence: "Manifest includes an absolute /data/projects path dependency while the proof is topology-blocked".to_owned(),
                recovery_hint: "Confirm worker /data/projects availability or rewrite the dependency through an admitted alias root.".to_owned(),
            },
        );
    }

    if lower_tail.contains("sun_len") || lower_tail.contains("path must be shorter than sun_len") {
        edges.insert(
            "tmpdir_rewrite_exceeds_sun_len".to_owned(),
            RchTopologyUnresolvedEdge {
                code: "tmpdir_rewrite_exceeds_sun_len".to_owned(),
                severity: "high",
                evidence: "Proof tail reports Unix socket path length overflow after TMPDIR rewrite".to_owned(),
                recovery_hint: "Use a short worker TMPDIR for socket-binding tests before launching daemon proof.".to_owned(),
            },
        );
    }

    if lower_tail.contains("no space left on device") || lower_tail.contains("enospc") {
        edges.insert(
            "job_target_dir_accumulation".to_owned(),
            RchTopologyUnresolvedEdge {
                code: "job_target_dir_accumulation".to_owned(),
                severity: "high",
                evidence: "Proof tail reports worker disk exhaustion".to_owned(),
                recovery_hint:
                    "Prune or reuse worker job target directories before retrying RCH proof."
                        .to_owned(),
            },
        );
    }

    edges.into_values().collect()
}

fn rch_runtime_summary(value: &JsonValue) -> RchTopologyRuntimeSummary {
    let runtime = value.get("rch_runtime").and_then(JsonValue::as_object);
    let status = runtime
        .and_then(|obj| obj.get("status"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let client_version = runtime
        .and_then(|obj| obj.get("client_version"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let client_compat = runtime
        .and_then(|obj| obj.get("client_compat"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let daemon_version = runtime
        .and_then(|obj| obj.get("daemon_version"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let daemon_compat = runtime
        .and_then(|obj| obj.get("daemon_compat"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let compatibility = match (client_compat.as_deref(), daemon_compat.as_deref()) {
        (Some(client), Some(daemon)) if client == daemon => "matched",
        (Some(_), Some(_)) => "mismatched",
        (Some(_), None) | (None, Some(_)) => "partial",
        (None, None) => "unknown",
    };
    RchTopologyRuntimeSummary {
        status,
        client_version,
        client_compat,
        daemon_version,
        daemon_compat,
        compatibility,
    }
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
            "unavailable",
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
                "unavailable",
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

    fn blocked_topology_recurrence_proof() -> JsonValue {
        let mut proof = baseline_success();
        proof["success"] = json!(false);
        proof["exit_code"] = JsonValue::Null;
        proof["worker_id"] = JsonValue::Null;
        proof["error_codes"] = json!(["RCH-E327"]);
        proof["degraded_codes"] = json!([
            "rch_verify_topology_blocked",
            "rch_verify_local_fallback_refused"
        ]);
        proof["stderr_tail"] = json!(
            "RCH-E327: Path dependency topology policy failed; move dependencies under /data/projects (or /dp) and retry.\nremote required; refusing local fallback\n"
        );
        proof["source_state"]["remote_source_materialized"] = json!(false);
        proof["source_state"]["source_materialization"] = json!("none");
        proof["selector_admission_probe"] = json!({
            "schema": "ee.rch.selector_admission_probe.v1",
            "status": "selection_failed",
            "selected_worker": null,
            "selection_failure_reason": "topology_blocked",
            "remote_required": true,
            "local_fallback_refused": true
        });
        proof["known_blocker"] = json!({
            "blocker_fingerprint": "sha256:2d65a1881c41fb5e52c8b3e7ed7ac95085c25dfab7ab1a302471938fed165fc4",
            "remediation_bead": "bd-17c65.10.17.1.2",
            "retry_after": "2026-06-09T20:24:29.320084Z"
        });
        proof
    }

    #[test]
    fn recurrence_flags_blocked_run_against_closed_remediation() {
        let proof = blocked_topology_recurrence_proof();
        let closed = vec!["bd-17c65.10.17.1.2".to_owned()];
        let report = classify_rch_verify_recurrence(&proof, &closed).expect("classify");
        assert_eq!(report.schema, RCH_VERIFY_LEDGER_RECURRENCE_REPORT_SCHEMA_V1);
        assert_eq!(report.classification, "environment_blocked_before_cargo");
        assert!(report.recurs_closed_remediation);
        assert_eq!(report.closed_remediation_refs, closed);
        assert_eq!(
            report.active_blocker_fingerprint.as_deref(),
            Some("sha256:2d65a1881c41fb5e52c8b3e7ed7ac95085c25dfab7ab1a302471938fed165fc4")
        );
        assert_eq!(
            report.retry_after.as_deref(),
            Some("2026-06-09T20:24:29.320084Z")
        );
        assert_eq!(report.source_materialization.as_deref(), Some("none"));
        assert_eq!(report.remote_source_materialized, Some(false));
        assert_eq!(report.selected_worker, None);
        assert!(report.local_fallback_refused);
        assert!(!report.source_closeable);
        assert_eq!(
            report.blocker_string.as_deref(),
            Some(
                "RCH-E327: Path dependency topology policy failed; move dependencies under /data/projects (or /dp) and retry."
            )
        );
    }

    #[test]
    fn recurrence_without_tracker_knowledge_still_refuses_closeout() {
        let proof = blocked_topology_recurrence_proof();
        let report = classify_rch_verify_recurrence(&proof, &[]).expect("classify");
        assert!(!report.recurs_closed_remediation);
        assert!(report.closed_remediation_refs.is_empty());
        assert_eq!(report.classification, "environment_blocked_before_cargo");
        assert!(!report.source_closeable);
    }

    #[test]
    fn recurrence_passed_run_is_source_closeable_and_not_recurrence() {
        let report =
            classify_rch_verify_recurrence(&baseline_success(), &["bd-17c65.10.17.1.2".to_owned()])
                .expect("classify");
        assert_eq!(report.classification, "source_outcome");
        assert!(!report.recurs_closed_remediation);
        assert!(report.source_closeable);
        assert_eq!(report.selected_worker.as_deref(), Some("worker-01"));
        assert!(!report.local_fallback_refused);
        assert!(report.blocker_string.is_none());
    }

    #[test]
    fn recurrence_report_serializes_bead_contract_field_names() {
        let proof = blocked_topology_recurrence_proof();
        let closed = vec!["bd-17c65.10.17.1.2".to_owned()];
        let report = classify_rch_verify_recurrence(&proof, &closed).expect("classify");
        let value = serde_json::to_value(&report).expect("serialize");
        for key in [
            "recursClosedRemediation",
            "closedRemediationRefs",
            "activeBlockerFingerprint",
            "retryAfter",
            "sourceMaterialization",
            "remoteSourceMaterialized",
            "selectedWorker",
            "localFallbackRefused",
            "sourceCloseable",
            "blockerString",
        ] {
            assert!(
                value.get(key).is_some(),
                "recurrence report must serialize stable field {key}"
            );
        }
    }

    #[test]
    fn topology_closure_audit_refuses_franken_stack_topology_gap() {
        let mut proof = blocked_topology_recurrence_proof();
        proof["rch_runtime"] = json!({
            "status": "checked",
            "client_version": "0.9.1",
            "client_compat": "0.9",
            "daemon_version": "0.9.1",
            "daemon_compat": "0.9"
        });
        let manifest = r#"
[dependencies]
fnx-runtime = { version = "0.1.0", path = "../franken_networkx/crates/fnx-runtime" }
frankensearch = { version = "0.3.0", path = "../frankensearch/frankensearch" }
asupersync = { version = "0.3.4", path = "/data/projects/asupersync" }

[dev-dependencies]
fsqlite = { path = "../frankensqlite/crates/fsqlite" }
"#;

        let report = audit_rch_topology_closure(
            &proof,
            manifest,
            Path::new("/Users/jemanuel/projects/eidetic_engine_cli"),
            Some(Path::new("/Users/jemanuel/projects")),
        )
        .expect("audit");

        assert_eq!(report.schema, RCH_VERIFY_TOPOLOGY_CLOSURE_AUDIT_SCHEMA_V1);
        assert_eq!(report.status, "refused_unproven");
        assert_eq!(report.path_dependency_count, 4);
        assert_eq!(report.source_state_hash.len(), 64);
        assert_eq!(report.manifest_hash.len(), 64);
        assert_eq!(report.source_materialization.as_deref(), Some("none"));
        assert_eq!(report.remote_source_materialized, Some(false));
        assert!(report.local_fallback_refused);
        assert_eq!(report.rch_runtime.compatibility, "matched");
        assert!(report.roots.iter().any(|root| root.path_evidence
            == "relative_parent:franken_networkx"
            && root.root_category == "franken_stack_sibling"));
        assert!(report.roots.iter().any(|root| {
            root.path_evidence == "/data/projects/asupersync"
                && root.root_category == "absolute_data_projects_root"
        }));
        let edge_codes = report
            .unresolved_topology_edges
            .iter()
            .map(|edge| edge.code.as_str())
            .collect::<BTreeSet<_>>();
        assert!(edge_codes.contains("symlinked_sibling_escaping_canonical_root"));
        assert!(edge_codes.contains("absolute_data_projects_root_requires_worker_alias"));
        let refusal = report.refusal.expect("refusal");
        assert_eq!(refusal.code, "rch_topology_closure_unproven");
        assert!(refusal.refused_before_cargo);
        assert!(
            refusal
                .blocker_string
                .as_deref()
                .is_some_and(|line| line.contains("RCH-E327"))
        );
    }

    #[test]
    fn topology_closure_audit_accepts_primary_project_path_roots() {
        let mut proof = baseline_success();
        proof["source_state"]["remote_source_materialized"] = json!(true);
        proof["source_state"]["source_materialization"] = json!("git_archive");
        let manifest = r#"
[dependencies]
determinism = { version = "0.1.0", path = "crates/determinism" }
"#;

        let report = audit_rch_topology_closure(
            &proof,
            manifest,
            Path::new("/repo/eidetic_engine_cli"),
            Some(Path::new("/repo")),
        )
        .expect("audit");

        assert_eq!(report.status, "closure_proven");
        assert_eq!(report.path_dependency_count, 1);
        assert_eq!(
            report.roots[0].expected_worker_mapping,
            "sync_tree/project_root"
        );
        assert!(report.unresolved_topology_edges.is_empty());
        assert!(report.refusal.is_none());
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
