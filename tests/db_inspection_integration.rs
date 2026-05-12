//! Integration tests for `ee db status`, `ee db check`, and `ee db migrations`.
//!
//! Bead: eidetic_engine_cli-m5gc5. Verifies that the inspection commands
//! actually open the database via SQLModel/FrankenSQLite and report real
//! schema, integrity, and migration state instead of file-based stubs.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn scenario_dir(name: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join("db_inspection")
        .join(name)
        .join(format!("{pid}-{ts}"))
}

fn init_workspace(dir: &Path) {
    fs::create_dir_all(dir).expect("create workspace dir");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(
        vec![
            OsString::from("ee"),
            OsString::from("init"),
            OsString::from("--workspace"),
            OsString::from(dir),
            OsString::from("--json"),
        ],
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
}

fn run_cli(args: Vec<OsString>) -> (ee::models::ProcessExitCode, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(args, &mut stdout, &mut stderr);
    (exit, String::from_utf8_lossy(&stdout).into_owned())
}

fn parse_response(stdout: &str) -> Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("expected JSON response, got {stdout:?}: {err}"))
}

fn report(stdout: &str) -> Value {
    let parsed = parse_response(stdout);
    parsed
        .get("data")
        .and_then(|d| d.get("report"))
        .cloned()
        .unwrap_or_else(|| panic!("missing data.report in {stdout}"))
}

#[test]
fn db_status_reports_missing_database_explicitly() {
    let dir = scenario_dir("status_missing");
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("ghost.db");
    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("status"),
        OsString::from("--database"),
        OsString::from(&database),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);
    assert_eq!(report["exists"], Value::Bool(false));
    assert!(
        report["error"].as_str().unwrap_or("").contains("not found"),
        "expected file-not-found error in {stdout}"
    );
    assert_eq!(report["schemaVersion"], Value::Null);
    assert_eq!(report["tableCount"], Value::Null);
}

#[test]
fn db_status_reports_real_schema_state_for_initialized_workspace() {
    let dir = scenario_dir("status_real");
    init_workspace(&dir);
    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("status"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);

    assert_eq!(report["exists"], Value::Bool(true));
    assert!(report["error"].is_null(), "unexpected error: {report}");

    let schema_version = report["schemaVersion"].as_u64().expect("schemaVersion");
    let latest = report["latestCompiledSchemaVersion"]
        .as_u64()
        .expect("latestCompiledSchemaVersion");
    assert_eq!(
        schema_version, latest,
        "ee init should leave the DB at the latest compiled migration"
    );
    assert_eq!(report["needsMigration"], Value::Bool(false));
    assert!(
        report["appliedMigrationCount"].as_u64().unwrap_or(0) > 0,
        "expected applied migrations to be > 0"
    );
    assert!(
        report["tableCount"].as_u64().unwrap_or(0) > 5,
        "expected real table count, got {report}"
    );
    assert_eq!(
        report["pendingMigrationVersions"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(usize::MAX),
        0,
        "expected zero pending migrations after init"
    );
    assert!(
        report["fileSizeBytes"].as_u64().unwrap_or(0) > 0,
        "expected non-zero file size"
    );
    let mode = report["journalMode"].as_str().unwrap_or("");
    assert!(
        !mode.is_empty(),
        "expected non-empty journal mode (got {mode:?})"
    );
}

#[test]
fn db_status_counts_includes_per_table_row_counts_when_requested() {
    let dir = scenario_dir("status_counts");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("status"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--counts"),
        OsString::from("--wal"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);

    let counts = report["tableRowCounts"]
        .as_array()
        .expect("tableRowCounts populated when --counts is passed")
        .clone();
    assert!(!counts.is_empty(), "expected per-table row counts");
    let workspaces = counts
        .iter()
        .find(|c| c["table"].as_str() == Some("workspaces"))
        .expect("workspaces table should appear in row counts");
    assert!(
        workspaces["rows"].as_u64().unwrap_or(0) >= 1,
        "expected at least one workspace row after init, got {workspaces}"
    );
    assert!(
        report["walPath"].as_str().is_some(),
        "expected walPath when --wal is passed"
    );
    assert!(
        report["walFileExists"].is_boolean(),
        "walFileExists should be reported"
    );
}

#[test]
fn db_check_passes_for_freshly_initialized_database() {
    let dir = scenario_dir("check_ok");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("check"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--full"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);
    assert_eq!(report["passed"], Value::Bool(true));
    assert_eq!(report["checkType"], Value::String("integrity_check".into()));
    assert_eq!(report["integrityPassed"], Value::Bool(true));
    assert_eq!(report["foreignKeyPassed"], Value::Bool(true));
    assert!(
        report["integrityIssues"].is_null(),
        "expected no integrity issues, got {report}"
    );
    assert!(
        report["foreignKeyViolations"].is_null(),
        "expected no fk violations, got {report}"
    );
}

#[test]
fn db_check_quick_check_runs_without_foreign_key_check() {
    let dir = scenario_dir("check_quick");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("check"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);
    assert_eq!(report["passed"], Value::Bool(true));
    assert_eq!(report["checkType"], Value::String("quick_check".into()));
    assert_eq!(report["integrityPassed"], Value::Bool(true));
    assert_eq!(
        report["foreignKeyPassed"],
        Value::Null,
        "quick check should not run foreign-key check"
    );
}

#[test]
fn db_check_reports_missing_database_with_storage_exit_code() {
    let dir = scenario_dir("check_missing");
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("missing.db");

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("check"),
        OsString::from("--database"),
        OsString::from(&database),
        OsString::from("--json"),
    ]);
    assert_eq!(exit, ee::models::ProcessExitCode::Storage);
    let report = report(&stdout);
    assert_eq!(report["passed"], Value::Bool(false));
    assert!(
        report["message"]
            .as_str()
            .unwrap_or("")
            .contains("not found"),
        "expected file-not-found message, got {report}"
    );
}

#[test]
fn db_migrations_lists_applied_and_pending_for_real_database() {
    let dir = scenario_dir("migrations_real");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("migrations"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);

    let applied = report["applied"].as_array().expect("applied list").clone();
    assert!(!applied.is_empty(), "expected applied migrations");
    let first = &applied[0];
    assert!(first["version"].as_u64().unwrap_or(0) >= 1);
    assert!(
        !first["name"].as_str().unwrap_or("").is_empty(),
        "expected non-empty migration name"
    );
    assert!(
        !first["checksum"].as_str().unwrap_or("").is_empty(),
        "expected non-empty checksum"
    );
    assert!(
        !first["appliedAt"].as_str().unwrap_or("").is_empty(),
        "expected non-empty appliedAt"
    );

    let pending = report["pending"].as_array().expect("pending list");
    assert!(
        pending.is_empty(),
        "expected no pending migrations after init: {report}"
    );
    assert_eq!(report["needsMigration"], Value::Bool(false));
    let schema_version = report["schemaVersion"].as_u64().expect("schemaVersion");
    let latest = report["latestCompiledSchemaVersion"]
        .as_u64()
        .expect("latestCompiledSchemaVersion");
    assert_eq!(schema_version, latest);
}

#[test]
fn db_migrations_filter_applied_excludes_pending_section() {
    let dir = scenario_dir("migrations_applied_only");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("migrations"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--status"),
        OsString::from("applied"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);
    assert!(!report["applied"].as_array().unwrap().is_empty());
    assert!(
        report["pending"].as_array().unwrap().is_empty(),
        "applied filter must not populate pending"
    );
}

#[test]
fn db_migrations_filter_pending_returns_empty_when_up_to_date() {
    let dir = scenario_dir("migrations_pending_only");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("migrations"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--status"),
        OsString::from("pending"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let report = report(&stdout);
    assert!(
        report["pending"].as_array().unwrap().is_empty(),
        "expected zero pending after init"
    );
    assert!(
        report["applied"].as_array().unwrap().is_empty(),
        "pending filter must not populate applied"
    );
}

#[test]
fn db_migrations_invalid_status_returns_error() {
    let dir = scenario_dir("migrations_invalid_status");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("migrations"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--status"),
        OsString::from("nonsense"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Storage,
        "stdout={stdout}"
    );
    let parsed = parse_response(&stdout);
    assert_eq!(parsed["success"], Value::Bool(false));
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["filter"], Value::String("nonsense".into()));
    assert!(
        report["error"]
            .as_str()
            .unwrap_or("")
            .contains("invalid --status"),
        "expected invalid-status error, got {report}"
    );
}

#[test]
fn db_migrations_missing_database_lists_compiled_pending() {
    let dir = scenario_dir("migrations_missing");
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("never-created.db");

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("migrations"),
        OsString::from("--database"),
        OsString::from(&database),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Storage,
        "stdout={stdout}"
    );
    let parsed = parse_response(&stdout);
    assert_eq!(parsed["success"], Value::Bool(false));
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["exists"], Value::Bool(false));
    let pending = report["pending"]
        .as_array()
        .expect("pending list when DB is missing");
    assert!(
        !pending.is_empty(),
        "expected compiled migrations to be reported as pending when DB is absent"
    );
}
