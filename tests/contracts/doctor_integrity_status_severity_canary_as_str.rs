//! Contract coverage for the three integrity-diagnostics public
//! `as_str` methods exported from `ee::core::doctor` (bd-3rsnj).
//!
//! Three public enums in `src/core/doctor.rs` expose stable string
//! identifiers that flow into the `ee doctor --integrity` JSON output
//! and downstream agent dashboards:
//!
//! * `IntegrityDiagnosticsStatus::as_str` (line 1604) — three
//!   variants: Ok, Degraded, Failed.
//! * `IntegrityDiagnosticSeverity::as_str` (line 1623) — three
//!   variants: Ok, Warning, Error.
//! * `IntegrityCanaryStatus::as_str` (line 1694) — six variants:
//!   NotRequested, DryRun, Created, AlreadyExists, Skipped, Failed.
//!
//! None of the per-variant strings are pinned anywhere. A silent
//! rename — `already_exists -> alreadyExists`, `dry_run -> dryRun`,
//! `degraded -> partial` — would not be caught by any existing test
//! even though the strings are part of the wire contract.
//!
//! This file freezes:
//!   - each variant's exact string output
//!   - per-enum pairwise distinctness (collapsing two variants to the
//!     same string would erase a documented distinction)
//!
//! Mirrors bd-w3iv0 / bd-1u0za bounded-vocabulary pin pattern:
//! deterministic, no fixtures, no new public API.

use std::collections::BTreeSet;

use ee::core::doctor::{
    IntegrityCanaryStatus, IntegrityDiagnosticSeverity, IntegrityDiagnosticsStatus,
};

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

fn ensure_pairwise_distinct(
    strings: &[(&'static str, &'static str)],
    enum_name: &str,
) -> TestResult {
    let mut seen = BTreeSet::new();
    for (variant_label, value) in strings {
        if !seen.insert(*value) {
            return Err(format!(
                "{enum_name}: variant {variant_label:?} produced duplicate as_str() value {value:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn integrity_diagnostics_status_renders_per_variant() -> TestResult {
    ensure_equal(
        &IntegrityDiagnosticsStatus::Ok.as_str(),
        &"ok",
        "IntegrityDiagnosticsStatus::Ok",
    )?;
    ensure_equal(
        &IntegrityDiagnosticsStatus::Degraded.as_str(),
        &"degraded",
        "IntegrityDiagnosticsStatus::Degraded",
    )?;
    ensure_equal(
        &IntegrityDiagnosticsStatus::Failed.as_str(),
        &"failed",
        "IntegrityDiagnosticsStatus::Failed",
    )?;
    ensure_pairwise_distinct(
        &[
            ("Ok", IntegrityDiagnosticsStatus::Ok.as_str()),
            ("Degraded", IntegrityDiagnosticsStatus::Degraded.as_str()),
            ("Failed", IntegrityDiagnosticsStatus::Failed.as_str()),
        ],
        "IntegrityDiagnosticsStatus",
    )
}

#[test]
fn integrity_diagnostic_severity_renders_per_variant() -> TestResult {
    ensure_equal(
        &IntegrityDiagnosticSeverity::Ok.as_str(),
        &"ok",
        "IntegrityDiagnosticSeverity::Ok",
    )?;
    ensure_equal(
        &IntegrityDiagnosticSeverity::Warning.as_str(),
        &"warning",
        "IntegrityDiagnosticSeverity::Warning",
    )?;
    ensure_equal(
        &IntegrityDiagnosticSeverity::Error.as_str(),
        &"error",
        "IntegrityDiagnosticSeverity::Error",
    )?;
    ensure_pairwise_distinct(
        &[
            ("Ok", IntegrityDiagnosticSeverity::Ok.as_str()),
            ("Warning", IntegrityDiagnosticSeverity::Warning.as_str()),
            ("Error", IntegrityDiagnosticSeverity::Error.as_str()),
        ],
        "IntegrityDiagnosticSeverity",
    )
}

#[test]
fn integrity_canary_status_renders_per_variant() -> TestResult {
    ensure_equal(
        &IntegrityCanaryStatus::NotRequested.as_str(),
        &"not_requested",
        "IntegrityCanaryStatus::NotRequested",
    )?;
    ensure_equal(
        &IntegrityCanaryStatus::DryRun.as_str(),
        &"dry_run",
        "IntegrityCanaryStatus::DryRun",
    )?;
    ensure_equal(
        &IntegrityCanaryStatus::Created.as_str(),
        &"created",
        "IntegrityCanaryStatus::Created",
    )?;
    ensure_equal(
        &IntegrityCanaryStatus::AlreadyExists.as_str(),
        &"already_exists",
        "IntegrityCanaryStatus::AlreadyExists",
    )?;
    ensure_equal(
        &IntegrityCanaryStatus::Skipped.as_str(),
        &"skipped",
        "IntegrityCanaryStatus::Skipped",
    )?;
    ensure_equal(
        &IntegrityCanaryStatus::Failed.as_str(),
        &"failed",
        "IntegrityCanaryStatus::Failed",
    )?;
    ensure_pairwise_distinct(
        &[
            ("NotRequested", IntegrityCanaryStatus::NotRequested.as_str()),
            ("DryRun", IntegrityCanaryStatus::DryRun.as_str()),
            ("Created", IntegrityCanaryStatus::Created.as_str()),
            (
                "AlreadyExists",
                IntegrityCanaryStatus::AlreadyExists.as_str(),
            ),
            ("Skipped", IntegrityCanaryStatus::Skipped.as_str()),
            ("Failed", IntegrityCanaryStatus::Failed.as_str()),
        ],
        "IntegrityCanaryStatus",
    )
}
