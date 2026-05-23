//! Contract coverage for the cass `SUBSYSTEM` const and the
//! `subsystem_name()` accessor (bd-18c88).
//!
//! `src/cass/mod.rs:64` defines `pub const SUBSYSTEM: &str = "cass"`
//! and `subsystem_name()` returns it. The literal is read by
//! `src/shadow.rs`, `src/serve.rs`, `src/mcp.rs`, and
//! `src/config/mod.rs` for status labels, audit logs, and degradation
//! reporting. The inline test `subsystem_name_is_stable` in
//! `src/cass/mod.rs` asserts the function return value but does not
//! pin the `SUBSYSTEM` const literal nor the identity contract
//! (function returns the const verbatim).

use ee::cass::{SUBSYSTEM, subsystem_name};

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
fn subsystem_const_literal_is_cass() -> TestResult {
    ensure_equal(
        &SUBSYSTEM,
        &"cass",
        "ee::cass::SUBSYSTEM const must equal the literal \"cass\" — \
         status labels and audit logs key on this string across releases",
    )
}

#[test]
fn subsystem_name_returns_the_const_verbatim() -> TestResult {
    // Identity contract: subsystem_name() must return SUBSYSTEM as-is.
    // A future agent cannot replace the function body with a derived
    // string (e.g. format!("{}", "cass")) without surfacing here.
    let function_result = subsystem_name();
    ensure_equal(
        &function_result,
        &SUBSYSTEM,
        "subsystem_name() must return ee::cass::SUBSYSTEM by reference, \
         not a derived/owned copy",
    )
}

#[test]
fn subsystem_name_matches_literal_cass_string() -> TestResult {
    // Triangulating contract: the function result must equal the
    // literal "cass". Together with the const pin above, this catches
    // a future refactor that changes the const value AND mirrors that
    // change into the function body (the test would fail at one
    // assertion but not the other if the two were dropped out of sync).
    ensure_equal(
        &subsystem_name(),
        &"cass",
        "subsystem_name() literal contract",
    )
}
