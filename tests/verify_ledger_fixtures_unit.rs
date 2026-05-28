//! Verify-ledger fixture and contract tests (bd-1rcy2).
//!
//! Covers the bd-17awb verify-ledger ingest/query surface with canonical
//! scrubbed fixtures that downstream contract gates and CLI smoke tests
//! can replay without requiring a live remote worker. The bead asks for:
//!
//!   * remote success
//!   * RCH-E327 topology/preflight blocker
//!   * capacity / no-worker blocker
//!   * local-fallback (advisory) detected
//!   * malformed verifier JSON (wrong schema id)
//!   * duplicate ingest
//!
//! Each fixture lives under `tests/fixtures/verify_ledger/` and is
//! exercised here through `ee::core::verify_ledger::{parse_rch_verify_v1,
//! ingest_rch_verify_v1, list_rch_verify_runs, list_rch_verify_blockers}`.
//!
//! Verification: cargo test --test verify_ledger_fixtures_unit must be
//! routed through RCH per AGENTS.md. Static jq parse on each fixture +
//! rustfmt are supplemental.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use ee::core::verify_ledger::{
    RCH_VERIFY_LEDGER_BLOCKERS_REPORT_SCHEMA_V1, RCH_VERIFY_LEDGER_INGEST_REPORT_SCHEMA_V1,
    RCH_VERIFY_LEDGER_RUNS_REPORT_SCHEMA_V1, RchVerifyLedgerError, RchVerifyLedgerParseError,
    ingest_rch_verify_v1, list_rch_verify_blockers, list_rch_verify_runs, parse_rch_verify_v1,
};
use ee::db::{CreateWorkspaceInput, DbConnection};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

const FIXTURE_REMOTE_SUCCESS: &str =
    include_str!("fixtures/verify_ledger/remote_cargo_test_success.json");
const FIXTURE_TOPOLOGY_BLOCKED: &str =
    include_str!("fixtures/verify_ledger/rch_e327_topology_blocked.json");
const FIXTURE_NO_WORKER_CAPACITY: &str =
    include_str!("fixtures/verify_ledger/rch_no_worker_capacity.json");
const FIXTURE_LOCAL_FALLBACK: &str =
    include_str!("fixtures/verify_ledger/local_fallback_detected.json");
const FIXTURE_WRONG_SCHEMA: &str =
    include_str!("fixtures/verify_ledger/malformed_envelope_wrong_schema.json");
const FIXTURE_DUPLICATE_SOURCE: &str =
    include_str!("fixtures/verify_ledger/duplicate_ingest_source.json");

const TEST_WORKSPACE_ID: &str = "wsp_verify_ledger_fixtures_unit";
const T_SUCCESS: &str = "2026-05-23T05:00:00Z";
const T_TOPOLOGY: &str = "2026-05-23T05:10:00Z";
const T_CAPACITY: &str = "2026-05-23T05:20:00Z";
const T_FALLBACK: &str = "2026-05-23T05:30:00Z";
const T_DUPLICATE: &str = "2026-05-23T05:40:00Z";
const T_NOW: &str = "2026-05-23T06:00:00Z";

fn parse_fixture(raw: &str) -> Result<JsonValue, String> {
    serde_json::from_str(raw).map_err(|error| format!("parse fixture JSON: {error}"))
}

fn connection_with_workspace() -> Result<DbConnection, String> {
    let connection =
        DbConnection::open_memory().map_err(|error| format!("open in-memory db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate in-memory db: {error}"))?;
    connection
        .insert_workspace(
            TEST_WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: "/tmp/ee-verify-ledger-fixtures-unit".to_owned(),
                name: Some("verify-ledger-fixtures-unit".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;
    Ok(connection)
}

fn ensure_64_hex(value: &str, context: &str) -> TestResult {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "{context} must be a 64-char hex string, got {value:?}"
        ));
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn fixture_files_are_under_versioned_directory() -> TestResult {
    let dir = repo_root()
        .join("tests")
        .join("fixtures")
        .join("verify_ledger");
    if !dir.is_dir() {
        return Err(format!(
            "tests/fixtures/verify_ledger must be a directory, got {}",
            dir.display()
        ));
    }
    for fixture in [
        "remote_cargo_test_success.json",
        "rch_e327_topology_blocked.json",
        "rch_no_worker_capacity.json",
        "local_fallback_detected.json",
        "malformed_envelope_wrong_schema.json",
        "duplicate_ingest_source.json",
    ] {
        let path = dir.join(fixture);
        if !path.is_file() {
            return Err(format!(
                "verify_ledger fixture {} is missing from {}",
                fixture,
                dir.display()
            ));
        }
    }
    Ok(())
}

#[test]
fn remote_success_fixture_parses_as_passed() -> TestResult {
    let value = parse_fixture(FIXTURE_REMOTE_SUCCESS)?;
    let row = parse_rch_verify_v1(&value).map_err(|error| format!("parse success: {error}"))?;
    if row.schema_id != "ee.rch.verify.v1" {
        return Err(format!("schema_id drifted: {}", row.schema_id));
    }
    if row.status != "passed" {
        return Err(format!(
            "remote-success fixture must classify as passed, got {}",
            row.status
        ));
    }
    if row.exit_code != Some(0) {
        return Err(format!(
            "exit_code must be Some(0), got {:?}",
            row.exit_code
        ));
    }
    if !row.remote_required {
        return Err("remote_required must be true on remote-success fixture".into());
    }
    if !row.degraded_codes.is_empty() {
        return Err(format!(
            "remote-success fixture must have empty degraded_codes, got {:?}",
            row.degraded_codes
        ));
    }
    if row.blocker_fingerprint.is_some() {
        return Err(format!(
            "remote-success fixture must not have a blocker_fingerprint, got {:?}",
            row.blocker_fingerprint
        ));
    }
    ensure_64_hex(&row.command_hash, "command_hash")?;
    ensure_64_hex(&row.source_state_hash, "source_state_hash")?;
    Ok(())
}

#[test]
fn rch_e327_topology_fixture_classifies_as_blocked_with_blocker_metadata() -> TestResult {
    let value = parse_fixture(FIXTURE_TOPOLOGY_BLOCKED)?;
    let row = parse_rch_verify_v1(&value).map_err(|error| format!("parse topology: {error}"))?;
    if row.status != "blocked" {
        return Err(format!(
            "topology fixture must classify as blocked, got {}",
            row.status
        ));
    }
    if !row
        .degraded_codes
        .iter()
        .any(|code| code == "rch_verify_topology_blocked")
    {
        return Err(format!(
            "topology fixture must surface rch_verify_topology_blocked, got {:?}",
            row.degraded_codes
        ));
    }
    if !row
        .degraded_codes
        .iter()
        .any(|code| code == "rch_verify_local_fallback_refused")
    {
        return Err(format!(
            "topology fixture must surface rch_verify_local_fallback_refused, got {:?}",
            row.degraded_codes
        ));
    }
    let fingerprint = row
        .blocker_fingerprint
        .as_deref()
        .ok_or("topology fixture must carry blocker_fingerprint")?;
    if !fingerprint.starts_with("sha256:") {
        return Err(format!(
            "blocker_fingerprint must keep its sha256: prefix, got {fingerprint:?}"
        ));
    }
    let remediation = row
        .remediation_bead
        .as_deref()
        .ok_or("topology fixture must name a remediation_bead")?;
    if remediation != "bd-17c65.10.17.1.2" {
        return Err(format!(
            "topology fixture must point to bd-17c65.10.17.1.2 remediation, got {remediation}"
        ));
    }
    if row.retry_after.is_none() {
        return Err("topology fixture must carry a retry_after timestamp".into());
    }
    Ok(())
}

#[test]
fn rch_no_worker_capacity_fixture_classifies_as_blocked() -> TestResult {
    let value = parse_fixture(FIXTURE_NO_WORKER_CAPACITY)?;
    let row = parse_rch_verify_v1(&value).map_err(|error| format!("parse capacity: {error}"))?;
    if row.status != "blocked" {
        return Err(format!(
            "no-worker-capacity fixture must classify as blocked, got {}",
            row.status
        ));
    }
    if !row
        .degraded_codes
        .iter()
        .any(|code| code == "rch_verify_no_worker_capacity")
    {
        return Err(format!(
            "capacity fixture must surface rch_verify_no_worker_capacity, got {:?}",
            row.degraded_codes
        ));
    }
    if row.blocker_fingerprint.is_none() {
        return Err("capacity fixture must carry blocker_fingerprint".into());
    }
    Ok(())
}

#[test]
fn local_fallback_fixture_classifies_as_fallback_detected() -> TestResult {
    let value = parse_fixture(FIXTURE_LOCAL_FALLBACK)?;
    let row = parse_rch_verify_v1(&value).map_err(|error| format!("parse fallback: {error}"))?;
    if row.status != "fallback_detected" {
        return Err(format!(
            "local-fallback fixture must classify as fallback_detected, got {}",
            row.status
        ));
    }
    if !row
        .degraded_codes
        .iter()
        .any(|code| code == "rch_verify_local_fallback_detected")
    {
        return Err(format!(
            "local-fallback fixture must surface rch_verify_local_fallback_detected, got {:?}",
            row.degraded_codes
        ));
    }
    Ok(())
}

#[test]
fn malformed_envelope_fixture_returns_unexpected_schema_error() -> TestResult {
    let value = parse_fixture(FIXTURE_WRONG_SCHEMA)?;
    match parse_rch_verify_v1(&value) {
        Err(RchVerifyLedgerParseError::UnexpectedSchema { found }) => {
            if found != "ee.rch.verify.v0" {
                return Err(format!(
                    "expected UnexpectedSchema(\"ee.rch.verify.v0\"), got {found:?}"
                ));
            }
            Ok(())
        }
        Err(other) => Err(format!(
            "malformed fixture must fail with UnexpectedSchema, got {other:?}"
        )),
        Ok(row) => Err(format!(
            "malformed fixture must not parse successfully, got row {row:?}"
        )),
    }
}

#[test]
fn fixtures_do_not_leak_pids_paths_or_secrets() -> TestResult {
    for (label, raw) in [
        ("remote_success", FIXTURE_REMOTE_SUCCESS),
        ("topology", FIXTURE_TOPOLOGY_BLOCKED),
        ("capacity", FIXTURE_NO_WORKER_CAPACITY),
        ("fallback", FIXTURE_LOCAL_FALLBACK),
        ("malformed", FIXTURE_WRONG_SCHEMA),
        ("duplicate_source", FIXTURE_DUPLICATE_SOURCE),
    ] {
        let lowered = raw.to_ascii_lowercase();
        for forbidden in [
            "/users/",
            "/private/",
            "/var/folders/",
            "begin pgp",
            "begin rsa",
            "begin openssh",
            "begin certificate",
            "aws_secret_access_key",
            "ghp_",
            "github_pat_",
        ] {
            if lowered.contains(forbidden) {
                return Err(format!(
                    "{label} fixture leaks forbidden token {forbidden:?}"
                ));
            }
        }
        // Bare numeric PIDs (5+ digits) preceded by "pid" markers should not
        // appear; allow numbers inside hex hashes.
        for needle in ["pid=", "pid:", "process_id=", "process_id:"] {
            if lowered.contains(needle) {
                return Err(format!("{label} fixture leaks {needle:?}"));
            }
        }
    }
    Ok(())
}

#[test]
fn ingest_remote_success_then_topology_returns_active_blocker() -> TestResult {
    let connection = connection_with_workspace()?;

    let success = parse_fixture(FIXTURE_REMOTE_SUCCESS)?;
    let topology = parse_fixture(FIXTURE_TOPOLOGY_BLOCKED)?;

    let first = ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &success, T_SUCCESS)
        .map_err(|error| format!("ingest success fixture: {error}"))?;
    if first.schema != RCH_VERIFY_LEDGER_INGEST_REPORT_SCHEMA_V1 {
        return Err(format!("ingest schema drifted: {}", first.schema));
    }
    if first.outcome != "inserted" {
        return Err(format!(
            "first ingest must be inserted, got {}",
            first.outcome
        ));
    }
    if first.inserted_count != 1 || first.duplicate_count != 0 {
        return Err(format!(
            "first ingest counts drifted: inserted={}, duplicate={}",
            first.inserted_count, first.duplicate_count
        ));
    }

    let second = ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &topology, T_TOPOLOGY)
        .map_err(|error| format!("ingest topology fixture: {error}"))?;
    if second.outcome != "inserted" {
        return Err(format!(
            "topology ingest must be inserted, got {}",
            second.outcome
        ));
    }

    let runs = list_rch_verify_runs(&connection, TEST_WORKSPACE_ID, None, None, T_NOW)
        .map_err(|error| format!("list runs: {error}"))?;
    if runs.schema != RCH_VERIFY_LEDGER_RUNS_REPORT_SCHEMA_V1 {
        return Err(format!("runs schema drifted: {}", runs.schema));
    }
    if runs.run_count != 2 {
        return Err(format!(
            "expected 2 runs after two ingests, got {}",
            runs.run_count
        ));
    }

    let blockers = list_rch_verify_blockers(&connection, TEST_WORKSPACE_ID, None, T_NOW)
        .map_err(|error| format!("list blockers: {error}"))?;
    if blockers.schema != RCH_VERIFY_LEDGER_BLOCKERS_REPORT_SCHEMA_V1 {
        return Err(format!("blockers schema drifted: {}", blockers.schema));
    }
    if blockers.blocker_count == 0 {
        return Err("active blockers list must include the topology row".into());
    }
    let topology_active = blockers.blockers.iter().any(|run| {
        run.status == "blocked" && run.remediation_bead.as_deref() == Some("bd-17c65.10.17.1.2")
    });
    if !topology_active {
        return Err(format!(
            "active blockers must include the bd-17c65.10.17.1.2 topology row, got {:?}",
            blockers
                .blockers
                .iter()
                .map(|r| &r.status)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn duplicate_ingest_is_idempotent_and_increments_duplicate_count() -> TestResult {
    let connection = connection_with_workspace()?;
    let source = parse_fixture(FIXTURE_DUPLICATE_SOURCE)?;

    let first = ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &source, T_DUPLICATE)
        .map_err(|error| format!("first ingest: {error}"))?;
    if first.outcome != "inserted" || first.inserted_count != 1 {
        return Err(format!(
            "first ingest of duplicate-source fixture must insert, got outcome={} inserted={}",
            first.outcome, first.inserted_count
        ));
    }

    let second = ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &source, T_DUPLICATE)
        .map_err(|error| format!("second ingest: {error}"))?;
    if second.outcome != "duplicate" {
        return Err(format!(
            "second ingest must report duplicate, got {}",
            second.outcome
        ));
    }
    if second.duplicate_count != 1 || second.inserted_count != 0 {
        return Err(format!(
            "duplicate ingest counts drifted: inserted={}, duplicate={}",
            second.inserted_count, second.duplicate_count
        ));
    }
    if second.run.id != first.run.id {
        return Err(format!(
            "duplicate ingest must return the same run id, got {} vs {}",
            first.run.id, second.run.id
        ));
    }

    let runs = list_rch_verify_runs(&connection, TEST_WORKSPACE_ID, None, None, T_NOW)
        .map_err(|error| format!("list runs after duplicate: {error}"))?;
    if runs.run_count != 1 {
        return Err(format!(
            "duplicate ingest must not create a second row, got run_count={}",
            runs.run_count
        ));
    }
    Ok(())
}

#[test]
fn malformed_fixture_ingest_fails_without_inserting_rows() -> TestResult {
    let connection = connection_with_workspace()?;
    let malformed = parse_fixture(FIXTURE_WRONG_SCHEMA)?;
    match ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &malformed, T_NOW) {
        Err(RchVerifyLedgerError::Parse(RchVerifyLedgerParseError::UnexpectedSchema { found })) => {
            if found != "ee.rch.verify.v0" {
                return Err(format!(
                    "expected UnexpectedSchema(ee.rch.verify.v0), got {found:?}"
                ));
            }
        }
        Err(other) => {
            return Err(format!(
                "malformed fixture ingest must fail with parse error, got {other:?}"
            ));
        }
        Ok(report) => {
            return Err(format!(
                "malformed fixture must not produce an ingest report, got {report:?}"
            ));
        }
    }
    let runs = list_rch_verify_runs(&connection, TEST_WORKSPACE_ID, None, None, T_NOW)
        .map_err(|error| format!("list runs after malformed: {error}"))?;
    if runs.run_count != 0 {
        return Err(format!(
            "malformed ingest must not insert any rows, got run_count={}",
            runs.run_count
        ));
    }
    Ok(())
}

#[test]
fn full_lifecycle_orders_runs_deterministically() -> TestResult {
    let connection = connection_with_workspace()?;
    let success = parse_fixture(FIXTURE_REMOTE_SUCCESS)?;
    let topology = parse_fixture(FIXTURE_TOPOLOGY_BLOCKED)?;
    let capacity = parse_fixture(FIXTURE_NO_WORKER_CAPACITY)?;
    let fallback = parse_fixture(FIXTURE_LOCAL_FALLBACK)?;

    ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &success, T_SUCCESS)
        .map_err(|error| format!("ingest success: {error}"))?;
    ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &topology, T_TOPOLOGY)
        .map_err(|error| format!("ingest topology: {error}"))?;
    ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &capacity, T_CAPACITY)
        .map_err(|error| format!("ingest capacity: {error}"))?;
    ingest_rch_verify_v1(&connection, TEST_WORKSPACE_ID, &fallback, T_FALLBACK)
        .map_err(|error| format!("ingest fallback: {error}"))?;

    // Two back-to-back queries must return the same canonical ordering so
    // CLI consumers and work-packet collectors never disagree.
    let first = list_rch_verify_runs(&connection, TEST_WORKSPACE_ID, None, None, T_NOW)
        .map_err(|error| format!("first list: {error}"))?;
    let second = list_rch_verify_runs(&connection, TEST_WORKSPACE_ID, None, None, T_NOW)
        .map_err(|error| format!("second list: {error}"))?;
    let first_ids: Vec<&str> = first.runs.iter().map(|run| run.id.as_str()).collect();
    let second_ids: Vec<&str> = second.runs.iter().map(|run| run.id.as_str()).collect();
    if first_ids != second_ids {
        return Err(format!(
            "list_rch_verify_runs ordering drifted across two reads:\nfirst:  {first_ids:?}\nsecond: {second_ids:?}"
        ));
    }
    if first.run_count != 4 {
        return Err(format!(
            "expected 4 runs after four distinct ingests, got run_count={}",
            first.run_count
        ));
    }

    let blockers = list_rch_verify_blockers(&connection, TEST_WORKSPACE_ID, None, T_NOW)
        .map_err(|error| format!("list blockers: {error}"))?;
    for blocker in &blockers.blockers {
        if blocker.status != "blocked" {
            return Err(format!(
                "list_rch_verify_blockers must only include blocked runs, got {} ({})",
                blocker.id, blocker.status
            ));
        }
    }
    Ok(())
}
