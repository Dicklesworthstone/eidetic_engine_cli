//! Contract coverage for `CassImportGuidanceStatus::as_str` (bd-1u0za).
//!
//! The four variants of `CassImportGuidanceStatus` (defined at
//! `src/core/doctor.rs:1454`) produce stable wire strings that flow
//! through the `cassImportGuidance.status` field of
//! `ee doctor --fix-plan` JSON output. Today no test under `tests/`
//! pins them — the enum is only referenced inside `src/core/doctor.rs`
//! itself and via field access in `src/output/mod.rs`. Silently
//! renaming any variant string ("agent_roots_detected" ->
//! "agent_roots_present") would break agent-facing fix-plan parsing
//! without surfacing in any test. Mirrors bd-w3iv0 (ImportSessionStatus
//! as_str) pin pattern.

use ee::core::doctor::CassImportGuidanceStatus;

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
fn agent_roots_detected_renders_as_agent_roots_detected() -> TestResult {
    ensure_equal(
        &CassImportGuidanceStatus::AgentRootsDetected.as_str(),
        &"agent_roots_detected",
        "AgentRootsDetected wire string",
    )
}

#[test]
fn no_agent_roots_detected_renders_as_no_agent_roots_detected() -> TestResult {
    ensure_equal(
        &CassImportGuidanceStatus::NoAgentRootsDetected.as_str(),
        &"no_agent_roots_detected",
        "NoAgentRootsDetected wire string",
    )
}

#[test]
fn not_inspected_renders_as_not_inspected() -> TestResult {
    ensure_equal(
        &CassImportGuidanceStatus::NotInspected.as_str(),
        &"not_inspected",
        "NotInspected wire string",
    )
}

#[test]
fn unavailable_renders_as_unavailable() -> TestResult {
    ensure_equal(
        &CassImportGuidanceStatus::Unavailable.as_str(),
        &"unavailable",
        "Unavailable wire string",
    )
}

#[test]
fn variants_produce_pairwise_distinct_wire_strings() -> TestResult {
    // Sanity check that no two variants collapse to the same wire
    // string. Collapsing would erase the discriminator that doctor
    // fix-plan consumers use to route follow-up actions.
    let all = [
        (
            "AgentRootsDetected",
            CassImportGuidanceStatus::AgentRootsDetected.as_str(),
        ),
        (
            "NoAgentRootsDetected",
            CassImportGuidanceStatus::NoAgentRootsDetected.as_str(),
        ),
        (
            "NotInspected",
            CassImportGuidanceStatus::NotInspected.as_str(),
        ),
        (
            "Unavailable",
            CassImportGuidanceStatus::Unavailable.as_str(),
        ),
    ];
    for (i, (name_a, str_a)) in all.iter().enumerate() {
        for (name_b, str_b) in all.iter().skip(i + 1) {
            if str_a == str_b {
                return Err(format!(
                    "{name_a} and {name_b} both render as {str_a:?}; variants must be distinct"
                ));
            }
        }
    }
    Ok(())
}
