//! Contract coverage for `CassClient::with_extra_env` /
//! `CassClient::extra_env` round-trip (bd-2n2h7).
//!
//! `CassClient::with_extra_env` (src/cass/client.rs:504) and the
//! `extra_env()` getter (line 528) form a builder/getter pair. The
//! bd-ytz8b `cass_client_defaults` test pins
//! `new_default().extra_env()` as empty, and an inline test
//! (src/cass/client.rs:826) covers the cross-cutting behavior
//! through the `invocation()` router. But there is no direct test
//! pinning the round-trip itself, the multi-call insertion order,
//! or the repeated-key insertion-preservation contract (the
//! analogue of bd-2whz8 `CassInvocation::env_overrides` semantics).
//!
//! This file pins:
//!   - `with_extra_env(k, v)` round-trips through `extra_env()`.
//!   - Multiple calls preserve the caller's insertion order.
//!   - Repeated keys are both retained at the slice level (the
//!     downstream `Command::env` last-wins resolution happens
//!     when the subprocess is spawned, not at the slice level).
//!
//! Mirrors bd-2whz8 / bd-ytz8b bounded-contract pin pattern:
//! deterministic, no fixtures, no new public API.

use std::ffi::OsString;

use ee::cass::CassClient;

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
fn single_with_extra_env_call_surfaces_through_extra_env_getter() -> TestResult {
    let client = CassClient::new_default().with_extra_env("EE_TRACE", "1");
    let expected: Vec<(OsString, OsString)> =
        vec![(OsString::from("EE_TRACE"), OsString::from("1"))];
    ensure_equal(
        &client.extra_env(),
        &expected.as_slice(),
        "with_extra_env(k, v) must surface through extra_env() as a one-element slice",
    )
}

#[test]
fn multiple_with_extra_env_calls_preserve_insertion_order() -> TestResult {
    let client = CassClient::new_default()
        .with_extra_env("EE_ALPHA", "a")
        .with_extra_env("EE_BETA", "b")
        .with_extra_env("EE_GAMMA", "c");

    let expected: Vec<(OsString, OsString)> = vec![
        (OsString::from("EE_ALPHA"), OsString::from("a")),
        (OsString::from("EE_BETA"), OsString::from("b")),
        (OsString::from("EE_GAMMA"), OsString::from("c")),
    ];
    ensure_equal(
        &client.extra_env(),
        &expected.as_slice(),
        "extra_env() must reflect each with_extra_env call in caller's insertion order",
    )
}

#[test]
fn repeated_keys_are_retained_at_slice_level_not_deduplicated() -> TestResult {
    // The slice contract preserves every push so audit tooling can
    // see exactly what the caller passed. `Command::env` resolves
    // repeated keys with last-wins semantics downstream when the
    // subprocess is actually spawned — this slice is upstream of
    // that resolution and must keep both entries visible.
    let client = CassClient::new_default()
        .with_extra_env("EE_TRACE", "first")
        .with_extra_env("EE_OTHER", "neutral")
        .with_extra_env("EE_TRACE", "second");

    let expected: Vec<(OsString, OsString)> = vec![
        (OsString::from("EE_TRACE"), OsString::from("first")),
        (OsString::from("EE_OTHER"), OsString::from("neutral")),
        (OsString::from("EE_TRACE"), OsString::from("second")),
    ];
    ensure_equal(
        &client.extra_env(),
        &expected.as_slice(),
        "extra_env() must retain repeated keys; Command::env last-wins happens at spawn time",
    )
}
