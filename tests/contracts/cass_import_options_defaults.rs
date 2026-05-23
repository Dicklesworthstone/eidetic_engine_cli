//! Contract coverage for `ee::cass::CassImportOptions::new` defaults
//! (bd-2bwqd).
//!
//! `CassImportOptions::new(workspace_path)` is the only public builder for
//! the import options consumed by `ee import cass`. Its six default field
//! values (5 internal defaults plus the caller-supplied workspace path) are
//! part of the contract every ee CLI invocation depends on, but nothing in
//! `src/cass/import.rs` or under `tests/` pins them today. Silently
//! flipping `include_spans` to `false` or lowering `limit` would change
//! the behavior of every `ee import cass` run without surfacing as a
//! regression.

use std::path::PathBuf;

use ee::cass::CassImportOptions;

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
fn cass_import_options_new_preserves_workspace_path_argument() -> TestResult {
    let opts = CassImportOptions::new("ws-fixture-default");
    ensure_equal(
        &opts.workspace_path,
        &PathBuf::from("ws-fixture-default"),
        "workspace_path round-trips into PathBuf",
    )
}

#[test]
fn cass_import_options_new_defaults_database_path_to_none() -> TestResult {
    let opts = CassImportOptions::new("ws-fixture");
    ensure_equal(&opts.database_path, &None, "database_path default")
}

#[test]
fn cass_import_options_new_defaults_limit_to_ten() -> TestResult {
    let opts = CassImportOptions::new("ws-fixture");
    ensure_equal(&opts.limit, &10_u32, "limit default")
}

#[test]
fn cass_import_options_new_defaults_since_to_none() -> TestResult {
    let opts = CassImportOptions::new("ws-fixture");
    ensure_equal(&opts.since, &None, "since default")
}

#[test]
fn cass_import_options_new_defaults_dry_run_to_false() -> TestResult {
    let opts = CassImportOptions::new("ws-fixture");
    ensure_equal(&opts.dry_run, &false, "dry_run default")
}

#[test]
fn cass_import_options_new_defaults_include_spans_to_true() -> TestResult {
    let opts = CassImportOptions::new("ws-fixture");
    ensure_equal(
        &opts.include_spans,
        &true,
        "include_spans default (evidence-span capture must remain opt-out, not opt-in)",
    )
}

#[test]
fn cass_import_options_new_returns_full_default_struct() -> TestResult {
    // Single struct-shaped equality assertion so the full default snapshot
    // is pinned in one place — any silently added field that bypasses the
    // per-field tests above must still match here to compile, and any
    // changed default value will surface as a diff in this test's output.
    let opts = CassImportOptions::new("ws-fixture-default");
    let expected = CassImportOptions {
        workspace_path: PathBuf::from("ws-fixture-default"),
        database_path: None,
        limit: 10,
        since: None,
        dry_run: false,
        include_spans: true,
    };
    ensure_equal(&opts, &expected, "full default struct equality")
}
