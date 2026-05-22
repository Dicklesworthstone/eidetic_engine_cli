//! bd-n0vkg: lock the `context_delta_prior_unknown` repair-string
//! contract.
//!
//! The J6 failure-mode catalog at
//! `tests/fixtures/failure_modes/context_delta_prior_unknown.json`
//! declares an `expected_emission.repair_string` value — the exact
//! repair text the binary is contracted to emit. Round 1 of REVIEW-MODE
//! caught that the CLI's three `context_delta_prior_unknown` emission
//! sites in `src/cli/mod.rs` had drifted: two used a paraphrase, one
//! used a third unrelated repair, and none matched the fixture. The
//! existing fixture validator
//! (`tests/contracts/failure_mode_fixtures.rs::repair_strings_are_pinned`)
//! checks fixture self-consistency only — it never compares against the
//! binary.
//!
//! This test pins the repair string to a shared
//! `core::context_delta::CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR` const that
//! the CLI emits and that this test asserts is byte-identical to the
//! fixture's `expected_emission.repair_string`. Any future drift
//! (binary side or fixture side) fails this test instead of going
//! silently into shipping.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use ee::core::context_delta::{
    CONTEXT_DELTA_PRIOR_UNKNOWN_CODE, CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR,
};

type TestResult = Result<(), String>;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
        .join("context_delta_prior_unknown.json")
}

fn read_fixture() -> Result<Value, String> {
    let text =
        fs::read_to_string(fixture_path()).map_err(|error| format!("read fixture: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse fixture: {error}"))
}

#[test]
fn fixture_advertises_the_prior_unknown_code() -> TestResult {
    // Sanity-pin: if someone renames the fixture file in place but
    // forgets to update its `code` field, the equality assertion below
    // would silently compare against the wrong fixture's repair text.
    let fixture = read_fixture()?;
    let code = fixture["code"]
        .as_str()
        .ok_or_else(|| "fixture missing top-level `code` string".to_string())?;
    if code != CONTEXT_DELTA_PRIOR_UNKNOWN_CODE {
        return Err(format!(
            "fixture code {code:?} does not match \
             CONTEXT_DELTA_PRIOR_UNKNOWN_CODE ({CONTEXT_DELTA_PRIOR_UNKNOWN_CODE:?}); \
             rename the fixture file back to {CONTEXT_DELTA_PRIOR_UNKNOWN_CODE}.json"
        ));
    }
    Ok(())
}

#[test]
fn cli_const_matches_fixture_pinned_repair_string_byte_for_byte() -> TestResult {
    let fixture = read_fixture()?;
    let pinned = fixture
        .pointer("/expected_emission/repair_string")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "fixture missing expected_emission.repair_string; J6 catalog requires it".to_string()
        })?;

    if pinned != CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR {
        return Err(format!(
            "context_delta_prior_unknown repair_string drift detected.\n\
             fixture (tests/fixtures/failure_modes/context_delta_prior_unknown.json):\n\
               {pinned:?}\n\
             cli   (src/core/context_delta.rs::CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR):\n\
               {CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR:?}\n\
             \n\
             Fix one of the two so they are byte-identical. The CLI's three\n\
             emission sites in src/cli/mod.rs (maybe_write_context_delta:\n\
             empty-hash branch, lookup-miss branch, DB-error branch) all\n\
             route through the const and will pick up whichever value you\n\
             land here."
        ));
    }
    Ok(())
}

#[test]
fn pinned_repair_satisfies_fixture_soft_contains_assertion() -> TestResult {
    // The fixture also carries a soft `repair_contains` field. The
    // pinned `repair_string` must satisfy that softer assertion (the
    // soft assertion is what older fixture-validators / less-strict
    // consumers rely on). If the two ever diverge, the fixture itself
    // is internally inconsistent — flag it loudly here so the J6 author
    // can pick one source of truth.
    let fixture = read_fixture()?;
    let soft = fixture
        .pointer("/expected_emission/repair_contains")
        .and_then(Value::as_str)
        .ok_or_else(|| "fixture missing expected_emission.repair_contains".to_string())?;
    if !CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR.contains(soft) {
        return Err(format!(
            "pinned repair {CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR:?} does not contain \
             the fixture's soft assertion {soft:?}; fixture is internally inconsistent"
        ));
    }
    Ok(())
}
