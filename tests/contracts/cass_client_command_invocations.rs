//! Contract coverage for the three public CassClient subcommand
//! invocation builders (bd-1ljx7).
//!
//! `CassClient::sessions_invocation` (src/cass/client.rs:643),
//! `view_invocation` (line 675), and `expand_invocation` (line 708)
//! each emit a specific argv shape that ee depends on for talking to
//! the cass binary. The exact ordering (`-n` then `-C` then `--json`
//! then `--` then path, or `--workspace` then `--json` then `--limit`
//! for sessions) is part of the wire contract between ee and cass —
//! reordering or renaming any flag would either break the
//! downstream cass parser or silently change runtime behavior.
//!
//! Only the lower-level `invocation()` builder and the
//! `preflight_invocations`/`search_invocation` helpers have inline
//! tests (src/cass/client.rs:810, 840, 850). The three builders
//! covered here have neither inline nor contract coverage.
//!
//! This file pins:
//!   - `view_invocation` produces `view -n <line> -C <context> --json
//!     -- <path>` in that exact order.
//!   - `expand_invocation` produces `expand -n <line> -C <context>
//!     --json -- <path>` (same shape as view but with the `expand`
//!     verb).
//!   - `sessions_invocation` prefix is
//!     `sessions --workspace <path> --json --limit <limit>` in that
//!     order. The optional `--data-dir <value>` suffix driven by
//!     `CASS_DATA_DIR` is not asserted here so the test stays
//!     deterministic regardless of the test-process environment.
//!
//! Mirrors the bd-2whz8 / bd-w3iv0 / bd-3ry2a bounded-contract pin
//! pattern: deterministic, no fixtures, no new public API.

use std::ffi::OsString;
use std::path::Path;

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

fn args_as_strings(args: &[OsString]) -> Result<Vec<&str>, String> {
    args.iter()
        .map(|os| {
            os.to_str()
                .ok_or_else(|| format!("non-utf8 arg slipped into invocation: {os:?}"))
        })
        .collect()
}

#[test]
fn view_invocation_uses_n_dash_capital_c_json_dash_dash_path_shape() -> TestResult {
    let client = CassClient::new_default();
    let invocation = client.view_invocation("/tmp/session.jsonl", 12, 3);

    let args = args_as_strings(invocation.args())?;
    ensure_equal(
        &args,
        &vec![
            "view",
            "-n",
            "12",
            "-C",
            "3",
            "--json",
            "--",
            "/tmp/session.jsonl",
        ],
        "view_invocation arg shape",
    )
}

#[test]
fn expand_invocation_mirrors_view_with_expand_verb() -> TestResult {
    let client = CassClient::new_default();
    let invocation = client.expand_invocation("/tmp/session.jsonl", 7, 5);

    let args = args_as_strings(invocation.args())?;
    ensure_equal(
        &args,
        &vec![
            "expand",
            "-n",
            "7",
            "-C",
            "5",
            "--json",
            "--",
            "/tmp/session.jsonl",
        ],
        "expand_invocation arg shape",
    )
}

#[test]
fn sessions_invocation_prefix_is_workspace_json_limit_in_order() -> TestResult {
    // sessions_invocation may append `--data-dir <value>` when the
    // CASS_DATA_DIR env var is set in the test-runner's environment,
    // so we pin only the deterministic prefix (the first 6 args)
    // here. The data-dir env-driven suffix is exercised through
    // higher-level integration tests.
    let client = CassClient::new_default();
    let invocation = client.sessions_invocation(Path::new("/tmp/workspace"), 17);

    let args = args_as_strings(invocation.args())?;
    if args.len() < 6 {
        return Err(format!(
            "sessions_invocation must emit at least 6 args (the deterministic prefix); got {args:?}"
        ));
    }
    ensure_equal(
        &&args[..6],
        &&[
            "sessions",
            "--workspace",
            "/tmp/workspace",
            "--json",
            "--limit",
            "17",
        ][..],
        "sessions_invocation prefix",
    )
}

#[test]
fn view_invocation_inherits_client_binary_and_timeout() -> TestResult {
    // The three subcommand builders all route through
    // `CassClient::invocation`, so the binary/timeout wiring is
    // shared. Pinning it here on one of the builders proves the
    // delegation has not regressed.
    let client = CassClient::new_default();
    let invocation = client.view_invocation("/tmp/session.jsonl", 1, 1);

    ensure_equal(
        &invocation.binary(),
        &client.binary(),
        "view_invocation binary inherits client binary",
    )?;
    ensure_equal(
        &invocation.timeout(),
        &Some(client.subprocess_timeout()),
        "view_invocation timeout inherits client subprocess_timeout",
    )
}
