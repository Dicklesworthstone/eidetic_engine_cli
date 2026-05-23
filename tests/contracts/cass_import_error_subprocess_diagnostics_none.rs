//! Contract coverage for the `None` branch of
//! `CassImportError::subprocess_diagnostics_json()` (bd-26bbx).
//!
//! Negative complement of bd-6tl68 which pins the golden JSON for the
//! five variants that produce diagnostics
//! (CassCommand timeout/failure, Cass(CassError::Io) with cap_error,
//! invalid_utf8, and capture-cap messages). This file pins which
//! variants must return None so a silent flip from None to Some(_)
//! could not slip past existing coverage (e.g. emitting fake
//! diagnostics for InvalidJson would mislead the ee.error.v2 envelope
//! consumer about whether subprocess supervision actually fired).

use std::path::PathBuf;

use ee::cass::{CassError, CassImportError};
use ee::db::{DbError, DbOperation};

type TestResult = Result<(), String>;

fn ensure_none(label: &str, diagnostics: Option<serde_json::Value>) -> TestResult {
    if let Some(value) = diagnostics {
        return Err(format!(
            "{label} must NOT produce subprocess diagnostics; got {value:?}"
        ));
    }
    Ok(())
}

#[test]
fn invalid_json_variant_returns_none() -> TestResult {
    let error = CassImportError::InvalidJson {
        source: "cass view",
        message: "unexpected token at line 1".to_string(),
    };
    ensure_none(
        "CassImportError::InvalidJson",
        error.subprocess_diagnostics_json(),
    )
}

#[test]
fn invalid_since_variant_returns_none() -> TestResult {
    let error = CassImportError::InvalidSince {
        value: "yesterday".to_string(),
        message: "unrecognised duration".to_string(),
    };
    ensure_none(
        "CassImportError::InvalidSince",
        error.subprocess_diagnostics_json(),
    )
}

#[test]
fn outer_io_variant_returns_none() -> TestResult {
    // CassImportError::Io is the *outer* IO variant (with path) — it
    // covers ee-process filesystem failures around the workspace, not
    // cass subprocess IO. subprocess_diagnostics_json must reject it
    // even though the inner-CassError::Io variant routes through the
    // diagnostics path.
    let error = CassImportError::Io {
        path: PathBuf::from("/tmp/missing"),
        message: "permission denied".to_string(),
    };
    ensure_none(
        "CassImportError::Io (outer, with path)",
        error.subprocess_diagnostics_json(),
    )
}

#[test]
fn storage_variant_returns_none() -> TestResult {
    let error = CassImportError::Storage(DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "synthetic storage error".to_string(),
    });
    ensure_none(
        "CassImportError::Storage",
        error.subprocess_diagnostics_json(),
    )
}

#[test]
fn cass_inner_degraded_variant_returns_none() -> TestResult {
    // Cass(CassError::Io) routes through diagnostics, but Cass(non-Io)
    // variants do not. Pinning Cass(Degraded) covers the explicit
    // skip path for inner CassError variants other than Io.
    let error = CassImportError::Cass(CassError::Degraded {
        kind: "stale_index".to_string(),
        repair_hint: "ee maintenance run --jobs index-refresh".to_string(),
    });
    ensure_none(
        "CassImportError::Cass(CassError::Degraded)",
        error.subprocess_diagnostics_json(),
    )
}

#[test]
fn cass_inner_binary_not_found_variant_returns_none() -> TestResult {
    let error = CassImportError::Cass(CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    });
    ensure_none(
        "CassImportError::Cass(CassError::BinaryNotFound)",
        error.subprocess_diagnostics_json(),
    )
}

#[test]
fn cass_inner_empty_stdout_variant_returns_none() -> TestResult {
    let error = CassImportError::Cass(CassError::EmptyStdout);
    ensure_none(
        "CassImportError::Cass(CassError::EmptyStdout)",
        error.subprocess_diagnostics_json(),
    )
}

#[test]
fn cass_inner_io_with_unrelated_message_returns_none() -> TestResult {
    // Cass(CassError::Io) routes through cass_io_subprocess_diagnostics_json,
    // which returns None unless the message starts with "cass subprocess".
    // Pin that the routing function rejects unrelated io messages so a
    // future refactor cannot accidentally produce fake diagnostics for
    // ordinary IO failures that don't carry subprocess-supervision context.
    let error = CassImportError::Cass(CassError::Io {
        message: "broken pipe".to_string(),
    });
    ensure_none(
        "CassImportError::Cass(CassError::Io { message: \"broken pipe\" })",
        error.subprocess_diagnostics_json(),
    )
}
