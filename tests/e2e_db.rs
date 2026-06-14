//! E2E DB inspection contract for `ee db` real-fixture behavior.
//!
//! Bead: bd-3usjw.1.4. This named target exists so the DB closeout can
//! point at `tests/e2e_db.rs` directly while the broader regression
//! matrix remains in `tests/db_inspection_integration.rs`.

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
        .join("db")
        .join(name)
        .join(format!("{pid}-{ts}"))
}

fn run_cli(args: Vec<OsString>) -> (ee::models::ProcessExitCode, Value) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(args, &mut stdout, &mut stderr);
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let parsed = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("expected JSON response, got {stdout:?}: {err}"));
    (exit, parsed)
}

fn init_workspace(dir: &Path) {
    fs::create_dir_all(dir).expect("create workspace dir");
    let (exit, parsed) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("init"),
        OsString::from("--workspace"),
        OsString::from(dir),
        OsString::from("--json"),
    ]);
    assert_eq!(exit, ee::models::ProcessExitCode::Success);
    assert_eq!(parsed["success"], Value::Bool(true));
}

#[test]
fn e2e_db_read_only_open_reads_existing_file_and_rejects_mutation() {
    let dir = scenario_dir("read_only_open");
    fs::create_dir_all(&dir).expect("create read-only DB scenario dir");
    let database = dir.join("read-only-open.db");

    {
        let writer = ee::db::DbConnection::open_file(&database).expect("seed DB opens writable");
        writer
            .execute_raw("CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT NOT NULL)")
            .expect("seed table creates");
        writer
            .execute_raw("INSERT INTO items (id, value) VALUES (1, 'visible')")
            .expect("seed row inserts");
        writer.close().expect("seed DB closes");
    }

    let read_only =
        ee::db::DbConnection::open_file_read_only(&database).expect("read-only DB opens");
    assert_eq!(read_only.mode(), ee::db::DatabaseOpenMode::ReadOnly);

    let tables = read_only.list_user_tables().expect("read-only table list");
    assert!(
        tables.iter().any(|table| table == "items"),
        "read-only table list must include seeded table: {tables:?}"
    );
    assert_eq!(
        read_only
            .count_table_rows("items")
            .expect("read-only row count"),
        1
    );

    let insert_result =
        read_only.execute_raw("INSERT INTO items (id, value) VALUES (2, 'must_not_write')");
    assert!(matches!(
        insert_result,
        Err(ee::db::DbError::InvalidMode {
            mode: ee::db::DatabaseOpenMode::ReadOnly,
            ..
        })
    ));

    let transaction_result = read_only.with_transaction(|| -> ee::db::Result<()> { Ok(()) });
    assert!(matches!(
        transaction_result,
        Err(ee::db::DbError::InvalidMode {
            mode: ee::db::DatabaseOpenMode::ReadOnly,
            ..
        })
    ));

    read_only.close().expect("read-only DB closes");
}

#[test]
fn e2e_db_contract_exercises_real_initialized_workspace() {
    let dir = scenario_dir("real_initialized_workspace");
    init_workspace(&dir);

    let (status_exit, status) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("status"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--counts"),
        OsString::from("--json"),
    ]);
    assert_eq!(status_exit, ee::models::ProcessExitCode::Success);
    assert_eq!(status["schema"], Value::String("ee.response.v2".into()));
    assert_eq!(status["data"]["command"], Value::String("db status".into()));
    let status_report = &status["data"]["report"];
    assert_eq!(status_report["exists"], Value::Bool(true));
    assert_eq!(status_report["needsMigration"], Value::Bool(false));
    assert!(status_report["schemaVersion"].as_u64().unwrap_or(0) > 0);
    assert!(
        status_report["tableCount"].as_u64().unwrap_or(0) > 5,
        "expected real schema table count in {status_report}"
    );
    assert!(
        status_report["tableRowCounts"]
            .as_array()
            .is_some_and(|counts| counts.iter().any(|row| {
                row["table"].as_str() == Some("workspaces")
                    && row["rows"].as_u64().unwrap_or(0) >= 1
            })),
        "expected real workspaces row count in {status_report}"
    );

    let (inspect_exit, inspect) = run_cli(vec![
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
    assert_eq!(inspect_exit, ee::models::ProcessExitCode::Success);
    assert_eq!(
        inspect["data"]["command"],
        Value::String("db inspect".into())
    );
    let inspect_report = &inspect["data"]["report"];
    assert_eq!(inspect_report["table"], Value::String("workspaces".into()));
    assert_eq!(inspect_report["exists"], Value::Bool(true));
    assert_eq!(inspect_report["returnedRowCount"], Value::from(1));
    assert!(
        inspect_report["columns"]
            .as_array()
            .is_some_and(|columns| columns.iter().any(|column| column.as_str() == Some("id"))),
        "expected workspaces.id column in {inspect_report}"
    );
    let first_row = inspect_report["rows"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("one inspected workspace row");
    assert!(first_row["values"]["id"].as_str().is_some());
    assert!(
        first_row.get("sourceUri").is_some(),
        "inspected rows must carry sourceUri provenance key"
    );

    let (check_exit, check) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("db"),
        OsString::from("check-integrity"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    assert_eq!(check_exit, ee::models::ProcessExitCode::Success);
    assert_eq!(
        check["data"]["command"],
        Value::String("db check-integrity".into())
    );
    let check_report = &check["data"]["report"];
    assert_eq!(check_report["passed"], Value::Bool(true));
    assert_eq!(
        check_report["checkType"],
        Value::String("integrity_check".into())
    );
    assert_eq!(check_report["integrityPassed"], Value::Bool(true));
    assert_eq!(check_report["foreignKeyPassed"], Value::Bool(true));
    assert_eq!(check["degraded"], Value::Array(Vec::new()));
}
