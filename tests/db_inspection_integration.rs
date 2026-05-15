//! Integration tests for `ee db status`, `ee db check`, and `ee db migrations`.
//!
//! Bead: eidetic_engine_cli-m5gc5. Verifies that the inspection commands
//! actually open the database via SQLModel/FrankenSQLite and report real
//! schema, integrity, and migration state instead of file-based stubs.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

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

fn trace_db_inspect(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "db_inspection_integration_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.1"),
        surface = "db_inspect",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "database inspection contract checkpoint"
    );
}

fn run_cli(args: Vec<OsString>) -> (ee::models::ProcessExitCode, String) {
    let started = Instant::now();
    trace_db_inspect("dispatch", 0, &[]);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(args, &mut stdout, &mut stderr);
    trace_db_inspect("response", started.elapsed().as_millis() as u64, &[]);
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

fn pretty_json(value: &Value) -> String {
    let mut rendered = serde_json::to_string_pretty(value).expect("render golden JSON");
    rendered.push('\n');
    rendered
}

fn assert_json_golden(actual: Value, expected: &str) {
    let actual = pretty_json(&actual);
    assert_eq!(actual, expected, "golden snapshot drift");
}

fn db_status_golden_view(parsed: &Value) -> Value {
    let report = &parsed["data"]["report"];
    json!({
        "schema": parsed["schema"].clone(),
        "success": parsed["success"].clone(),
        "command": parsed["data"]["command"].clone(),
        "report": {
            "exists": report["exists"].clone(),
            "fileSizeClass": if report["fileSizeBytes"].as_u64().unwrap_or(0) > 0 { "positive" } else { "zero" },
            "journalModePresent": report["journalMode"].as_str().is_some_and(|mode| !mode.is_empty()),
            "pageSizeClass": if report["pageSizeBytes"].as_u64().unwrap_or(0) > 0 { "positive" } else { "zero" },
            "pageCountClass": if report["pageCount"].as_u64().unwrap_or(0) > 0 { "positive" } else { "zero" },
            "schemaVersionMatchesCompiled": report["schemaVersion"] == report["latestCompiledSchemaVersion"],
            "needsMigration": report["needsMigration"].clone(),
            "appliedMigrationCountClass": if report["appliedMigrationCount"].as_u64().unwrap_or(0) > 0 { "positive" } else { "zero" },
            "pendingMigrationVersions": report["pendingMigrationVersions"].clone(),
            "tableCountClass": if report["tableCount"].as_u64().unwrap_or(0) > 5 { "many" } else { "few" },
            "error": report["error"].clone(),
        },
        "degraded": parsed["degraded"].clone(),
    })
}

fn db_check_integrity_golden_view(parsed: &Value) -> Value {
    let report = &parsed["data"]["report"];
    json!({
        "schema": parsed["schema"].clone(),
        "success": parsed["success"].clone(),
        "command": parsed["data"]["command"].clone(),
        "report": {
            "checkType": report["checkType"].clone(),
            "passed": report["passed"].clone(),
            "integrityPassed": report["integrityPassed"].clone(),
            "integrityIssues": report["integrityIssues"].clone(),
            "foreignKeyPassed": report["foreignKeyPassed"].clone(),
            "foreignKeyViolations": report["foreignKeyViolations"].clone(),
            "message": report["message"].clone(),
        },
        "degraded": parsed["degraded"].clone(),
    })
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
    let parsed = parse_response(&stdout);
    let report = parsed["data"]["report"].clone();
    assert_json_golden(
        db_status_golden_view(&parsed),
        include_str!("golden/db_status.snap"),
    );

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
fn db_status_reports_migration_pending_with_exit_8() {
    let dir = scenario_dir("status_migration_pending");
    init_workspace(&dir);
    let skewed_db = dir.join("schema-migration-required.db");

    let (skew_exit, skew_stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("diag"),
        OsString::from("database-skew"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--output-database"),
        OsString::from(&skewed_db),
        OsString::from("--skew"),
        OsString::from("schema-migration-required"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        skew_exit,
        ee::models::ProcessExitCode::Success,
        "stdout={skew_stdout}"
    );

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("status"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--database"),
        OsString::from(&skewed_db),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::MigrationRequired,
        "stdout={stdout}"
    );
    let parsed = parse_response(&stdout);
    assert_eq!(parsed["success"], Value::Bool(false));
    assert!(
        parsed["degraded"].as_array().is_some_and(|entries| entries
            .iter()
            .any(|entry| entry["code"] == Value::String("db_migration_pending".into()))),
        "expected db_migration_pending degraded entry in {parsed}"
    );
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["needsMigration"], Value::Bool(true));
    assert!(
        report["pendingMigrationVersions"]
            .as_array()
            .is_some_and(|pending| !pending.is_empty()),
        "expected pending migrations in {report}"
    );
}

#[test]
fn db_status_reports_stale_wal_sidecar() {
    let dir = scenario_dir("status_stale_wal");
    fs::create_dir_all(dir.join(".ee")).expect("create workspace storage dir");
    let db_path = dir.join(".ee").join("ghost.db");
    let mut wal_path = db_path.as_os_str().to_os_string();
    wal_path.push("-wal");
    fs::File::create(PathBuf::from(wal_path)).expect("create empty wal sidecar");

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("status"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--database"),
        OsString::from(&db_path),
        OsString::from("--wal"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let parsed = parse_response(&stdout);
    assert_eq!(parsed["success"], Value::Bool(false));
    assert!(
        parsed["degraded"].as_array().is_some_and(|entries| entries
            .iter()
            .any(|entry| entry["code"] == Value::String("db_wal_stale".into()))),
        "expected db_wal_stale degraded entry in {parsed}"
    );
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["exists"], Value::Bool(false));
    assert_eq!(report["walFileExists"], Value::Bool(true));
    assert_eq!(report["shmFileExists"], Value::Bool(false));
}

#[test]
fn db_reindex_dry_run_reports_pending_derived_index_work() {
    let dir = scenario_dir("reindex_dry_run");
    init_workspace(&dir);

    let (remember_exit, remember_stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("remember"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--level"),
        OsString::from("procedural"),
        OsString::from("--kind"),
        OsString::from("rule"),
        OsString::from("Run cargo fmt before release."),
        OsString::from("--json"),
    ]);
    assert_eq!(
        remember_exit,
        ee::models::ProcessExitCode::Success,
        "stdout={remember_stdout}"
    );

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("reindex"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--dry-run"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let parsed = parse_response(&stdout);
    assert_eq!(
        parsed["data"]["command"],
        Value::String("db reindex".into())
    );
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["dryRun"], Value::Bool(true));
    assert_eq!(report["previewOnly"], Value::Bool(true));
    assert_eq!(report["mutationAllowed"], Value::Bool(false));
    assert_eq!(report["exists"], Value::Bool(true));
    assert_eq!(report["error"], Value::Null);
    assert!(
        report["documentCounts"]["memories"].as_u64().unwrap_or(0) >= 1,
        "expected at least one memory document in {report}"
    );
    assert!(
        report["documentCounts"]["total"].as_u64().unwrap_or(0) >= 1,
        "expected at least one indexable document in {report}"
    );
    assert_eq!(report["needsRebuild"], Value::Bool(true));
    assert!(
        report["pendingActions"]
            .as_array()
            .is_some_and(|actions| actions.iter().any(|action| {
                action["kind"] == Value::String("full_rebuild".into())
                    && action["mutationAllowed"] == Value::Bool(false)
                    && action["indexFamilies"].as_array().is_some_and(|families| {
                        families
                            .iter()
                            .any(|family| family.as_str() == Some("fts5"))
                    })
                    && action["indexFamilies"].as_array().is_some_and(|families| {
                        families
                            .iter()
                            .any(|family| family.as_str() == Some("json"))
                    })
            })),
        "expected read-only full_rebuild action covering fts5/json in {report}"
    );
}

#[test]
fn db_reindex_dry_run_reports_missing_database_without_writing() {
    let dir = scenario_dir("reindex_missing");
    fs::create_dir_all(&dir).unwrap();
    let database = dir.join("missing.db");

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("reindex"),
        OsString::from("--database"),
        OsString::from(&database),
        OsString::from("--dry-run"),
        OsString::from("--json"),
    ]);
    assert_eq!(exit, ee::models::ProcessExitCode::Storage);
    let parsed = parse_response(&stdout);
    assert_eq!(parsed["success"], Value::Bool(false));
    assert_eq!(
        parsed["data"]["command"],
        Value::String("db reindex".into())
    );
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["exists"], Value::Bool(false));
    assert_eq!(report["mutationAllowed"], Value::Bool(false));
    assert!(
        report["error"].as_str().unwrap_or("").contains("not found"),
        "expected file-not-found error in {report}"
    );
    assert!(
        !database.exists(),
        "db reindex dry-run must not create a missing database"
    );
}

#[test]
fn db_inspect_returns_limited_rows_from_allowlisted_table() {
    let dir = scenario_dir("inspect_workspaces");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("inspect"),
        OsString::from("workspaces"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--limit"),
        OsString::from("1"),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let parsed = parse_response(&stdout);
    assert_eq!(
        parsed["data"]["command"],
        Value::String("db inspect".into())
    );
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["table"], Value::String("workspaces".into()));
    assert_eq!(report["exists"], Value::Bool(true));
    assert_eq!(report["limit"], Value::from(1));
    assert_eq!(report["returnedRowCount"], Value::from(1));
    assert!(
        report["tableRowCount"].as_i64().unwrap_or(0) >= 1,
        "expected real table row count in {report}"
    );
    assert!(
        report["columns"]
            .as_array()
            .is_some_and(|columns| columns.iter().any(|column| column.as_str() == Some("id"))),
        "expected workspaces.id column in {report}"
    );
    let row = report["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .unwrap_or_else(|| panic!("expected one row in {report}"));
    assert!(
        row["values"]["id"].as_str().is_some(),
        "expected row values to include workspace id: {row}"
    );
    assert!(
        row.get("sourceUri").is_some(),
        "each inspected row should carry sourceUri when applicable, even if null"
    );
}

#[test]
fn db_inspect_rejects_unknown_table_without_running_arbitrary_sql() {
    let dir = scenario_dir("inspect_unknown");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("inspect"),
        OsString::from("workspaces;DROP TABLE workspaces"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    assert_eq!(exit, ee::models::ProcessExitCode::Storage);
    let parsed = parse_response(&stdout);
    assert_eq!(parsed["success"], Value::Bool(false));
    let report = parsed["data"]["report"].clone();
    assert!(
        report["error"]
            .as_str()
            .unwrap_or("")
            .contains("was not found"),
        "expected unknown-table error in {report}"
    );
    assert!(
        report["availableTables"]
            .as_array()
            .is_some_and(|tables| tables
                .iter()
                .any(|table| table.as_str() == Some("workspaces"))),
        "expected available table list in {report}"
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
fn db_check_integrity_alias_runs_full_integrity_contract() {
    let dir = scenario_dir("check_integrity_alias");
    init_workspace(&dir);

    let (exit, stdout) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("check-integrity"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    assert_eq!(
        exit,
        ee::models::ProcessExitCode::Success,
        "stdout={stdout}"
    );
    let parsed = parse_response(&stdout);
    assert_json_golden(
        db_check_integrity_golden_view(&parsed),
        include_str!("golden/db_check_integrity.snap"),
    );
    assert_eq!(
        parsed["data"]["command"],
        Value::String("db check-integrity".into())
    );
    let report = parsed["data"]["report"].clone();
    assert_eq!(report["passed"], Value::Bool(true));
    assert_eq!(report["checkType"], Value::String("integrity_check".into()));
    assert_eq!(report["integrityPassed"], Value::Bool(true));
    assert_eq!(report["foreignKeyPassed"], Value::Bool(true));
    assert_eq!(report["auditRecorded"], Value::Bool(true));
    assert_eq!(
        report["auditAction"],
        Value::String("db.check_integrity".into())
    );

    let conn = ee::db::DbConnection::open_schema_only(dir.join(".ee").join("ee.db"))
        .expect("open database for audit inspection");
    let entries = conn
        .list_audit_by_action("db.check_integrity", Some(10))
        .expect("list check-integrity audit rows");
    assert_eq!(entries.len(), 1, "expected one check-integrity audit row");
    let entry = &entries[0];
    assert_eq!(entry.actor.as_deref(), Some("ee db check-integrity"));
    assert_eq!(entry.surface, "db");
    let details: Value = serde_json::from_str(
        entry
            .details
            .as_deref()
            .expect("audit row must include details"),
    )
    .expect("audit details JSON");
    assert_eq!(
        details["command"],
        Value::String("db check-integrity".into())
    );
    assert_eq!(
        details["checkType"],
        Value::String("integrity_check".into())
    );
    assert_eq!(details["passed"], Value::Bool(true));
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
