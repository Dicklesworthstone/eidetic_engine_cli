//! Contract coverage for `ImportSessionResult::source_path` (bd-1imsb).
//!
//! `ImportSessionResult` (src/cass/session.rs:860) is a pub enum with
//! three variants — `Imported`, `Skipped`, `Failed` — and a
//! `pub fn source_path(&self) -> &str` that extracts the embedded
//! `source_path` from each variant. The sibling `is_success` /
//! `is_failure` predicates are covered by an inline test
//! (`import_session_result_predicates`, src/cass/session.rs:1392), but
//! `source_path()` has zero coverage anywhere — no inline test, no
//! contract test, and no production callsite.
//!
//! Without a pin a future refactor could silently route
//! `Failed.error` into the `source_path()` return value (or skip a
//! variant in the match) without any test failing. This file freezes
//! the accessor contract for all three variants.
//!
//! Mirrors bd-w3iv0 / bd-2whz8 bounded-contract pin pattern:
//! deterministic, no fixtures, no new public API.

use ee::cass::ImportSessionResult;

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
fn imported_variant_returns_embedded_source_path() -> TestResult {
    let result = ImportSessionResult::Imported {
        source_path: "/tmp/imported-session.jsonl".to_string(),
        spans_created: 7,
    };
    ensure_equal(
        &result.source_path(),
        &"/tmp/imported-session.jsonl",
        "Imported variant source_path()",
    )
}

#[test]
fn skipped_variant_returns_embedded_source_path() -> TestResult {
    let result = ImportSessionResult::Skipped {
        source_path: "/tmp/skipped-session.jsonl".to_string(),
        reason: "already imported".to_string(),
    };
    ensure_equal(
        &result.source_path(),
        &"/tmp/skipped-session.jsonl",
        "Skipped variant source_path()",
    )
}

#[test]
fn failed_variant_returns_embedded_source_path_not_error() -> TestResult {
    // Guards against the most plausible refactor regression: matching
    // `Failed` and accidentally returning `error` instead of
    // `source_path`. We use distinct strings so a swap is caught.
    let result = ImportSessionResult::Failed {
        source_path: "/tmp/failed-session.jsonl".to_string(),
        error: "parse error at line 3".to_string(),
    };
    ensure_equal(
        &result.source_path(),
        &"/tmp/failed-session.jsonl",
        "Failed variant source_path() must return source_path, not error",
    )
}
