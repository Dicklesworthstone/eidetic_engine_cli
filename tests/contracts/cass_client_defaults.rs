//! Contract coverage for `CassClient` builder defaults and the
//! `DEFAULT_SUBPROCESS_TIMEOUT` constant (bd-ytz8b).
//!
//! Sister to bd-32efl (REQUIRED_CAPABILITIES/STABLE_ENV_OVERRIDES) and
//! bd-2bwqd / bd-rja7x (CassImportOptions and CassSessionInfo defaults).
//!
//! `CassClient::new_default()` and `CassClient::with_binary()` are the
//! two public constructors used everywhere ee talks to CASS. Their
//! starting field values (`extra_env: Vec::new()`,
//! `subprocess_timeout: DEFAULT_SUBPROCESS_TIMEOUT`) are part of the
//! contract that all downstream invocations depend on. Today they are
//! exercised indirectly through one inline assert that uses the
//! symbolic constant — silently shifting the literal Duration to,
//! say, 5 seconds, would change every cass subprocess deadline without
//! surfacing in any direct test.

use std::path::Path;
use std::time::Duration;

use ee::cass::client::DEFAULT_SUBPROCESS_TIMEOUT;
use ee::cass::{CassClient, DEFAULT_BINARY};

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
fn default_subprocess_timeout_is_thirty_seconds() -> TestResult {
    ensure_equal(
        &DEFAULT_SUBPROCESS_TIMEOUT,
        &Duration::from_secs(30),
        "DEFAULT_SUBPROCESS_TIMEOUT must equal 30 seconds — every cass subprocess invocation \
         that doesn't override --timeout inherits this deadline",
    )
}

#[test]
fn new_default_uses_default_binary_name() -> TestResult {
    let client = CassClient::new_default();
    ensure_equal(
        &client.binary(),
        &Path::new(DEFAULT_BINARY),
        "new_default() routes through DEFAULT_BINARY (relative name) so $PATH discovery applies",
    )
}

#[test]
fn new_default_starts_with_no_extra_env() -> TestResult {
    let client = CassClient::new_default();
    ensure_equal(
        &client.extra_env().len(),
        &0_usize,
        "extra_env starts empty; STABLE_ENV_OVERRIDES are applied per-invocation, not at client construction",
    )
}

#[test]
fn new_default_starts_with_default_subprocess_timeout() -> TestResult {
    let client = CassClient::new_default();
    ensure_equal(
        &client.subprocess_timeout(),
        &DEFAULT_SUBPROCESS_TIMEOUT,
        "new_default() seeds subprocess_timeout from DEFAULT_SUBPROCESS_TIMEOUT (30s)",
    )
}

#[test]
fn with_binary_preserves_explicit_path() -> TestResult {
    let client = CassClient::with_binary("/usr/local/bin/cass");
    ensure_equal(
        &client.binary(),
        &Path::new("/usr/local/bin/cass"),
        "with_binary preserves the caller's explicit binary path verbatim",
    )
}

#[test]
fn with_binary_starts_with_default_subprocess_timeout() -> TestResult {
    let client = CassClient::with_binary("/usr/local/bin/cass");
    ensure_equal(
        &client.subprocess_timeout(),
        &DEFAULT_SUBPROCESS_TIMEOUT,
        "with_binary() seeds subprocess_timeout from DEFAULT_SUBPROCESS_TIMEOUT (30s)",
    )
}

#[test]
fn with_timeout_overrides_default() -> TestResult {
    let client = CassClient::new_default().with_timeout(Duration::from_millis(500));
    ensure_equal(
        &client.subprocess_timeout(),
        &Duration::from_millis(500),
        "with_timeout overrides the default; this proves the builder is a real setter, not a no-op",
    )
}
