//! Dispatch complete external reports without interpreting nested examples.
//!
//! JavaScript test reports and lint/type-checker reports have separate typed
//! validators. All of them retain the caller's independent completion gate.

use super::{
    CanonicalDiagnostic, CompilerOutput, EslintCoverage, MAX_COMPILER_RECORDS,
    MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES,
};

#[path = "cass_error_recall_lint_checkers.rs"]
mod lint;

pub(super) fn inspect(text: &str) -> Result<Option<CompilerOutput>, ()> {
    if let Some(output) = super::super::test_coverage::javascript::inspect(text)? {
        return Ok(Some(output));
    }
    lint::inspect(text)
}
