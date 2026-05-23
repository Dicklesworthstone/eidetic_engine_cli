//! Contract coverage for `CassContract` version getter round-trip
//! from `new()` (bd-dro5r).
//!
//! `CassContract::new(crate_version, api_version, contract_version, caps)`
//! is the public constructor used wherever ee assembles a CASS contract
//! view (preflight parsing, fixtures, ensure_compatible callers). The
//! `capabilities()` getter is well-tested by the inline
//! `capability_list_is_sorted_and_deduped` and
//! `capability_list_drops_blanks_and_trims` tests, but the three
//! version getters (`crate_version`, `api_version`, `contract_version`)
//! have no direct round-trip pin — the inline Display test asserts
//! rendered substrings like `crate=0.3.0` and `api=1`, but a future
//! agent who renamed an internal field but forgot to update the
//! getter would not be caught.

use ee::cass::CassContract;

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
fn new_round_trips_crate_version_string() -> TestResult {
    let contract = CassContract::new("0.3.0", 1, "1", ["json_output"]);
    ensure_equal(
        &contract.crate_version(),
        &"0.3.0",
        "crate_version() must return the caller-supplied crate_version verbatim",
    )
}

#[test]
fn new_round_trips_api_version_integer() -> TestResult {
    let contract = CassContract::new("0.3.0", 42, "1", ["json_output"]);
    ensure_equal(
        &contract.api_version(),
        &42_u32,
        "api_version() must return the caller-supplied u32 verbatim",
    )
}

#[test]
fn new_round_trips_contract_version_string() -> TestResult {
    let contract = CassContract::new("0.3.0", 1, "v2-alpha", ["json_output"]);
    ensure_equal(
        &contract.contract_version(),
        &"v2-alpha",
        "contract_version() must return the caller-supplied contract_version verbatim",
    )
}

#[test]
fn new_round_trips_all_three_versions_simultaneously() -> TestResult {
    // Three-getter round-trip in one assertion path: catches a refactor
    // that accidentally swaps fields (e.g. crate_version getter
    // accidentally returns contract_version's storage).
    let contract = CassContract::new("9.8.7", 11, "13", ["json_output"]);
    ensure_equal(
        &contract.crate_version(),
        &"9.8.7",
        "crate_version simultaneously",
    )?;
    ensure_equal(
        &contract.api_version(),
        &11_u32,
        "api_version simultaneously",
    )?;
    ensure_equal(
        &contract.contract_version(),
        &"13",
        "contract_version simultaneously",
    )
}

#[test]
fn new_accepts_empty_capability_list() -> TestResult {
    // The capabilities pipeline (trim + filter blank + sort + dedup)
    // is tested elsewhere; pin here that an empty-iterator input
    // produces an empty capabilities() return without panicking and
    // without leaking any default capability.
    let contract = CassContract::new("0.3.0", 1, "1", Vec::<String>::new());
    ensure_equal(
        &contract.capabilities(),
        &Vec::<String>::new().as_slice(),
        "empty capability iterator must produce an empty capabilities slice",
    )
}
