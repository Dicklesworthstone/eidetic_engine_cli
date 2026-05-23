//! Contract coverage for `CassInvocation` builder defaults and
//! getter round-trip (bd-2whz8).
//!
//! `CassInvocation` (src/cass/process.rs:238) is the only public surface
//! for constructing a cass subprocess invocation outside of
//! `CassClient`. Its builder methods (`new`, `with_cwd`, `with_env`,
//! `with_timeout`) and getters (`binary`, `args`, `cwd`,
//! `env_overrides`, `timeout`) are exercised by inline tests only as
//! setup machinery for other assertions — never as the unit under
//! test. This file pins:
//!
//!   - `CassInvocation::new(binary, args)` defaults: `cwd() == None`,
//!     `env_overrides()` empty, `timeout() == None`.
//!   - `binary()` and `args()` round-trip the constructor inputs.
//!   - `with_timeout(d)` round-trips through `timeout()`.
//!   - `with_cwd(p)` round-trips through `cwd()`.
//!   - `with_env(k, v)` appends to `env_overrides()`, preserving the
//!     caller's insertion order (later assignments do not overwrite
//!     earlier ones at the slice level — `Command::env` last-wins
//!     resolution happens downstream when the process is spawned).
//!
//! Mirrors bd-w3iv0 / bd-3ry2a bounded-contract pin pattern:
//! deterministic, no fixtures, no new public API.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ee::cass::CassInvocation;

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
fn new_defaults_cwd_env_and_timeout_to_unset() -> TestResult {
    let invocation = CassInvocation::new(PathBuf::from("/usr/local/bin/cass"), ["view", "--json"]);

    ensure_equal(
        &invocation.binary(),
        &Path::new("/usr/local/bin/cass"),
        "binary() round-trips constructor input",
    )?;

    let expected_args: Vec<OsString> = vec![OsString::from("view"), OsString::from("--json")];
    ensure_equal(
        &invocation.args(),
        &expected_args.as_slice(),
        "args() round-trips constructor input",
    )?;

    ensure_equal(
        &invocation.cwd(),
        &None,
        "fresh invocation cwd defaults to None",
    )?;
    ensure_equal(
        &invocation.env_overrides().len(),
        &0,
        "fresh invocation env_overrides defaults to empty",
    )?;
    ensure_equal(
        &invocation.timeout(),
        &None,
        "fresh invocation timeout defaults to None",
    )
}

#[test]
fn with_timeout_round_trips_through_timeout_getter() -> TestResult {
    let budget = Duration::from_millis(2500);
    let invocation = CassInvocation::new("cass", ["health", "--json"]).with_timeout(budget);
    ensure_equal(
        &invocation.timeout(),
        &Some(budget),
        "with_timeout(d).timeout() must surface Some(d)",
    )
}

#[test]
fn with_cwd_round_trips_through_cwd_getter() -> TestResult {
    let cwd_path = PathBuf::from("/tmp/cass-workspace");
    let invocation = CassInvocation::new("cass", ["sessions", "--json"]).with_cwd(cwd_path.clone());
    ensure_equal(
        &invocation.cwd(),
        &Some(cwd_path.as_path()),
        "with_cwd(p).cwd() must surface Some(p)",
    )
}

#[test]
fn with_env_appends_overrides_in_insertion_order() -> TestResult {
    // `with_env` is documented to push each call into the override
    // list; `Command::env` last-wins resolution happens downstream
    // at spawn time, not at the slice level. The contract under
    // test here is that env_overrides() reflects each push in the
    // caller's order so downstream consumers can audit overrides
    // without reordering surprises.
    let invocation = CassInvocation::new("cass", ["view", "--json"])
        .with_env("EE_CASS_FOO", "first")
        .with_env("EE_CASS_BAR", "second")
        .with_env("EE_CASS_FOO", "third");

    let expected: Vec<(OsString, OsString)> = vec![
        (OsString::from("EE_CASS_FOO"), OsString::from("first")),
        (OsString::from("EE_CASS_BAR"), OsString::from("second")),
        (OsString::from("EE_CASS_FOO"), OsString::from("third")),
    ];
    ensure_equal(
        &invocation.env_overrides(),
        &expected.as_slice(),
        "env_overrides() preserves push order across repeated keys",
    )
}
