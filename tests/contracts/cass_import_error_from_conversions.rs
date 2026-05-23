//! Contract coverage for `CassImportError` From conversions (bd-zp0dh).
//!
//! `src/cass/import.rs` provides two `From` impls used by every `?` call
//! inside `import_cass_sessions`:
//!
//! * `From<CassError>  -> CassImportError::Cass(error)`
//! * `From<DbError>    -> CassImportError::Storage(error)`
//!
//! These thin wrappers determine which downstream branch fires for
//! `repair_hint`, `subprocess_diagnostics_json`, and `ee.error.v2` rendering.
//! Silently re-mapping either conversion would change operator behavior
//! across ~30 call sites without surfacing in any current test.

use std::path::PathBuf;

use ee::cass::{CassError, CassImportError};
use ee::db::{DbError, DbOperation};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn from_cass_error_wraps_into_cass_variant() -> TestResult {
    let inner = CassError::BinaryNotFound {
        binary: PathBuf::from("/usr/local/bin/cass"),
    };
    let inner_debug = format!("{inner:?}");
    let wrapped: CassImportError = inner.into();
    match wrapped {
        CassImportError::Cass(ref captured) => ensure(
            format!("{captured:?}") == inner_debug,
            format!(
                "From<CassError> must preserve the inner CassError verbatim;\
                 \n--- expected\n{inner_debug}\n+++ got\n{captured:?}"
            ),
        ),
        other => Err(format!(
            "From<CassError> must produce CassImportError::Cass(_), got {other:?}"
        )),
    }
}

#[test]
fn from_db_error_wraps_into_storage_variant() -> TestResult {
    let inner = DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "synthetic storage error for From-impl contract".to_string(),
    };
    let inner_debug = format!("{inner:?}");
    let wrapped: CassImportError = inner.into();
    match wrapped {
        CassImportError::Storage(ref captured) => ensure(
            format!("{captured:?}") == inner_debug,
            format!(
                "From<DbError> must preserve the inner DbError verbatim;\
                 \n--- expected\n{inner_debug}\n+++ got\n{captured:?}"
            ),
        ),
        other => Err(format!(
            "From<DbError> must produce CassImportError::Storage(_), got {other:?}"
        )),
    }
}

#[test]
fn question_mark_operator_picks_up_from_impls() -> TestResult {
    // If the From impls were dropped or renamed, this closure would not
    // even compile. Keeping it as a runtime test ensures the conversion
    // surface stays usable in real `?` chains, not just direct `.into()`
    // calls.
    fn cass_path() -> Result<(), CassImportError> {
        let raw: Result<(), CassError> = Err(CassError::EmptyStdout);
        raw?;
        Ok(())
    }

    fn db_path() -> Result<(), CassImportError> {
        let raw: Result<(), DbError> = Err(DbError::MalformedRow {
            operation: DbOperation::Query,
            message: "synthetic storage error for question-mark contract".to_string(),
        });
        raw?;
        Ok(())
    }

    match cass_path() {
        Err(CassImportError::Cass(_)) => Ok::<(), String>(()),
        other => Err(format!(
            "?-operator over CassError must yield Cass(_), got {other:?}"
        )),
    }?;

    match db_path() {
        Err(CassImportError::Storage(_)) => Ok::<(), String>(()),
        other => Err(format!(
            "?-operator over DbError must yield Storage(_), got {other:?}"
        )),
    }?;

    Ok(())
}
