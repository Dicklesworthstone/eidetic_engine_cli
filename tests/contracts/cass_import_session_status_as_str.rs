//! Contract coverage for `ImportSessionStatus::as_str` (bd-w3iv0).
//!
//! `ImportSessionStatus` is a `pub enum` in `src/cass/import.rs:94`
//! with three variants — `Imported`, `Skipped`, `WouldImport` — and a
//! `pub const fn as_str(self) -> &'static str` that returns the stable
//! machine string for each. Those three strings ride the
//! `CassImportReport.sessions[*].status` JSON field and downstream
//! `ee.cass.import.v1` surfaces; renaming any one of them silently
//! (`would_import` -> `willImport`, etc.) would break downstream agent
//! consumers without breaking any existing test because no current
//! test pins the per-variant literal.
//!
//! This file pins:
//!   - `Imported   -> "imported"`
//!   - `Skipped    -> "skipped"`
//!   - `WouldImport -> "would_import"`
//!   - all three strings are pairwise distinct
//!
//! Mirrors the bd-3ry2a / bd-375ve / bd-1hr0l bounded-vocabulary pin
//! pattern: deterministic, no fixtures, no new public API.

use ee::cass::ImportSessionStatus;

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
fn imported_renders_as_imported() -> TestResult {
    ensure_equal(
        &ImportSessionStatus::Imported.as_str(),
        &"imported",
        "ImportSessionStatus::Imported.as_str()",
    )
}

#[test]
fn skipped_renders_as_skipped() -> TestResult {
    ensure_equal(
        &ImportSessionStatus::Skipped.as_str(),
        &"skipped",
        "ImportSessionStatus::Skipped.as_str()",
    )
}

#[test]
fn would_import_renders_as_would_import() -> TestResult {
    // Underscore form is the ee.cass.import.v1 contract; downstream
    // agents key off this exact spelling.
    ensure_equal(
        &ImportSessionStatus::WouldImport.as_str(),
        &"would_import",
        "ImportSessionStatus::WouldImport.as_str()",
    )
}

#[test]
fn all_variant_strings_are_pairwise_distinct() -> TestResult {
    // Collapsing two variants to the same string would erase the
    // imported/skipped/would_import distinction in downstream
    // analytics and dry-run vs apply telemetry.
    let imported = ImportSessionStatus::Imported.as_str();
    let skipped = ImportSessionStatus::Skipped.as_str();
    let would_import = ImportSessionStatus::WouldImport.as_str();
    if imported == skipped {
        return Err(format!(
            "ImportSessionStatus::Imported and ::Skipped collapsed to the same string {imported:?}"
        ));
    }
    if imported == would_import {
        return Err(format!(
            "ImportSessionStatus::Imported and ::WouldImport collapsed to the same string {imported:?}"
        ));
    }
    if skipped == would_import {
        return Err(format!(
            "ImportSessionStatus::Skipped and ::WouldImport collapsed to the same string {skipped:?}"
        ));
    }
    Ok(())
}
