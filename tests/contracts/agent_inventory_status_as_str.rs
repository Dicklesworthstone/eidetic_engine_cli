//! Contract coverage for `AgentInventoryStatus::as_str` vocabulary
//! (bd-vlhhb).
//!
//! `AgentInventoryStatus` (defined at `src/core/agent_detect.rs:27`) is
//! the input to `CassImportGuidance::from_agent_inventory`'s
//! status-routing match (`src/core/doctor.rs:1517-1523`). The four
//! variants' `as_str()` wire strings (`ready`, `empty`,
//! `not_inspected`, `unavailable`) flow through the `ee agent status`
//! JSON envelope and the cass-import-guidance dispatch.
//!
//! Today only the `Ready` variant is exercised inline in
//! `tests/contracts/agent_status.rs:127`. The other three variants
//! are unpinned anywhere — a future agent renaming any of them
//! would break agent-facing fix-plan dispatch and the
//! CassImportGuidance status mapping without surfacing in any test.

use ee::core::agent_detect::AgentInventoryStatus;

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
fn ready_renders_as_ready() -> TestResult {
    ensure_equal(
        &AgentInventoryStatus::Ready.as_str(),
        &"ready",
        "Ready wire string",
    )
}

#[test]
fn empty_renders_as_empty() -> TestResult {
    ensure_equal(
        &AgentInventoryStatus::Empty.as_str(),
        &"empty",
        "Empty wire string",
    )
}

#[test]
fn not_inspected_renders_as_not_inspected() -> TestResult {
    ensure_equal(
        &AgentInventoryStatus::NotInspected.as_str(),
        &"not_inspected",
        "NotInspected wire string",
    )
}

#[test]
fn unavailable_renders_as_unavailable() -> TestResult {
    ensure_equal(
        &AgentInventoryStatus::Unavailable.as_str(),
        &"unavailable",
        "Unavailable wire string",
    )
}

#[test]
fn variants_produce_pairwise_distinct_wire_strings() -> TestResult {
    let all = [
        ("Ready", AgentInventoryStatus::Ready.as_str()),
        ("Empty", AgentInventoryStatus::Empty.as_str()),
        ("NotInspected", AgentInventoryStatus::NotInspected.as_str()),
        ("Unavailable", AgentInventoryStatus::Unavailable.as_str()),
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
