//! Ingest and query helpers for the RCH verifier evidence ledger (bd-17awb).
//!
//! Parses `ee.rch.verify.v1` proof JSON produced by `scripts/rch_verify.sh` (or
//! equivalent external tooling) into a `NormalizedRchVerifyRow` matching the
//! `rch_verify_runs` schema landed under V061 by bd-22p8c. Tails are bounded,
//! hashes are stripped of source-specific prefixes, and the canonical
//! 64-character hex constraint is enforced before any database write.

use std::error::Error;
use std::fmt;

use serde::Serialize;
use serde_json::Value as JsonValue;

pub const RCH_VERIFY_LEDGER_SCHEMA_V1: &str = "ee.rch.verify.v1";
pub const RCH_VERIFY_LEDGER_TAIL_MAX_BYTES: usize = 8192;
pub const RCH_VERIFY_LEDGER_COMMAND_TEXT_MAX_BYTES: usize = 4096;
pub const RCH_VERIFY_LEDGER_DEGRADED_JSON_MAX_BYTES: usize = 4096;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
