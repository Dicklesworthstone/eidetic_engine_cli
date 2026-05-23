//! Contract coverage for `ee::cass::CassImportError::repair_hint` (bd-8s0ys).
//!
//! Pins the exact repair-hint string emitted per variant so downstream
//! `ee.error.v2` envelopes do not silently shift hint text under operators
//! when a future agent edits the match arms in
//! `src/cass/import.rs::CassImportError::repair_hint`.
//!
//! Existing src/cass/error.rs already covers `CassError::repair_hint` with
//! presence assertions. This file covers the `CassImportError` wrapper,
//! including its delegation to the inner `CassError` and the exact strings
//! attached to non-delegated variants.

use std::path::PathBuf;

use ee::cass::{CassError, CassImportError};
use ee::db::{DbError, DbOperation};

type TestResult = Result<(), String>;

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: &T,
    expected: &T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn repair_hint_for_cass_command_failure_is_run_cass_health_json() -> TestResult {
    let error = CassImportError::CassCommand {
        command: "cass view".to_string(),
        exit_code: Some(1),
        stderr: "some failure".to_string(),
        timed_out: false,
        stderr_truncated: false,
        stdout_line_count: None,
        peak_stdout_line_bytes: None,
        peak_stdout_buffer_bytes: None,
    };
    ensure_equal(
        &error.repair_hint(),
        &Some("run cass health --json"),
        "CassCommand repair_hint",
    )
}

#[test]
fn repair_hint_for_invalid_json_points_to_api_version_and_doctor() -> TestResult {
    let error = CassImportError::InvalidJson {
        source: "cass view",
        message: "missing required field".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &Some("run cass api-version --json and cass doctor --json"),
        "InvalidJson repair_hint",
    )
}

#[test]
fn repair_hint_for_invalid_since_points_to_duration_syntax() -> TestResult {
    let error = CassImportError::InvalidSince {
        value: "yesterday".to_string(),
        message: "unrecognised duration".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &Some("use --since with a duration like 90d, 24h, or 7d3h"),
        "InvalidSince repair_hint",
    )
}

#[test]
fn repair_hint_for_io_points_to_workspace_permissions() -> TestResult {
    let error = CassImportError::Io {
        path: PathBuf::from("/tmp/missing"),
        message: "permission denied".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &Some("check workspace and database path permissions"),
        "Io repair_hint",
    )
}

#[test]
fn repair_hint_for_storage_points_to_ee_init_repair_plan() -> TestResult {
    let error = CassImportError::Storage(DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "synthetic storage error for repair-hint contract".to_string(),
    });
    ensure_equal(
        &error.repair_hint(),
        &Some("ee init --workspace . --repair-plan"),
        "Storage repair_hint",
    )
}

#[test]
fn repair_hint_for_cass_delegates_to_inner_cass_error() -> TestResult {
    // Delegation contract: a Cass(inner) variant must surface the inner
    // CassError::repair_hint verbatim, including the `None` case for
    // unactionable inner variants.
    let actionable_inner = CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    };
    let expected_actionable = actionable_inner.repair_hint().map(str::to_owned);
    let actionable_wrapped = CassImportError::Cass(actionable_inner);
    ensure_equal(
        &actionable_wrapped.repair_hint().map(str::to_owned),
        &expected_actionable,
        "Cass(BinaryNotFound) delegation",
    )?;

    let inactionable_inner = CassError::EmptyStdout;
    let inactionable_wrapped = CassImportError::Cass(inactionable_inner);
    ensure_equal(
        &inactionable_wrapped.repair_hint(),
        &None,
        "Cass(EmptyStdout) delegation returns None",
    )
}
