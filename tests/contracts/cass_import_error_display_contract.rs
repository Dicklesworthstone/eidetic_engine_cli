//! Contract coverage for `ee::cass::CassImportError`'s `Display` impl
//! (bd-1sw5v).
//!
//! Pins the exact user-facing text rendered for each variant so a future
//! agent cannot edit the format strings in `src/cass/import.rs` (which feed
//! `ee.error.v2` envelopes and operator handoff text) without a contract
//! test surfacing the change.
//!
//! Sister contracts:
//! * `cass_error_display_contract` (bd-hzk96) pins the inner CassError text.
//! * `cass_import_error_repair_hint_contract` (bd-8s0ys) pins each variant's
//!   `repair_hint`.
//! * `cass_error_repair_hint_contract` (bd-hzk96) pins inner CassError hints.

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
fn display_for_cass_command_includes_command_exit_and_stderr() -> TestResult {
    let error = CassImportError::CassCommand {
        command: "cass view".to_string(),
        exit_code: Some(2),
        stderr: "missing --workspace flag".to_string(),
        timed_out: false,
        stderr_truncated: false,
        stdout_line_count: None,
        peak_stdout_line_bytes: None,
        peak_stdout_buffer_bytes: None,
    };
    ensure_equal(
        &format!("{error}"),
        &"cass command `cass view` failed with exit Some(2): missing --workspace flag".to_string(),
        "CassCommand Display",
    )
}

#[test]
fn display_for_cass_command_renders_none_exit_for_timeout() -> TestResult {
    let error = CassImportError::CassCommand {
        command: "cass view".to_string(),
        exit_code: None,
        stderr: "deadline exceeded".to_string(),
        timed_out: true,
        stderr_truncated: false,
        stdout_line_count: None,
        peak_stdout_line_bytes: None,
        peak_stdout_buffer_bytes: None,
    };
    ensure_equal(
        &format!("{error}"),
        &"cass command `cass view` failed with exit None: deadline exceeded".to_string(),
        "CassCommand Display (no exit code)",
    )
}

#[test]
fn display_for_invalid_json_names_source_and_message() -> TestResult {
    let error = CassImportError::InvalidJson {
        source: "cass view",
        message: "missing required field".to_string(),
    };
    ensure_equal(
        &format!("{error}"),
        &"invalid CASS cass view JSON: missing required field".to_string(),
        "InvalidJson Display",
    )
}

#[test]
fn display_for_invalid_since_quotes_value() -> TestResult {
    let error = CassImportError::InvalidSince {
        value: "yesterday".to_string(),
        message: "unrecognised duration".to_string(),
    };
    ensure_equal(
        &format!("{error}"),
        &"invalid --since value `yesterday`: unrecognised duration".to_string(),
        "InvalidSince Display",
    )
}

#[test]
fn display_for_io_includes_path_and_message() -> TestResult {
    let error = CassImportError::Io {
        path: PathBuf::from("/tmp/missing"),
        message: "permission denied".to_string(),
    };
    ensure_equal(
        &format!("{error}"),
        &"I/O error at /tmp/missing: permission denied".to_string(),
        "Io Display",
    )
}

#[test]
fn display_for_cass_delegates_to_inner_cass_error_display() -> TestResult {
    // Wrapper must surface the inner CassError verbatim. The inner Display
    // contract is pinned separately by bd-hzk96's cass_error_display_contract.
    let inner = CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    };
    let expected = format!("{inner}");
    let wrapped = CassImportError::Cass(inner);
    ensure_equal(
        &format!("{wrapped}"),
        &expected,
        "Cass(inner) Display equals inner Display",
    )
}

#[test]
fn display_for_storage_delegates_to_inner_db_error_display() -> TestResult {
    // Storage variant must surface the inner DbError verbatim — the wrapper
    // should not add a "storage error:" prefix or other framing.
    let inner = DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "synthetic storage error for Display contract".to_string(),
    };
    let expected = format!("{inner}");
    let wrapped = CassImportError::Storage(inner);
    ensure_equal(
        &format!("{wrapped}"),
        &expected,
        "Storage(inner) Display equals inner Display",
    )
}
