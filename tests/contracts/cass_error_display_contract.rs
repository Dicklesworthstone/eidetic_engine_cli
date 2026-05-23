//! Contract coverage for `ee::cass::CassError`'s `Display` impl
//! (bd-j3ydk).
//!
//! Companion to bd-hzk96 (which pinned `CassError::repair_hint`) and
//! bd-8s0ys (`CassImportError::repair_hint`).
//!
//! The inline `display_includes_kind_and_context` test in
//! `src/cass/error.rs` only verifies that the `Degraded` rendering
//! *contains* a substring; the remaining 8 variants' templates are
//! entirely unpinned. The Display strings flow through the
//! `ee.error.v2` envelope to operator surfaces, so a silent reword
//! ("cass produced no stdout payload" → "cass returned an empty
//! stdout") is part of the contract surface and must not slip through
//! unattributed.
//!
//! This file freezes the exact rendered string per variant so any
//! future edit to `impl Display for CassError` in `src/cass/error.rs`
//! must touch this file too.

use std::path::PathBuf;

use ee::cass::CassError;

type TestResult = Result<(), String>;

fn ensure_equal(actual: &str, expected: &str, context: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn display_for_invalid_binary_includes_path_and_reason() -> TestResult {
    let error = CassError::InvalidBinary {
        binary: PathBuf::from("/tmp/sketchy-cass"),
        reason: "outside allowlist".to_string(),
    };
    ensure_equal(
        &error.to_string(),
        "cass binary '/tmp/sketchy-cass' is not allowed: outside allowlist",
        "InvalidBinary Display",
    )
}

#[test]
fn display_for_binary_not_found_includes_path() -> TestResult {
    let error = CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    };
    ensure_equal(
        &error.to_string(),
        "cass binary not found at '/usr/local/bin/cass'",
        "BinaryNotFound Display",
    )
}

#[test]
fn display_for_io_includes_message() -> TestResult {
    let error = CassError::Io {
        message: "broken pipe".to_string(),
    };
    ensure_equal(
        &error.to_string(),
        "cass subprocess io error: broken pipe",
        "Io Display",
    )
}

#[test]
fn display_for_empty_stdout_is_fixed_string() -> TestResult {
    let error = CassError::EmptyStdout;
    ensure_equal(
        &error.to_string(),
        "cass produced no stdout payload",
        "EmptyStdout Display",
    )
}

#[test]
fn display_for_invalid_stdout_json_includes_hint() -> TestResult {
    let error = CassError::InvalidStdoutJson {
        hint: "expected '{' at line 1".to_string(),
    };
    ensure_equal(
        &error.to_string(),
        "cass stdout was not valid JSON: expected '{' at line 1",
        "InvalidStdoutJson Display",
    )
}

#[test]
fn display_for_contract_mismatch_includes_required_and_observed() -> TestResult {
    let error = CassError::ContractMismatch {
        required: "v3".to_string(),
        observed: "v2".to_string(),
    };
    ensure_equal(
        &error.to_string(),
        "cass contract mismatch: required v3, observed v2",
        "ContractMismatch Display",
    )
}

#[test]
fn display_for_degraded_includes_kind_and_repair_hint() -> TestResult {
    let error = CassError::Degraded {
        kind: "stale_lexical_index".to_string(),
        repair_hint: "cass index --full".to_string(),
    };
    ensure_equal(
        &error.to_string(),
        "cass reports degraded capability 'stale_lexical_index': cass index --full",
        "Degraded Display",
    )
}

#[test]
fn display_for_runtime_includes_kind_and_message() -> TestResult {
    let error = CassError::Runtime {
        kind: "session_not_found".to_string(),
        message: "no session with that id".to_string(),
    };
    ensure_equal(
        &error.to_string(),
        "cass runtime error 'session_not_found': no session with that id",
        "Runtime Display",
    )
}

#[test]
fn display_for_unknown_includes_kind_and_message() -> TestResult {
    let error = CassError::Unknown {
        kind: "future_kind_not_modelled".to_string(),
        message: "cass surfaced an unknown error envelope".to_string(),
    };
    ensure_equal(
        &error.to_string(),
        "cass reported unknown error kind 'future_kind_not_modelled': cass surfaced an unknown error envelope",
        "Unknown Display",
    )
}
