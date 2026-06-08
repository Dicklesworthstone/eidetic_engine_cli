//! Verification evidence normalizer (`bd-1nxz4.5`).
//!
//! Agents in the ee swarm copy verification proof from many sources — RCH
//! proof JSON, verify-script tails, GitHub Actions job summaries, local
//! static-check records — into Beads and Agent Mail. Downstream consumers
//! (completion-audit, support-bundle, closeout playbook) have to interpret
//! that heterogeneous evidence today.
//!
//! This module is a **read-only** library layer that ingests existing
//! evidence shapes and normalizes them into a single
//! [`VerificationEvidence`] envelope tagged with the
//! [`VERIFICATION_EVIDENCE_SCHEMA_V1`] schema. It never runs Cargo, never
//! shells out, never deletes files, and preserves the command hashes and
//! git tree identity that came in with each source proof.
//!
//! Environment blockers (path-dep version skew, topology refusal, worker
//! disk pressure, daemon version mismatch) are classified separately from
//! genuine code failures (compile/lint errors, test failures) so a
//! consumer can tell "RCH is sick" apart from "the diff is broken".
//!
//! ## What's in this slice
//!
//! - [`parse_rch_verify`] for the `ee.rch.verify.v1` proof JSON that
//!   `scripts/rch_verify.sh` already emits.
//! - [`parse_verify_script_event`] for `ee.test_event.v1` command-tail
//!   rows emitted by verify and e2e scripts.
//! - [`parse_github_actions_job`] for canonical GitHub Actions check-run
//!   summaries.
//! - [`parse_static_check`] for local static-only records such as
//!   `rustfmt --check` and `git diff --check`.
//! - [`compact_summary`] that renders a Beads-ready Markdown bullet list.
//!
//! ## Follow-up (tracked under `bd-1nxz4.5`)
//!
//! - `ee verification evidence ...` CLI surface and golden output.
//! - End-to-end harness driving fake input files into the normalizer
//!   without launching Cargo.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

/// Stable schema tag emitted on every normalized envelope. Mirrored in
/// `docs/schemas/ee.verification_evidence.v1.json`.
pub const VERIFICATION_EVIDENCE_SCHEMA_V1: &str = "ee.verification_evidence.v1";
const RCH_VERIFY_SCHEMA_V1: &str = "ee.rch.verify.v1";
const TEST_EVENT_SCHEMA_V1: &str = "ee.test_event.v1";
const GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1: &str = "ee.github_actions.check_run.v1";
const STATIC_CHECK_SCHEMA_V1: &str = "ee.static_check.v1";

/// Where the raw proof came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    /// `ee.rch.verify.v1` proof JSON from `scripts/rch_verify.sh`.
    RchVerify,
    /// `ee.test_event.v1` style verify-script tail.
    VerifyScript,
    /// GitHub Actions job summary or check-run JSON.
    GitHubActionsJob,
    /// Local static-only proof — rustfmt, clippy, UBS, git diff --check.
    StaticCheck,
}

impl EvidenceSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RchVerify => "rch_verify",
            Self::VerifyScript => "verify_script",
            Self::GitHubActionsJob => "github_actions_job",
            Self::StaticCheck => "static_check",
        }
    }
}

/// Normalized classification across every source.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    /// Verification ran and the diff is good.
    Passed,
    /// Verification ran and reported a real code failure (compile error,
    /// lint, test failure). The diff is the suspect.
    FailedInCode,
    /// Verification could not run because of an environment blocker
    /// (path-dep version skew, worker disk, daemon mismatch, topology
    /// refusal, malformed worker state). The diff was never exercised.
    EnvironmentBlocked,
    /// Input could not be parsed as a recognized evidence shape.
    Malformed,
}

impl EvidenceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::FailedInCode => "failed_in_code",
            Self::EnvironmentBlocked => "environment_blocked",
            Self::Malformed => "malformed",
        }
    }
}

/// First error location preserved from the source proof. Optional because
/// passing proofs and environment-blocked proofs may not carry one.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstError {
    pub file: Option<String>,
    pub line: Option<u64>,
    pub message: Option<String>,
}

impl FirstError {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.file.is_none() && self.line.is_none() && self.message.is_none()
    }
}

/// Normalized verification-evidence envelope. Designed to drop straight
/// into the `data.evidence` slot of an `ee.response.v2` response, or to be
/// pasted as a Beads comment via [`compact_summary`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationEvidence {
    pub schema: &'static str,
    pub source: EvidenceSource,
    pub status: EvidenceStatus,
    pub bead_id: Option<String>,
    pub command: Option<String>,
    pub command_kind: Option<String>,
    pub command_hash: Option<String>,
    pub worker_id: Option<String>,
    pub exit_code: Option<i64>,
    pub elapsed_ms: Option<u64>,
    pub git_head: Option<String>,
    pub git_tree: Option<String>,
    pub dirty_status_hash: Option<String>,
    pub verification_attribution: Option<String>,
    pub degraded_codes: Vec<String>,
    pub error_codes: Vec<String>,
    pub environment_blocker_codes: Vec<String>,
    #[serde(skip_serializing_if = "FirstError::is_empty")]
    pub first_error: FirstError,
    pub raw_status: Option<String>,
}

/// Errors returned by the parser entry points.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// Input was not a JSON object.
    NotAnObject,
    /// Top-level `schema` field is missing or not a string.
    MissingSchema,
    /// Top-level `schema` field does not match the expected tag.
    UnexpectedSchema {
        found: String,
        expected: &'static str,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => formatter.write_str("evidence input is not a JSON object"),
            Self::MissingSchema => {
                formatter.write_str("evidence input is missing a `schema` field")
            }
            Self::UnexpectedSchema { found, expected } => write!(
                formatter,
                "evidence schema mismatch: found `{found}`, expected `{expected}`"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Domain knowledge: which `degraded_codes` from `ee.rch.verify.v1` count
/// as environment blockers vs surfacable code failures. The set is the
/// subset the active swarm has actually been seeing.
const ENVIRONMENT_BLOCKER_DEGRADED_CODES: &[&str] = &[
    "rch_verify_build_admission_unavailable",
    "rch_verify_cargo_path_dependency_version_blocked",
    "rch_verify_topology_blocked",
    "rch_verify_client_daemon_version_skew",
    "rch_verify_worker_disk_full",
    "rch_verify_worker_health_threshold_blocked",
    "rch_verify_remote_transport_timeout",
    "rch_verify_local_fallback_refused",
    "rch_verify_remote_marker_missing",
    "rch_verify_committed_tree_path_deps_unsupported",
    "rch_verify_committed_tree_unsupported",
    "rch_verify_dirty_tree_refused",
    "rch_verify_dirty_tracked_paths",
    "rch_verify_dirty_staged_paths",
    "rch_verify_dirty_unstaged_paths",
    "rch_verify_dirty_beads_metadata",
    "rch_verify_dirty_untracked_paths",
    "rch_verify_source_state_refused",
];

/// Parse a `ee.rch.verify.v1` proof JSON object into a normalized
/// envelope. Unknown fields are tolerated.
pub fn parse_rch_verify(raw: &Value) -> Result<VerificationEvidence, ParseError> {
    let object = raw.as_object().ok_or(ParseError::NotAnObject)?;
    let schema = object
        .get("schema")
        .and_then(Value::as_str)
        .ok_or(ParseError::MissingSchema)?;
    if schema != RCH_VERIFY_SCHEMA_V1 {
        return Err(ParseError::UnexpectedSchema {
            found: schema.to_owned(),
            expected: RCH_VERIFY_SCHEMA_V1,
        });
    }

    let bead_id = string_field(object, "bead_id");
    let command_kind = string_field(object, "command_kind");
    let command_hash = string_field(object, "command_hash");
    let worker_id = string_field(object, "worker_id");
    let git_head = string_field(object, "git_head");
    let git_tree = string_field(object, "git_tree");
    let dirty_status_hash = string_field(object, "dirty_status_hash");
    let verification_attribution = string_field(object, "verification_attribution");
    let raw_status = string_field(object, "status");
    let exit_code = object.get("exit_code").and_then(Value::as_i64);
    let elapsed_ms = object
        .get("elapsed_ms")
        .and_then(Value::as_u64)
        .or_else(|| {
            object
                .get("elapsed_ms")
                .and_then(Value::as_f64)
                .map(|value| value.max(0.0) as u64)
        });

    let command_text = string_field(object, "command_text").or_else(|| {
        object
            .get("command")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty())
    });

    let degraded_codes = string_array(object, "degraded_codes");
    let error_codes = string_array(object, "error_codes");
    let source_state_degraded = string_array(object, "source_state_degraded_codes");
    let worker_state_degraded = string_array(object, "worker_state_degraded_codes");

    let mut combined_degraded = degraded_codes.clone();
    for code in source_state_degraded
        .iter()
        .chain(worker_state_degraded.iter())
    {
        if !combined_degraded.contains(code) {
            combined_degraded.push(code.clone());
        }
    }

    let environment_blocker_codes: Vec<String> = combined_degraded
        .iter()
        .filter(|code| ENVIRONMENT_BLOCKER_DEGRADED_CODES.contains(&code.as_str()))
        .cloned()
        .collect();

    let first_error = FirstError {
        file: string_field(object, "first_error_file"),
        line: object.get("first_error_line").and_then(Value::as_u64),
        message: string_field(object, "first_error_message"),
    };

    let status = classify_rch_status(
        raw_status.as_deref(),
        exit_code,
        &combined_degraded,
        &environment_blocker_codes,
        &first_error,
    );

    Ok(VerificationEvidence {
        schema: VERIFICATION_EVIDENCE_SCHEMA_V1,
        source: EvidenceSource::RchVerify,
        status,
        bead_id,
        command: command_text,
        command_kind,
        command_hash,
        worker_id,
        exit_code,
        elapsed_ms,
        git_head,
        git_tree,
        dirty_status_hash,
        verification_attribution,
        degraded_codes: combined_degraded,
        error_codes,
        environment_blocker_codes,
        first_error,
        raw_status,
    })
}

/// Parse an `ee.test_event.v1` command-tail row into the normalized
/// envelope. The parser accepts fields either at top level or under the
/// conventional `fields` object used by the e2e logger.
pub fn parse_verify_script_event(raw: &Value) -> Result<VerificationEvidence, ParseError> {
    let object = checked_object(raw, TEST_EVENT_SCHEMA_V1)?;
    let raw_status = string_field_any(object, &["status", "outcome", "kind"])
        .or_else(|| string_field_from_fields(object, "status"));
    let exit_code = i64_field_any(object, &["exit_code", "exitCode", "rc"]);
    let degraded_codes = combined_degraded_codes(object);
    let error_codes = string_arrays_any(object, &["error_codes", "errorCodes"]);
    let environment_blocker_codes = environment_blockers_from(object, &degraded_codes);
    let status =
        classify_generic_status(raw_status.as_deref(), exit_code, &environment_blocker_codes);

    Ok(VerificationEvidence {
        schema: VERIFICATION_EVIDENCE_SCHEMA_V1,
        source: EvidenceSource::VerifyScript,
        status,
        bead_id: string_field_any(object, &["bead_id", "beadId", "test_id", "testId"]),
        command: command_from_script_event(object),
        command_kind: string_field_any(object, &["command_kind", "commandKind", "kind", "phase"]),
        command_hash: string_field_any(object, &["command_hash", "commandHash"]),
        worker_id: string_field_any(
            object,
            &[
                "worker_id",
                "workerId",
                "worker_host",
                "workerHost",
                "runner_name",
            ],
        ),
        exit_code,
        elapsed_ms: u64_field_any(object, &["elapsed_ms", "elapsedMs", "duration_ms"]),
        git_head: string_field_any(object, &["git_head", "gitHead", "head_sha", "headSha"]),
        git_tree: string_field_any(object, &["git_tree", "gitTree"]),
        dirty_status_hash: string_field_any(object, &["dirty_status_hash", "dirtyStatusHash"]),
        verification_attribution: string_field_any(
            object,
            &[
                "verification_attribution",
                "verificationAttribution",
                "agent_name",
            ],
        ),
        degraded_codes,
        error_codes,
        environment_blocker_codes,
        first_error: first_error_from_fields(object),
        raw_status,
    })
}

/// Parse a canonical GitHub Actions check-run/job summary. This accepts
/// compact summaries exported by `gh api` adapters, not the full nested
/// GitHub API payload.
pub fn parse_github_actions_job(raw: &Value) -> Result<VerificationEvidence, ParseError> {
    let object = checked_object(raw, GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1)?;
    let raw_status = string_field_any(object, &["conclusion", "status", "outcome"]);
    let exit_code = i64_field_any(object, &["exit_code", "exitCode"]);
    let degraded_codes = combined_degraded_codes(object);
    let error_codes = string_arrays_any(object, &["error_codes", "errorCodes"]);
    let environment_blocker_codes = environment_blockers_from(object, &degraded_codes);
    let status =
        classify_github_status(raw_status.as_deref(), exit_code, &environment_blocker_codes);

    Ok(VerificationEvidence {
        schema: VERIFICATION_EVIDENCE_SCHEMA_V1,
        source: EvidenceSource::GitHubActionsJob,
        status,
        bead_id: string_field_any(object, &["bead_id", "beadId"]),
        command: github_action_label(object),
        command_kind: string_field_any(object, &["command_kind", "commandKind"])
            .or_else(|| Some("github_actions_check_run".to_owned())),
        command_hash: string_field_any(object, &["command_hash", "commandHash"]),
        worker_id: string_field_any(
            object,
            &["runner_name", "runnerName", "runner_id", "runnerId"],
        ),
        exit_code,
        elapsed_ms: u64_field_any(
            object,
            &["elapsed_ms", "elapsedMs", "run_duration_ms", "duration_ms"],
        ),
        git_head: string_field_any(object, &["head_sha", "headSha", "git_head", "gitHead"]),
        git_tree: string_field_any(object, &["git_tree", "gitTree"]),
        dirty_status_hash: string_field_any(object, &["dirty_status_hash", "dirtyStatusHash"]),
        verification_attribution: string_field_any(
            object,
            &[
                "verification_attribution",
                "verificationAttribution",
                "html_url",
                "htmlUrl",
            ],
        ),
        degraded_codes,
        error_codes,
        environment_blocker_codes,
        first_error: first_error_from_fields(object),
        raw_status,
    })
}

/// Parse a local static-only proof record. Static checks are deliberately
/// parser-only here: the normalizer never launches `rustfmt`, `clippy`,
/// UBS, or Git itself.
pub fn parse_static_check(raw: &Value) -> Result<VerificationEvidence, ParseError> {
    let object = checked_object(raw, STATIC_CHECK_SCHEMA_V1)?;
    let raw_status = string_field_any(object, &["status", "outcome"]);
    let exit_code = i64_field_any(object, &["exit_code", "exitCode", "rc"]);
    let degraded_codes = combined_degraded_codes(object);
    let error_codes = string_arrays_any(object, &["error_codes", "errorCodes"]);
    let environment_blocker_codes = environment_blockers_from(object, &degraded_codes);
    let status =
        classify_generic_status(raw_status.as_deref(), exit_code, &environment_blocker_codes);

    Ok(VerificationEvidence {
        schema: VERIFICATION_EVIDENCE_SCHEMA_V1,
        source: EvidenceSource::StaticCheck,
        status,
        bead_id: string_field_any(object, &["bead_id", "beadId"]),
        command: command_from_script_event(object),
        command_kind: string_field_any(
            object,
            &[
                "command_kind",
                "commandKind",
                "check_kind",
                "checkKind",
                "tool",
            ],
        ),
        command_hash: string_field_any(object, &["command_hash", "commandHash"]),
        worker_id: string_field_any(object, &["worker_id", "workerId", "host", "runner_name"]),
        exit_code,
        elapsed_ms: u64_field_any(object, &["elapsed_ms", "elapsedMs", "duration_ms"]),
        git_head: string_field_any(object, &["git_head", "gitHead"]),
        git_tree: string_field_any(object, &["git_tree", "gitTree"]),
        dirty_status_hash: string_field_any(object, &["dirty_status_hash", "dirtyStatusHash"]),
        verification_attribution: string_field_any(
            object,
            &["verification_attribution", "verificationAttribution"],
        ),
        degraded_codes,
        error_codes,
        environment_blocker_codes,
        first_error: first_error_from_fields(object),
        raw_status,
    })
}

fn classify_rch_status(
    raw_status: Option<&str>,
    exit_code: Option<i64>,
    degraded: &[String],
    environment_blockers: &[String],
    first_error: &FirstError,
) -> EvidenceStatus {
    if !environment_blockers.is_empty() {
        return EvidenceStatus::EnvironmentBlocked;
    }
    match raw_status {
        Some("remote_pass") => EvidenceStatus::Passed,
        Some("rch_environment_failure") => EvidenceStatus::EnvironmentBlocked,
        Some("source_state_refused") => EvidenceStatus::EnvironmentBlocked,
        Some("remote_failure") => {
            // `remote_failure` with no degraded codes and a first-error
            // line is a real code failure. With env-shaped degraded
            // codes (already handled above) it would have been
            // classified as environment-blocked already.
            if !first_error.is_empty() {
                EvidenceStatus::FailedInCode
            } else if degraded
                .iter()
                .any(|code| code == "rch_verify_remote_command_failed" && first_error.is_empty())
            {
                EvidenceStatus::FailedInCode
            } else {
                EvidenceStatus::FailedInCode
            }
        }
        Some(_) => {
            if exit_code == Some(0) {
                EvidenceStatus::Passed
            } else {
                EvidenceStatus::FailedInCode
            }
        }
        None => {
            if exit_code == Some(0) {
                EvidenceStatus::Passed
            } else {
                EvidenceStatus::FailedInCode
            }
        }
    }
}

fn classify_generic_status(
    raw_status: Option<&str>,
    exit_code: Option<i64>,
    environment_blockers: &[String],
) -> EvidenceStatus {
    if !environment_blockers.is_empty() {
        return EvidenceStatus::EnvironmentBlocked;
    }
    if let Some(raw) = raw_status {
        match normalized_status(raw).as_str() {
            "pass" | "passed" | "success" | "successful" | "ok" | "remote_pass" => {
                return EvidenceStatus::Passed;
            }
            "environment_blocked"
            | "blocked"
            | "skipped"
            | "cancelled"
            | "canceled"
            | "timed_out"
            | "timeout"
            | "action_required"
            | "queued"
            | "pending"
            | "in_progress"
            | "neutral"
            | "rch_environment_failure"
            | "source_state_refused" => {
                return EvidenceStatus::EnvironmentBlocked;
            }
            "fail" | "failed" | "failure" | "error" | "failed_in_code" | "remote_failure"
            | "assert_fail" | "test_failed" => return EvidenceStatus::FailedInCode,
            _ => {}
        }
    }
    match exit_code {
        Some(0) => EvidenceStatus::Passed,
        Some(_) => EvidenceStatus::FailedInCode,
        None => EvidenceStatus::FailedInCode,
    }
}

fn classify_github_status(
    raw_status: Option<&str>,
    exit_code: Option<i64>,
    environment_blockers: &[String],
) -> EvidenceStatus {
    if !environment_blockers.is_empty() {
        return EvidenceStatus::EnvironmentBlocked;
    }
    if let Some(raw) = raw_status {
        match normalized_status(raw).as_str() {
            "success" | "passed" | "pass" => return EvidenceStatus::Passed,
            "failure" | "failed" | "error" => return EvidenceStatus::FailedInCode,
            "cancelled" | "canceled" | "timed_out" | "timeout" | "action_required" | "skipped"
            | "neutral" | "queued" | "pending" | "in_progress" | "startup_failure" => {
                return EvidenceStatus::EnvironmentBlocked;
            }
            _ => {}
        }
    }
    classify_generic_status(raw_status, exit_code, environment_blockers)
}

fn normalized_status(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| match ch {
            '-' | ' ' => '_',
            _ => ch.to_ascii_lowercase(),
        })
        .collect()
}

fn checked_object<'a>(
    raw: &'a Value,
    expected_schema: &'static str,
) -> Result<&'a serde_json::Map<String, Value>, ParseError> {
    let object = raw.as_object().ok_or(ParseError::NotAnObject)?;
    let schema = object
        .get("schema")
        .and_then(Value::as_str)
        .ok_or(ParseError::MissingSchema)?;
    if schema != expected_schema {
        return Err(ParseError::UnexpectedSchema {
            found: schema.to_owned(),
            expected: expected_schema,
        });
    }
    Ok(object)
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

fn string_array(object: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn nested_fields(
    object: &serde_json::Map<String, Value>,
) -> Option<&serde_json::Map<String, Value>> {
    object.get("fields").and_then(Value::as_object)
}

fn value_field<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a Value> {
    object
        .get(key)
        .or_else(|| nested_fields(object).and_then(|fields| fields.get(key)))
}

fn value_field_any<'a>(
    object: &'a serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Value> {
    keys.iter().find_map(|key| value_field(object, key))
}

fn string_field_from_fields(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    nested_fields(object)
        .and_then(|fields| fields.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

fn string_field_any(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value_field(object, key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    })
}

fn i64_field_any(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<i64> {
    value_field_any(object, keys).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
            .or_else(|| value.as_f64().map(|n| n as i64))
            .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
    })
}

fn u64_field_any(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    value_field_any(object, keys).and_then(|value| {
        value.as_u64().or_else(|| {
            value
                .as_f64()
                .map(|n| n.max(0.0) as u64)
                .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
    })
}

fn string_arrays_any(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for key in keys {
        for source in [
            object.get(*key),
            nested_fields(object).and_then(|fields| fields.get(*key)),
        ] {
            if let Some(array) = source.and_then(Value::as_array) {
                for value in array.iter().filter_map(Value::as_str) {
                    push_unique(&mut values, value.to_owned());
                }
            }
        }
    }
    values
}

fn combined_degraded_codes(object: &serde_json::Map<String, Value>) -> Vec<String> {
    string_arrays_any(
        object,
        &[
            "degraded_codes",
            "degradedCodes",
            "source_state_degraded_codes",
            "sourceStateDegradedCodes",
            "worker_state_degraded_codes",
            "workerStateDegradedCodes",
        ],
    )
}

fn environment_blockers_from(
    object: &serde_json::Map<String, Value>,
    degraded_codes: &[String],
) -> Vec<String> {
    let mut blockers = string_arrays_any(
        object,
        &["environment_blocker_codes", "environmentBlockerCodes"],
    );
    for code in degraded_codes {
        if ENVIRONMENT_BLOCKER_DEGRADED_CODES.contains(&code.as_str()) {
            push_unique(&mut blockers, code.clone());
        }
    }
    blockers
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn first_error_from_fields(object: &serde_json::Map<String, Value>) -> FirstError {
    let first_error_object = value_field(object, "first_error")
        .or_else(|| value_field(object, "firstError"))
        .and_then(Value::as_object);
    let object_value = |key: &str| first_error_object.and_then(|fields| fields.get(key));

    FirstError {
        file: string_field_any(object, &["first_error_file", "firstErrorFile"]).or_else(|| {
            object_value("file")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|s| !s.is_empty())
        }),
        line: u64_field_any(object, &["first_error_line", "firstErrorLine"]).or_else(|| {
            object_value("line").and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
        }),
        message: string_field_any(object, &["first_error_message", "firstErrorMessage"]).or_else(
            || {
                object_value("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty())
            },
        ),
    }
}

fn command_from_script_event(object: &serde_json::Map<String, Value>) -> Option<String> {
    string_field_any(
        object,
        &["command_text", "commandText", "command_line", "commandLine"],
    )
    .or_else(|| value_field_any(object, &["args", "argv"]).and_then(command_from_array))
    .or_else(|| string_field_any(object, &["command"]))
}

fn command_from_array(value: &Value) -> Option<String> {
    let joined = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

fn github_action_label(object: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(command) = string_field_any(object, &["command"]) {
        return Some(command);
    }
    if let Some(workflow) = string_field_any(object, &["workflow", "workflow_name", "workflowName"])
    {
        let job = string_field_any(object, &["job", "job_name", "jobName", "name"]);
        return Some(match job {
            Some(job) => format!("{workflow} / {job}"),
            None => workflow,
        });
    }
    string_field_any(object, &["name"])
}

/// Render a Beads-ready Markdown bullet list summary. The format matches
/// the style other agents already paste into bead comments so consumers
/// (closeout playbook, completion-audit) can read either prose-by-human or
/// machine-emitted entries uniformly.
#[must_use]
pub fn compact_summary(evidence: &VerificationEvidence) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Verification evidence: `{}` => `{}`.",
        inline_code(
            evidence
                .command
                .as_deref()
                .unwrap_or(evidence.command_kind.as_deref().unwrap_or("(unspecified)"))
        ),
        evidence.status.as_str()
    );
    if let Some(bead) = evidence.bead_id.as_deref() {
        let _ = writeln!(out, "- bead_id: `{}`", inline_code(bead));
    }
    let _ = writeln!(out, "- source: `{}`", evidence.source.as_str());
    if let Some(raw_status) = evidence.raw_status.as_deref() {
        let _ = writeln!(out, "- raw_status: `{}`", inline_code(raw_status));
    }
    if let Some(kind) = evidence.command_kind.as_deref() {
        let _ = writeln!(out, "- command_kind: `{}`", inline_code(kind));
    }
    if let Some(hash) = evidence.command_hash.as_deref() {
        let _ = writeln!(out, "- command_hash: `{}`", inline_code(hash));
    }
    if let Some(worker) = evidence.worker_id.as_deref() {
        let _ = writeln!(out, "- worker_id: `{}`", inline_code(worker));
    }
    if let Some(exit) = evidence.exit_code {
        let _ = writeln!(out, "- exit_code: `{exit}`");
    }
    if let Some(ms) = evidence.elapsed_ms {
        let _ = writeln!(out, "- elapsed_ms: `{ms}`");
    }
    if let Some(attrib) = evidence.verification_attribution.as_deref() {
        let _ = writeln!(out, "- verification_attribution: `{}`", inline_code(attrib));
    }
    if !evidence.environment_blocker_codes.is_empty() {
        let _ = writeln!(
            out,
            "- environment_blocker_codes: {}",
            backtick_list(&evidence.environment_blocker_codes)
        );
    }
    if !evidence.error_codes.is_empty() {
        let _ = writeln!(
            out,
            "- error_codes: {}",
            backtick_list(&evidence.error_codes)
        );
    }
    if !evidence.degraded_codes.is_empty() {
        let _ = writeln!(
            out,
            "- degraded_codes: {}",
            backtick_list(&evidence.degraded_codes)
        );
    }
    if let Some(file) = evidence.first_error.file.as_deref() {
        let line = evidence
            .first_error
            .line
            .map(|n| format!(":{n}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "- first_error: `{}`",
            inline_code(&format!("{file}{line}"))
        );
    }
    out
}

fn backtick_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("`{}`", inline_code(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn inline_code(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| match ch {
            '`' | '$' | '(' | ')' | '<' | '>' | '|' | ';' | '&' | '\n' | '\r' => '_',
            _ if ch.is_control() => '_',
            _ => ch,
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "(empty)".to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn passing_rch_proof() -> Value {
        json!({
            "schema": "ee.rch.verify.v1",
            "bead_id": "bd-2mey5",
            "command_text": "cargo check --lib",
            "command_kind": "cargo_check",
            "command_hash": "abc123",
            "worker_id": "vmi1149989",
            "exit_code": 0,
            "elapsed_ms": 11000,
            "status": "remote_pass",
            "degraded_codes": [],
            "verification_attribution": "local_checkout_observed_remote_source_unknown"
        })
    }

    #[test]
    fn parse_rch_verify_passing_proof_normalizes_to_passed_status() {
        let evidence = parse_rch_verify(&passing_rch_proof()).expect("parses");
        assert_eq!(evidence.source, EvidenceSource::RchVerify);
        assert_eq!(evidence.status, EvidenceStatus::Passed);
        assert_eq!(evidence.bead_id.as_deref(), Some("bd-2mey5"));
        assert_eq!(evidence.command.as_deref(), Some("cargo check --lib"));
        assert_eq!(evidence.command_kind.as_deref(), Some("cargo_check"));
        assert_eq!(evidence.command_hash.as_deref(), Some("abc123"));
        assert_eq!(evidence.worker_id.as_deref(), Some("vmi1149989"));
        assert_eq!(evidence.exit_code, Some(0));
        assert_eq!(evidence.elapsed_ms, Some(11000));
        assert!(evidence.degraded_codes.is_empty());
        assert!(evidence.environment_blocker_codes.is_empty());
        assert!(evidence.first_error.is_empty());
    }

    #[test]
    fn parse_rch_verify_classifies_path_dep_version_skew_as_environment_blocked() {
        // Real-world shape from worker vmi1149989 on bd-2mey5 RCH probe at
        // 2026-05-19T03:03Z.
        let evidence = parse_rch_verify(&json!({
            "schema": "ee.rch.verify.v1",
            "bead_id": "bd-2mey5",
            "command_text": "cargo check --lib",
            "command_kind": "cargo_check",
            "command_hash": "eae0cb5e0af81aca",
            "worker_id": "vmi1149989",
            "exit_code": 101,
            "elapsed_ms": 8229,
            "status": "rch_environment_failure",
            "degraded_codes": [
                "rch_verify_build_admission_unavailable",
                "rch_verify_remote_command_failed",
                "rch_verify_cargo_path_dependency_version_blocked"
            ],
            "worker_state_degraded_codes": [
                "rch_verify_cargo_path_dependency_version_blocked"
            ],
            "verification_attribution": "local_checkout_observed_remote_source_unknown"
        }))
        .expect("parses");

        assert_eq!(evidence.status, EvidenceStatus::EnvironmentBlocked);
        assert_eq!(evidence.exit_code, Some(101));
        assert!(
            evidence
                .environment_blocker_codes
                .contains(&"rch_verify_cargo_path_dependency_version_blocked".to_owned())
        );
        assert!(
            evidence
                .environment_blocker_codes
                .contains(&"rch_verify_build_admission_unavailable".to_owned())
        );
        // worker_state_degraded_codes union-merged into degraded_codes:
        assert_eq!(
            evidence
                .degraded_codes
                .iter()
                .filter(|c| c.as_str() == "rch_verify_cargo_path_dependency_version_blocked")
                .count(),
            1
        );
    }

    #[test]
    fn parse_rch_verify_classifies_topology_refusal_as_environment_blocked() {
        // Shape mirrors tests/fixtures/rch_verify_control_plane/topology_refusal.json.
        let evidence = parse_rch_verify(&json!({
            "schema": "ee.rch.verify.v1",
            "command_text": "cargo test --test rch_verify_contract",
            "command_kind": "cargo_test",
            "command_hash": "cd825533cce8c288",
            "exit_code": 1,
            "elapsed_ms": 42,
            "status": "rch_environment_failure",
            "error_codes": ["RCH-E327"],
            "degraded_codes": [
                "rch_verify_remote_command_failed",
                "rch_verify_topology_blocked",
                "rch_verify_local_fallback_refused",
                "rch_verify_remote_marker_missing"
            ]
        }))
        .expect("parses");

        assert_eq!(evidence.status, EvidenceStatus::EnvironmentBlocked);
        assert!(
            evidence
                .environment_blocker_codes
                .contains(&"rch_verify_topology_blocked".to_owned())
        );
        assert_eq!(evidence.error_codes, vec!["RCH-E327".to_owned()]);
    }

    #[test]
    fn parse_rch_verify_classifies_remote_compile_error_as_code_failure() {
        let evidence = parse_rch_verify(&json!({
            "schema": "ee.rch.verify.v1",
            "command_text": "cargo test --lib ppr_proof -- --nocapture",
            "command_kind": "cargo_test",
            "command_hash": "abc123",
            "worker_id": "trj",
            "exit_code": 101,
            "elapsed_ms": 5000,
            "status": "remote_failure",
            "first_error_file": "/data/projects/eidetic_engine_cli/src/db/mod.rs",
            "first_error_line": 431,
            "first_error_message": "expected struct, got enum",
            "degraded_codes": ["rch_verify_remote_command_failed"]
        }))
        .expect("parses");

        assert_eq!(evidence.status, EvidenceStatus::FailedInCode);
        assert_eq!(
            evidence.first_error.file.as_deref(),
            Some("/data/projects/eidetic_engine_cli/src/db/mod.rs")
        );
        assert_eq!(evidence.first_error.line, Some(431));
        assert!(evidence.environment_blocker_codes.is_empty());
    }

    #[test]
    fn parse_rch_verify_command_array_collapses_to_string() {
        let evidence = parse_rch_verify(&json!({
            "schema": "ee.rch.verify.v1",
            "command": ["cargo", "check", "--all-targets"],
            "status": "remote_pass",
            "exit_code": 0
        }))
        .expect("parses");
        assert_eq!(
            evidence.command.as_deref(),
            Some("cargo check --all-targets")
        );
    }

    #[test]
    fn parse_rch_verify_rejects_non_object_input() {
        let error = parse_rch_verify(&json!("not an object")).unwrap_err();
        assert_eq!(error, ParseError::NotAnObject);
    }

    #[test]
    fn parse_rch_verify_rejects_input_missing_schema() {
        let error = parse_rch_verify(&json!({"command_kind": "cargo_check"})).unwrap_err();
        assert_eq!(error, ParseError::MissingSchema);
    }

    #[test]
    fn parse_rch_verify_rejects_input_with_wrong_schema() {
        let error = parse_rch_verify(&json!({"schema": "ee.test_event.v1"})).unwrap_err();
        match error {
            ParseError::UnexpectedSchema { found, expected } => {
                assert_eq!(found, "ee.test_event.v1");
                assert_eq!(expected, "ee.rch.verify.v1");
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn compact_summary_renders_environment_blocker_lines() {
        let evidence = parse_rch_verify(&json!({
            "schema": "ee.rch.verify.v1",
            "bead_id": "bd-1nxz4.5",
            "command_text": "cargo check --lib",
            "command_kind": "cargo_check",
            "command_hash": "deadbeef",
            "worker_id": "css",
            "exit_code": 101,
            "elapsed_ms": 8229,
            "status": "rch_environment_failure",
            "verification_attribution": "committed_tree",
            "degraded_codes": ["rch_verify_cargo_path_dependency_version_blocked"]
        }))
        .expect("parses");

        let summary = compact_summary(&evidence);
        assert!(
            summary.contains("`rch_environment_failure`"),
            "summary should include raw status: {summary}"
        );
        assert!(
            summary.contains("environment_blocker_codes"),
            "summary should label env blockers: {summary}"
        );
        assert!(
            summary.contains("rch_verify_cargo_path_dependency_version_blocked"),
            "summary should mention specific blocker: {summary}"
        );
        assert!(
            summary.contains("- bead_id: `bd-1nxz4.5`"),
            "summary should include bead id: {summary}"
        );
    }

    #[test]
    fn evidence_status_as_str_is_snake_case() {
        assert_eq!(EvidenceStatus::Passed.as_str(), "passed");
        assert_eq!(EvidenceStatus::FailedInCode.as_str(), "failed_in_code");
        assert_eq!(
            EvidenceStatus::EnvironmentBlocked.as_str(),
            "environment_blocked"
        );
        assert_eq!(EvidenceStatus::Malformed.as_str(), "malformed");
    }

    #[test]
    fn evidence_source_as_str_is_snake_case() {
        assert_eq!(EvidenceSource::RchVerify.as_str(), "rch_verify");
        assert_eq!(EvidenceSource::VerifyScript.as_str(), "verify_script");
        assert_eq!(
            EvidenceSource::GitHubActionsJob.as_str(),
            "github_actions_job"
        );
        assert_eq!(EvidenceSource::StaticCheck.as_str(), "static_check");
    }

    #[test]
    fn serialized_envelope_matches_camel_case_schema_field_names() {
        let evidence = parse_rch_verify(&passing_rch_proof()).expect("parses");
        let json = serde_json::to_value(&evidence).expect("serializes");
        let object = json.as_object().expect("object");
        for required in [
            "schema",
            "source",
            "status",
            "beadId",
            "command",
            "commandKind",
            "commandHash",
            "workerId",
            "exitCode",
            "elapsedMs",
            "degradedCodes",
            "errorCodes",
            "environmentBlockerCodes",
            "rawStatus",
        ] {
            assert!(
                object.contains_key(required),
                "missing camelCase field {required} in {json}"
            );
        }
        assert_eq!(
            object.get("schema").and_then(Value::as_str),
            Some(VERIFICATION_EVIDENCE_SCHEMA_V1)
        );
        assert_eq!(
            object.get("source").and_then(Value::as_str),
            Some("rch_verify")
        );
        assert_eq!(object.get("status").and_then(Value::as_str), Some("passed"));
    }
}
