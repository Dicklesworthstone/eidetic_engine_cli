//! Contract coverage for `AgentInventoryDegradation` struct field
//! round-trip (bd-3e2uz).
//!
//! `AgentInventoryDegradation` (defined at
//! `src/core/agent_detect.rs:52`) flows into the `degraded[]` array of
//! `ee.agent.status` JSON envelopes via `AgentInventoryReport.degraded`.
//! Today the struct has zero test coverage anywhere — a future agent
//! who renamed any field or changed the type of `severity` /
//! `repair` from `&'static str` to `String` would break
//! envelope-parsing agent harnesses without surfacing in any test.
//! Sister to bd-2okvr (CassImportRootGuidance), bd-rja7x
//! (CassSessionInfo defaults).

use ee::core::agent_detect::AgentInventoryDegradation;

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

fn fixture() -> AgentInventoryDegradation {
    AgentInventoryDegradation {
        code: "agent_status_partial".to_string(),
        severity: "medium",
        message: "agent inventory partial: one connector unreachable".to_string(),
        repair: "ee agent scan --existing-only --json",
    }
}

#[test]
fn struct_literal_preserves_code() -> TestResult {
    ensure_equal(
        &fixture().code,
        &"agent_status_partial".to_string(),
        "code field round-trip",
    )
}

#[test]
fn struct_literal_preserves_severity() -> TestResult {
    ensure_equal(&fixture().severity, &"medium", "severity field round-trip")
}

#[test]
fn struct_literal_preserves_message() -> TestResult {
    ensure_equal(
        &fixture().message,
        &"agent inventory partial: one connector unreachable".to_string(),
        "message field round-trip",
    )
}

#[test]
fn struct_literal_preserves_repair() -> TestResult {
    ensure_equal(
        &fixture().repair,
        &"ee agent scan --existing-only --json",
        "repair field round-trip",
    )
}

#[test]
fn struct_literal_preserves_all_four_fields_simultaneously() -> TestResult {
    let degradation = AgentInventoryDegradation {
        code: "cass_unavailable".to_string(),
        severity: "high",
        message: "cass binary missing on $PATH".to_string(),
        repair: "install cass or set [cass.binary] in config",
    };
    ensure_equal(
        &degradation.code,
        &"cass_unavailable".to_string(),
        "code simultaneously",
    )?;
    ensure_equal(&degradation.severity, &"high", "severity simultaneously")?;
    ensure_equal(
        &degradation.message,
        &"cass binary missing on $PATH".to_string(),
        "message simultaneously",
    )?;
    ensure_equal(
        &degradation.repair,
        &"install cass or set [cass.binary] in config",
        "repair simultaneously",
    )
}

#[test]
fn struct_derives_clone_and_partial_eq() -> TestResult {
    let original = fixture();
    let cloned = original.clone();
    ensure_equal(
        &cloned,
        &original,
        "Clone must produce a PartialEq-equal value (Clone+Eq+PartialEq+Debug derive contract)",
    )
}
