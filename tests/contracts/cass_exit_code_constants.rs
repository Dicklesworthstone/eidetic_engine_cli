//! Contract coverage for `CASS_EXIT_OK` / `CASS_EXIT_DEGRADED` literal
//! values (bd-1hr0l).
//!
//! `src/cass/process.rs` exposes two integer constants that ee uses to
//! classify CASS subprocess outcomes:
//!
//! * `CASS_EXIT_OK = 0`
//! * `CASS_EXIT_DEGRADED = 1`
//!
//! Existing tests reference these constants by symbol (e.g.
//! `CassExitClass::classify(Some(CASS_EXIT_OK), ...)`), so a future agent
//! could silently re-define `CASS_EXIT_OK` to `2` (or swap the two
//! values) and every existing test would still pass. CASS itself is a
//! versioned external CLI dependency, so the contract on these numeric
//! values is part of ee's published surface — pinning the literals
//! guards against the silent-renumbering class of regression.

use ee::cass::{CASS_EXIT_DEGRADED, CASS_EXIT_OK};

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
fn cass_exit_ok_is_zero() -> TestResult {
    ensure_equal(
        &CASS_EXIT_OK,
        &0_i32,
        "CASS_EXIT_OK must equal 0 — the documented CASS contract: success means exit 0",
    )
}

#[test]
fn cass_exit_degraded_is_one() -> TestResult {
    ensure_equal(
        &CASS_EXIT_DEGRADED,
        &1_i32,
        "CASS_EXIT_DEGRADED must equal 1 — CASS reserves exit 1 for the \
         degraded-but-data-present outcome the retrieval path keys off of",
    )
}

#[test]
fn cass_exit_constants_are_distinct() -> TestResult {
    // Cheap sanity check that the two constants do not collide.
    // Independent values are part of the contract: collapsing them
    // would erase the distinction between Success and Degraded in
    // CassExitClass::classify.
    if CASS_EXIT_OK == CASS_EXIT_DEGRADED {
        return Err(format!(
            "CASS_EXIT_OK and CASS_EXIT_DEGRADED must be distinct integers; got {CASS_EXIT_OK} == {CASS_EXIT_DEGRADED}"
        ));
    }
    Ok(())
}
