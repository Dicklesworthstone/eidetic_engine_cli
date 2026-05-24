//! Contract coverage for `DoctorMeshAutoEnrollmentCheckStatus`
//! `as_str` + `needs_attention` per variant (bd-1xamu).
//!
//! `DoctorMeshAutoEnrollmentCheckStatus` (src/core/doctor.rs:371) is
//! the SRR6.46 mesh auto-enrollment check-status enum. It has four
//! variants — Ok, Warning, Fail, Skipped — and two public methods:
//!
//! * `as_str()` returns the stable snake_case wire string that flows
//!   into the doctor JSON `meshAutoEnrollment.checks[*].status`
//!   field. Per the doc comment on the enum, this is intentionally
//!   separate from the legacy top-level `CheckSeverity` because the
//!   SRR6.46 block needs an explicit `skipped` state and a `fail`
//!   state distinct from the historical `ok | warning | error`
//!   wire values.
//! * `needs_attention()` is the predicate the doctor summary uses
//!   to count failing checks: `true` for Warning|Fail, `false`
//!   for Ok|Skipped.
//!
//! Neither method has any test pin. The variants are used in switch
//! arms in `doctor.rs::DoctorMeshAutoEnrollmentSummary::from_checks`
//! (line 457), but the per-variant string identity and predicate
//! semantics are unpinned — a silent rename of `fail -> failed`,
//! a swap of the `Warning` predicate truth value, or collapsing
//! `Skipped` into `Ok` would not be caught.
//!
//! This file pins:
//!   - each variant's exact as_str() string
//!   - the four-way pairwise-distinct invariant
//!   - needs_attention() truthiness per variant
//!
//! Mirrors bd-3rsnj / bd-w3iv0 bounded-vocabulary pin pattern:
//! deterministic, no fixtures, no new public API.

use std::collections::BTreeSet;

use ee::core::doctor::DoctorMeshAutoEnrollmentCheckStatus;

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
fn each_variant_renders_per_doctor_json_wire_contract() -> TestResult {
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Ok.as_str(),
        &"ok",
        "Ok variant",
    )?;
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Warning.as_str(),
        &"warning",
        "Warning variant",
    )?;
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Fail.as_str(),
        &"fail",
        "Fail variant — note: distinct from legacy CheckSeverity \"error\" by design",
    )?;
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Skipped.as_str(),
        &"skipped",
        "Skipped variant — required by SRR6.46 when mesh is disabled",
    )
}

#[test]
fn all_four_variant_strings_are_pairwise_distinct() -> TestResult {
    // Collapsing any two variants to the same string would erase
    // documented distinctions (e.g. Skipped → Ok would lose the
    // mesh-disabled signal in the summary).
    let mut seen = BTreeSet::new();
    for (label, variant) in [
        ("Ok", DoctorMeshAutoEnrollmentCheckStatus::Ok),
        ("Warning", DoctorMeshAutoEnrollmentCheckStatus::Warning),
        ("Fail", DoctorMeshAutoEnrollmentCheckStatus::Fail),
        ("Skipped", DoctorMeshAutoEnrollmentCheckStatus::Skipped),
    ] {
        let value = variant.as_str();
        if !seen.insert(value) {
            return Err(format!(
                "DoctorMeshAutoEnrollmentCheckStatus::{label}.as_str() collapsed to a \
                 previously-seen value {value:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn needs_attention_predicate_is_true_only_for_warning_and_fail() -> TestResult {
    // The doctor summary's failing-check count keys off this
    // predicate; flipping any of the four truth values would change
    // the SRR6.46 summary tallies silently.
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Ok.needs_attention(),
        &false,
        "Ok must not need attention",
    )?;
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Warning.needs_attention(),
        &true,
        "Warning must need attention",
    )?;
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Fail.needs_attention(),
        &true,
        "Fail must need attention",
    )?;
    ensure_equal(
        &DoctorMeshAutoEnrollmentCheckStatus::Skipped.needs_attention(),
        &false,
        "Skipped must not need attention — mesh-disabled is not a failure",
    )
}
