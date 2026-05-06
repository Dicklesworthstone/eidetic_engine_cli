#![cfg(unix)]

use std::ffi::OsString;
use std::fmt::Debug;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use ee::db::{CreateWorkspaceInput, DatabaseConfig, DbConnection};
use ee::models::id::WorkspaceId;
use uuid::Uuid;

type TestResult = Result<(), String>;

const IMPORT_PROCESS_COUNT: usize = 2;

#[test]
fn parallel_cass_imports_preserve_ledger_counters() -> TestResult {
    let root = unique_artifact_dir("parallel-cass-import")?;
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    let mut bin_permissions = fs::metadata(&fake_bin_dir)
        .map_err(|error| error.to_string())?
        .permissions();
    bin_permissions.set_mode(0o755);
    fs::set_permissions(&fake_bin_dir, bin_permissions).map_err(|error| error.to_string())?;

    let session_path = workspace.join("session-a.jsonl");
    fs::write(&session_path, "{}\n").map_err(|error| error.to_string())?;
    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;

    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let session_path = session_path
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = root.join("ee.db");
    precreate_workspace_database(&database, &workspace)?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let session_arg = session_path.to_string_lossy().into_owned();
    let cass_binary_arg = cass_binary.to_string_lossy().into_owned();
    let path = path_with_fake_cass(&fake_bin_dir)?;
    let start = Arc::new(Barrier::new(IMPORT_PROCESS_COUNT));

    let handles: Vec<_> = (0..IMPORT_PROCESS_COUNT)
        .map(|_| {
            spawn_import_process(
                Arc::clone(&start),
                workspace_arg.clone(),
                database_arg.clone(),
                session_arg.clone(),
                cass_binary_arg.clone(),
                path.clone(),
            )
        })
        .collect();

    let mut reports = Vec::with_capacity(IMPORT_PROCESS_COUNT);
    for (index, handle) in handles.into_iter().enumerate() {
        let output = handle
            .join()
            .map_err(|_| format!("import subprocess thread {index} panicked"))??;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure(
            !stderr.contains("panicked") && !stderr.contains("thread '"),
            format!("import subprocess {index} must not panic; stderr: {stderr}"),
        )?;
        ensure(
            output.status.success(),
            format!(
                "import subprocess {index} should succeed; stdout: {}; stderr: {stderr}",
                String::from_utf8_lossy(&output.stdout),
            ),
        )?;
        ensure(
            output.stderr.is_empty(),
            format!("import subprocess {index} stderr must stay clean: {stderr}"),
        )?;
        let report: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                format!(
                    "import subprocess {index} stdout must be JSON: {error}; stdout: {}",
                    String::from_utf8_lossy(&output.stdout)
                )
            })?;
        ensure_equal(
            &report["schema"],
            &serde_json::json!("ee.response.v1"),
            "response schema",
        )?;
        ensure_equal(
            &report["success"],
            &serde_json::json!(true),
            "response success",
        )?;
        reports.push(report);
    }

    let sessions_imported = sum_report_count(&reports, "sessionsImported")?;
    let sessions_skipped = sum_report_count(&reports, "sessionsSkipped")?;
    let spans_imported = sum_report_count(&reports, "spansImported")?;

    ensure_equal(&sessions_imported, &1, "exactly one session is imported")?;
    ensure_equal(
        &sessions_skipped,
        &(IMPORT_PROCESS_COUNT as u64 - 1),
        "remaining imports skip the existing session",
    )?;
    ensure_equal(&spans_imported, &1, "exactly one span is imported")?;

    let connection =
        DbConnection::open(DatabaseConfig::file(database.clone())).map_err(|e| e.to_string())?;
    let workspace_id = stable_workspace_id(&workspace_arg);
    let sessions = connection
        .list_sessions(&workspace_id)
        .map_err(|e| e.to_string())?;
    let ledgers = connection
        .list_import_ledgers(&workspace_id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&sessions.len(), &1, "stored sessions")?;
    ensure_equal(&ledgers.len(), &1, "import ledgers")?;
    let ledger = ledgers
        .first()
        .ok_or_else(|| "import ledger should exist".to_string())?;
    ensure_equal(&ledger.status.as_str(), &"completed", "ledger status")?;
    ensure_equal(
        &u64::from(ledger.attempt_count),
        &(IMPORT_PROCESS_COUNT as u64),
        "ledger attempt count",
    )?;
    ensure_equal(
        &u64::from(ledger.imported_session_count),
        &sessions_imported,
        "ledger imported session count equals subprocess contributions",
    )?;
    ensure_equal(
        &u64::from(ledger.imported_span_count),
        &spans_imported,
        "ledger imported span count equals subprocess contributions",
    )?;
    connection.close().map_err(|e| e.to_string())
}

fn spawn_import_process(
    start: Arc<Barrier>,
    workspace_arg: String,
    database_arg: String,
    session_arg: String,
    cass_binary_arg: String,
    path: OsString,
) -> thread::JoinHandle<Result<Output, String>> {
    thread::spawn(move || {
        start.wait();
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .args([
                "--workspace",
                workspace_arg.as_str(),
                "--json",
                "import",
                "cass",
                "--database",
                database_arg.as_str(),
                "--limit",
                "1",
            ])
            .env("PATH", path)
            .env("EE_CASS_BINARY", cass_binary_arg)
            .env("EE_FAKE_CASS_SESSION", session_arg)
            .env("EE_FAKE_CASS_WORKSPACE", workspace_arg)
            .env("EE_FAKE_CASS_SLEEP_SECS", "1")
            .env("NO_COLOR", "1")
            .output()
            .map_err(|error| format!("failed to run ee import cass: {error}"))
    })
}

fn precreate_workspace_database(database: &Path, workspace: &Path) -> TestResult {
    let Some(parent) = database.parent() else {
        return Err(format!(
            "database path has no parent: {}",
            database.display()
        ));
    };
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;

    let workspace_path = workspace.to_string_lossy().into_owned();
    let connection = DbConnection::open(DatabaseConfig::file(database.to_path_buf()))
        .map_err(|e| e.to_string())?;
    connection.migrate().map_err(|e| e.to_string())?;
    let workspace_id = stable_workspace_id(&workspace_path);
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace_path,
                name: workspace
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        )
        .map_err(|e| e.to_string())?;
    connection.close().map_err(|e| e.to_string())
}

fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    Ok(base
        .join("ee-cass-import-concurrency")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

fn path_with_fake_cass(fake_dir: &Path) -> Result<OsString, String> {
    let mut entries = vec![fake_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(entries).map_err(|error| error.to_string())
}

fn write_fake_cass_binary(path: &Path) -> TestResult {
    let script = r#"#!/bin/sh
set -eu
cmd="${1:-}"
case "$cmd" in
  sessions)
    sleep_secs="${EE_FAKE_CASS_SLEEP_SECS:-}"
    if [ -n "$sleep_secs" ]; then
      sleep "$sleep_secs"
    fi
    printf '{"sessions":[{"path":"%s","workspace":"%s","agent":"codex","started_at":"2026-04-30T00:00:00Z","message_count":2,"token_count":42,"content_hash":"hash-session-a"}]}\n' "$EE_FAKE_CASS_SESSION" "$EE_FAKE_CASS_WORKSPACE"
    ;;
  view)
    printf '{"lines":[{"line":1,"content":"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"remember this\"}}"}]}\n'
    ;;
  *)
    echo "unexpected cass command: $cmd" >&2
    exit 64
    ;;
esac
"#;
    fs::write(path, script).map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

fn sum_report_count(reports: &[serde_json::Value], field: &str) -> Result<u64, String> {
    reports
        .iter()
        .map(|report| {
            report["data"][field]
                .as_u64()
                .ok_or_else(|| format!("report field data.{field} must be an unsigned integer"))
        })
        .sum()
}

fn stable_workspace_id(path: &str) -> String {
    WorkspaceId::from_uuid(stable_uuid(&format!("workspace:{path}"))).to_string()
}

fn stable_uuid(input: &str) -> Uuid {
    let hash = blake3::hash(input.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    Uuid::from_bytes(bytes)
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}
