//! Contract coverage for `CassError::is_degraded` (bd-3atp9).
//!
//! The retrieval path keys on `is_degraded()` to decide whether to
//! keep degraded results or fail closed. Today the inline test
//! `degraded_is_the_only_recoverable_variant` in `src/cass/error.rs:245`
//! covers only 3 of 8 variants (Degraded, EmptyStdout, Runtime). The
//! other 5 (InvalidBinary, BinaryNotFound, Io, InvalidStdoutJson,
//! ContractMismatch, Unknown) have no direct pin. A silent edit to the
//! `matches!` pattern could quietly broaden or narrow the recoverable
//! set without surfacing in any test.

use std::path::PathBuf;

use ee::cass::CassError;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn is_degraded_is_true_for_degraded_variant() -> TestResult {
    let error = CassError::Degraded {
        kind: "stale_index".to_string(),
        repair_hint: "ee maintenance run --jobs index-refresh".to_string(),
    };
    ensure(
        error.is_degraded(),
        "CassError::Degraded must be the recoverable variant — retrieval path keeps these results",
    )
}

#[test]
fn is_degraded_is_false_for_invalid_binary() -> TestResult {
    let error = CassError::InvalidBinary {
        binary: PathBuf::from("/tmp/cass"),
        reason: "outside allowlist".to_string(),
    };
    ensure(
        !error.is_degraded(),
        "CassError::InvalidBinary must fail closed — caller cannot trust the path",
    )
}

#[test]
fn is_degraded_is_false_for_binary_not_found() -> TestResult {
    let error = CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    };
    ensure(
        !error.is_degraded(),
        "CassError::BinaryNotFound must fail closed — no fallback when the binary is missing",
    )
}

#[test]
fn is_degraded_is_false_for_io() -> TestResult {
    let error = CassError::Io {
        message: "broken pipe".to_string(),
    };
    ensure(
        !error.is_degraded(),
        "CassError::Io must fail closed — subprocess IO failure is not recoverable data",
    )
}

#[test]
fn is_degraded_is_false_for_empty_stdout() -> TestResult {
    let error = CassError::EmptyStdout;
    ensure(
        !error.is_degraded(),
        "CassError::EmptyStdout must fail closed — no payload means no degraded data either",
    )
}

#[test]
fn is_degraded_is_false_for_invalid_stdout_json() -> TestResult {
    let error = CassError::InvalidStdoutJson {
        hint: "unexpected token at line 1".to_string(),
    };
    ensure(
        !error.is_degraded(),
        "CassError::InvalidStdoutJson must fail closed — payload was emitted but is not parseable",
    )
}

#[test]
fn is_degraded_is_false_for_contract_mismatch() -> TestResult {
    let error = CassError::ContractMismatch {
        required: "api_version=1".to_string(),
        observed: "api_version=2".to_string(),
    };
    ensure(
        !error.is_degraded(),
        "CassError::ContractMismatch must fail closed — contract drift requires explicit upgrade",
    )
}

#[test]
fn is_degraded_is_false_for_runtime() -> TestResult {
    let error = CassError::Runtime {
        kind: "panic".to_string(),
        message: "cass subprocess crashed".to_string(),
    };
    ensure(
        !error.is_degraded(),
        "CassError::Runtime must fail closed — cass-reported runtime errors are not recoverable",
    )
}

#[test]
fn is_degraded_is_false_for_unknown() -> TestResult {
    let error = CassError::Unknown {
        kind: "new_kind".to_string(),
        message: "future cass error".to_string(),
    };
    ensure(
        !error.is_degraded(),
        "CassError::Unknown must fail closed — forward-compat for unrecognized errors must not be auto-recoverable",
    )
}
