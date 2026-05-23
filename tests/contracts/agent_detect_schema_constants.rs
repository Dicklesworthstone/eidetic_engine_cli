//! Contract coverage for the agent-detect schema wire constants
//! (bd-276p8).
//!
//! `AGENT_STATUS_SCHEMA_V1` and `AGENT_SOURCES_SCHEMA_V1` (defined at
//! `src/core/agent_detect.rs:21,23`) are wire-format identifiers
//! written into `ee.agent.status` and `ee.agent.sources` JSON
//! envelopes that agent harnesses parse. Today the constants are
//! referenced by symbol in `tests/contracts/cass_import_guidance_*`
//! and `tests/contracts/agent_status.rs`, but no test asserts their
//! literal string values directly — a future agent could rename
//! either const value (e.g. `ee.agent.status.v1` ->
//! `ee.agent_status.v1`) without breaking any symbol-referencing
//! test. Sister to bd-1ewgp (cass schema-id constants) and bd-32efl
//! (cass contract constants).

use ee::core::agent_detect::{AGENT_SOURCES_SCHEMA_V1, AGENT_STATUS_SCHEMA_V1};

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
fn agent_status_schema_v1_literal_is_pinned() -> TestResult {
    ensure_equal(
        &AGENT_STATUS_SCHEMA_V1,
        &"ee.agent.status.v1",
        "ee.agent.status.v1 is the wire identifier on every ee agent status JSON envelope",
    )
}

#[test]
fn agent_sources_schema_v1_literal_is_pinned() -> TestResult {
    ensure_equal(
        &AGENT_SOURCES_SCHEMA_V1,
        &"ee.agent.sources.v1",
        "ee.agent.sources.v1 is the wire identifier on every ee agent sources JSON envelope",
    )
}

#[test]
fn agent_detect_schema_constants_are_distinct() -> TestResult {
    // Sanity check that the two constants don't collapse to the same
    // wire string. Collapsing would erase the discriminator that
    // agent-side consumers use to dispatch status vs sources.
    if AGENT_STATUS_SCHEMA_V1 == AGENT_SOURCES_SCHEMA_V1 {
        return Err(format!(
            "AGENT_STATUS_SCHEMA_V1 and AGENT_SOURCES_SCHEMA_V1 must be distinct; both equal {AGENT_STATUS_SCHEMA_V1}"
        ));
    }
    Ok(())
}
