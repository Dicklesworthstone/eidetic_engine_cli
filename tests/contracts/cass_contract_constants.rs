//! Contract coverage for the public cass version + capability constants
//! (bd-32efl).
//!
//! `REQUIRED_API_VERSION`, `REQUIRED_CONTRACT_VERSION`,
//! `REQUIRED_CAPABILITIES`, `STABLE_ENV_OVERRIDES`, and `DEFAULT_BINARY`
//! make up the cass public-contract surface that `CassClient` and
//! `CassContract::ensure_compatible` depend on. Today the existing
//! inline tests in `src/cass/contract.rs` exercise the constants
//! indirectly (via `missing_required_capabilities` and `has_capability`)
//! but no test pins:
//!
//! * the literal integer for `REQUIRED_API_VERSION` (a future agent
//!   could change `pub const REQUIRED_API_VERSION: u32 = 1` to `2`
//!   without breaking any symbol-referencing test).
//! * the literal string for `REQUIRED_CONTRACT_VERSION` / `DEFAULT_BINARY`.
//! * the full ordered membership of `REQUIRED_CAPABILITIES` (length 10,
//!   stored alphabetically).
//! * the full ordered membership of `STABLE_ENV_OVERRIDES`.
//!
//! This file closes that gap with byte-equal assertions.

use ee::cass::{
    DEFAULT_BINARY, REQUIRED_API_VERSION, REQUIRED_CAPABILITIES, REQUIRED_CONTRACT_VERSION,
    STABLE_ENV_OVERRIDES,
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

#[test]
fn required_api_version_is_one() -> TestResult {
    ensure_equal(
        &REQUIRED_API_VERSION,
        &1_u32,
        "REQUIRED_API_VERSION must equal 1 — the documented current cass api version",
    )
}

#[test]
fn required_contract_version_is_one_as_string() -> TestResult {
    ensure_equal(
        &REQUIRED_CONTRACT_VERSION,
        &"1",
        "REQUIRED_CONTRACT_VERSION is the cass contract-version string; bump only when the wire shape changes",
    )
}

#[test]
fn default_binary_is_cass() -> TestResult {
    ensure_equal(
        &DEFAULT_BINARY,
        &"cass",
        "DEFAULT_BINARY drives $PATH discovery when no override is set",
    )
}

#[test]
fn required_capabilities_membership_is_pinned_in_order() -> TestResult {
    let expected: &[&str] = &[
        "api_version_command",
        "expand_command",
        "field_selection",
        "introspect_command",
        "json_output",
        "request_id",
        "robot_meta",
        "status_command",
        "timeout",
        "view_command",
    ];
    ensure_equal(
        &REQUIRED_CAPABILITIES,
        &expected,
        "REQUIRED_CAPABILITIES — 10 entries, alphabetical order — gates CassContract::ensure_compatible",
    )
}

#[test]
fn required_capabilities_has_exactly_ten_entries() -> TestResult {
    ensure_equal(
        &REQUIRED_CAPABILITIES.len(),
        &10_usize,
        "REQUIRED_CAPABILITIES length is part of the public contract; adding or removing an entry must be a deliberate slice edit, not a silent change",
    )
}

#[test]
fn required_capabilities_is_alphabetically_sorted() -> TestResult {
    let mut sorted = REQUIRED_CAPABILITIES.to_vec();
    sorted.sort_unstable();
    ensure_equal(
        &sorted.as_slice(),
        &REQUIRED_CAPABILITIES,
        "REQUIRED_CAPABILITIES must remain alphabetically sorted so missing_required_capabilities reports gaps in a stable order",
    )
}

#[test]
fn stable_env_overrides_membership_is_pinned_in_order() -> TestResult {
    let expected: &[(&str, &str)] = &[
        ("CASS_IGNORE_SOURCES_CONFIG", "1"),
        ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", "1"),
    ];
    ensure_equal(
        &STABLE_ENV_OVERRIDES,
        &expected,
        "STABLE_ENV_OVERRIDES — 2 entries, applied in order by CassClient — controls cass subprocess environment",
    )
}

#[test]
fn stable_env_overrides_has_exactly_two_entries() -> TestResult {
    ensure_equal(
        &STABLE_ENV_OVERRIDES.len(),
        &2_usize,
        "STABLE_ENV_OVERRIDES length is part of the public contract",
    )
}
