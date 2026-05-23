//! Contract coverage for `ee::cass::CassError::repair_hint` (bd-hzk96).
//!
//! Companion to bd-8s0ys (which pinned
//! `CassImportError::repair_hint` in
//! `tests/contracts/cass_import_error_repair_hint_contract.rs`).
//!
//! The inline tests in `src/cass/error.rs` only assert
//! `is_some()`/`is_none()` presence for each variant; they do not pin
//! the exact repair-hint string that downstream `ee.error.v2`
//! envelopes surface to operators. A silent reword inside the
//! `repair_hint` match arms would not be caught by any existing test.
//! This file freezes the per-variant vocabulary so any future edit
//! must touch this file too.
//!
//! Pinned strings (one assertion per variant):
//!   - InvalidBinary, BinaryNotFound, ContractMismatch return their
//!     hard-coded actionable strings.
//!   - Degraded passes through the inner `repair_hint` payload.
//!   - EmptyStdout, InvalidStdoutJson, Io, Runtime, Unknown return
//!     `None` (the human surface should fall back to
//!     `ee doctor --json`).

use std::path::PathBuf;

use ee::cass::CassError;

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
fn repair_hint_for_invalid_binary_points_to_allowlisted_executable() -> TestResult {
    let error = CassError::InvalidBinary {
        binary: PathBuf::from("/tmp/sketchy-cass"),
        reason: "outside allowlist".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &Some("set EE_CASS_BINARY or [cass.binary] to an absolute trusted cass executable"),
        "InvalidBinary repair_hint",
    )
}

#[test]
fn repair_hint_for_binary_not_found_points_to_install_or_config() -> TestResult {
    let error = CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    };
    ensure_equal(
        &error.repair_hint(),
        &Some("install cass or set [cass.binary] in config"),
        "BinaryNotFound repair_hint",
    )
}

#[test]
fn repair_hint_for_contract_mismatch_points_to_compatible_upgrade() -> TestResult {
    let error = CassError::ContractMismatch {
        required: "v3".to_string(),
        observed: "v2".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &Some("upgrade cass to a compatible contract version"),
        "ContractMismatch repair_hint",
    )
}

#[test]
fn repair_hint_for_degraded_passes_through_inner_payload() -> TestResult {
    // The Degraded variant carries its own actionable hint produced by
    // the cass adapter — the public repair_hint() must surface it
    // verbatim so operator dashboards do not get a generic message
    // when a specific one is available.
    let inner_hint = "cass index --full";
    let error = CassError::Degraded {
        kind: "stale_lexical_index".to_string(),
        repair_hint: inner_hint.to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &Some(inner_hint),
        "Degraded repair_hint passthrough",
    )
}

#[test]
fn repair_hint_for_empty_stdout_returns_none_so_caller_falls_back_to_doctor() -> TestResult {
    let error = CassError::EmptyStdout;
    ensure_equal(
        &error.repair_hint(),
        &None,
        "EmptyStdout repair_hint must be None",
    )
}

#[test]
fn repair_hint_for_invalid_stdout_json_returns_none() -> TestResult {
    let error = CassError::InvalidStdoutJson {
        hint: "trailing comma at line 14".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &None,
        "InvalidStdoutJson repair_hint must be None",
    )
}

#[test]
fn repair_hint_for_io_returns_none() -> TestResult {
    let error = CassError::Io {
        message: "permission denied".to_string(),
    };
    ensure_equal(&error.repair_hint(), &None, "Io repair_hint must be None")
}

#[test]
fn repair_hint_for_runtime_returns_none() -> TestResult {
    let error = CassError::Runtime {
        kind: "ingest_unavailable".to_string(),
        message: "ingest queue stalled".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &None,
        "Runtime repair_hint must be None",
    )
}

#[test]
fn repair_hint_for_unknown_returns_none() -> TestResult {
    let error = CassError::Unknown {
        kind: "future_kind_not_modelled".to_string(),
        message: "cass surfaced an unknown error envelope".to_string(),
    };
    ensure_equal(
        &error.repair_hint(),
        &None,
        "Unknown repair_hint must be None",
    )
}
