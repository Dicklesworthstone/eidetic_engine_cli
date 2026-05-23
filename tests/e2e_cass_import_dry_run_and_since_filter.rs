//! Perfect-E2E coverage for `ee import cass --dry-run` and
//! `ee import cass --since <duration>`.
//!
//! These two flags are the agent-facing safety levers for CASS import. Existing
//! cass-import E2E tests (`tests/cass_import_concurrency.rs`,
//! `tests/e2e_cass_import_redaction.rs`) exercise only the happy-path
//! persistent import. This file fills the gap with real-service coverage that:
//!
//! * stands up a real SQLite ee.db via [`DbConnection::open`] + `migrate`;
//! * drives the real `ee` binary against a faked `cass` shell binary (the only
//!   way to run hermetically without a production CASS install — every other
//!   assertion is against real subsystems);
//! * emits `ee.test_event.v1` JSONL breadcrumbs at each phase so the failure
//!   mode is debuggable from the log without re-running.
//!
//! Bead: bd-3njz9.

#![cfg(unix)]

use std::ffi::OsString;
use std::fmt::Debug;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};
use ee::db::{CreateWorkspaceInput, DatabaseConfig, DbConnection};
use ee::models::id::WorkspaceId;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

type TestResult = Result<(), String>;

const BEAD_ID: &str = "bd-3njz9";
const TEST_EVENT_SCHEMA: &str = "ee.test_event.v1";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestEvent<'a> {
    schema: &'static str,
    ts: String,
    bead_id: &'static str,
    scenario: &'static str,
    phase: &'static str,
    status: &'static str,
    scrubbed_workspace_path_hash: String,
    details: &'a Value,
}

#[test]
fn dry_run_import_creates_no_persistent_state() -> TestResult {
    let scenario = "dry_run_import_creates_no_persistent_state";
    let root = unique_artifact_dir(scenario)?;
    let log_path = root.join("test-events.jsonl");
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    set_dir_executable(&fake_bin_dir)?;

    // The fake `cass sessions` emits two sessions that straddle a date boundary.
    // Dry-run should report both as `would_import` regardless of timestamp; the
    // since filter is exercised in the second test.
    let old_path = workspace.join("session-2020-old.jsonl");
    let new_path = workspace.join("session-recent.jsonl");
    fs::write(&old_path, "{}\n").map_err(|error| error.to_string())?;
    fs::write(&new_path, "{}\n").map_err(|error| error.to_string())?;

    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_multi_session_binary(&cass_binary)?;

    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let old_path = old_path.canonicalize().map_err(|error| error.to_string())?;
    let new_path = new_path.canonicalize().map_err(|error| error.to_string())?;
    let database = root.join("ee.db");

    let scrub = scrubbed_workspace_hash(&workspace);
    emit_event(
        &log_path,
        scenario,
        "setup",
        "pass",
        &scrub,
        &json!({
            "database_exists_before_run": database.exists(),
            "old_session_path": old_path.display().to_string(),
            "new_session_path": new_path.display().to_string(),
        }),
    )?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let cass_binary_arg = cass_binary.to_string_lossy().into_owned();
    let path_env = path_with_fake_cass(&fake_bin_dir)?;

    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args([
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "import",
            "cass",
            "--database",
            database_arg.as_str(),
            "--limit",
            "10",
            "--dry-run",
            "--no-spans",
        ])
        .env("PATH", &path_env)
        .env("EE_CASS_BINARY", &cass_binary_arg)
        .env(
            "EE_FAKE_SESSION_OLD_PATH",
            old_path.to_string_lossy().as_ref(),
        )
        .env("EE_FAKE_SESSION_OLD_TS", "2020-01-01T00:00:00Z")
        .env(
            "EE_FAKE_SESSION_NEW_PATH",
            new_path.to_string_lossy().as_ref(),
        )
        .env("EE_FAKE_SESSION_NEW_TS", format_now_minus_hours(1))
        .env("EE_FAKE_SESSION_WORKSPACE", workspace_arg.as_str())
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee import cass --dry-run: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    emit_event(
        &log_path,
        scenario,
        "run_dry_run",
        if output.status.success() {
            "pass"
        } else {
            "fail"
        },
        &scrub,
        &json!({
            "exit_code": output.status.code(),
            "stdout_bytes": stdout.len(),
            "stderr_bytes": stderr.len(),
        }),
    )?;

    ensure(
        output.status.success(),
        format!("dry-run import must succeed; stderr: {stderr}; stdout: {stdout}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("dry-run stderr must stay clean; got: {stderr}"),
    )?;

    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("dry-run stdout must be JSON: {error}; stdout: {stdout}"))?;
    ensure_equal(
        &report["schema"],
        &json!("ee.response.v2"),
        "dry-run response schema",
    )?;
    ensure_equal(&report["success"], &json!(true), "dry-run response success")?;
    ensure_equal(
        &report["data"]["dryRun"],
        &json!(true),
        "dry-run data.dryRun",
    )?;
    ensure_equal(
        &report["data"]["status"],
        &json!("dry_run"),
        "dry-run data.status",
    )?;
    ensure_equal(
        &report["data"]["databasePath"],
        &Value::Null,
        "dry-run data.databasePath must be null (no DB was opened)",
    )?;
    ensure_equal(
        &report["data"]["ledgerId"],
        &Value::Null,
        "dry-run data.ledgerId must be null (no ledger row was inserted)",
    )?;
    ensure_equal(
        &report["data"]["sessionsImported"],
        &json!(0),
        "dry-run data.sessionsImported",
    )?;
    ensure_equal(
        &report["data"]["spansImported"],
        &json!(0),
        "dry-run data.spansImported",
    )?;
    ensure_equal(
        &report["data"]["indexJobsQueued"],
        &json!(0),
        "dry-run data.indexJobsQueued",
    )?;

    let sessions = report["data"]["sessions"]
        .as_array()
        .ok_or_else(|| "dry-run data.sessions must be an array".to_string())?;
    ensure_equal(
        &sessions.len(),
        &2,
        "dry-run should report both fake sessions as would-import",
    )?;
    for (index, session) in sessions.iter().enumerate() {
        ensure_equal(
            &session["status"],
            &json!("would_import"),
            &format!("dry-run sessions[{index}].status"),
        )?;
        ensure_equal(
            &session["sessionId"],
            &Value::Null,
            &format!("dry-run sessions[{index}].sessionId must be null"),
        )?;
        ensure_equal(
            &session["indexJobId"],
            &Value::Null,
            &format!("dry-run sessions[{index}].indexJobId must be null"),
        )?;
    }

    emit_event(
        &log_path,
        scenario,
        "verify_dry_run_response",
        "pass",
        &scrub,
        &json!({
            "sessions_reported": sessions.len(),
            "would_import_count": sessions.iter().filter(|s| s["status"] == "would_import").count(),
        }),
    )?;

    // The strongest dry-run guarantee: no database file at all should exist.
    // The CLI computes a database_path relative to the workspace, but dry-run
    // returns before `ensure_database_parent` or `DbConnection::open`. If a
    // regression opens the DB even in dry-run, the file will appear on disk.
    ensure(
        !database.exists(),
        format!(
            "dry-run must NOT create the database file at {}",
            database.display()
        ),
    )?;
    let default_db = workspace.join(".ee").join("ee.db");
    ensure(
        !default_db.exists(),
        format!(
            "dry-run must NOT create the default database at {}",
            default_db.display()
        ),
    )?;

    emit_event(
        &log_path,
        scenario,
        "verify_dry_run_db",
        "pass",
        &scrub,
        &json!({
            "explicit_database_present": database.exists(),
            "default_database_present": default_db.exists(),
            "guarantee": "dry-run never opens DbConnection",
        }),
    )?;

    assert_required_phases(
        &log_path,
        &[
            "setup",
            "run_dry_run",
            "verify_dry_run_response",
            "verify_dry_run_db",
        ],
    )?;
    Ok(())
}

#[test]
fn since_filter_drops_pre_cutoff_sessions() -> TestResult {
    let scenario = "since_filter_drops_pre_cutoff_sessions";
    let root = unique_artifact_dir(scenario)?;
    let log_path = root.join("test-events.jsonl");
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    set_dir_executable(&fake_bin_dir)?;

    let old_path = workspace.join("session-pre-cutoff.jsonl");
    let new_path = workspace.join("session-post-cutoff.jsonl");
    fs::write(&old_path, "{}\n").map_err(|error| error.to_string())?;
    fs::write(&new_path, "{}\n").map_err(|error| error.to_string())?;

    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_multi_session_binary(&cass_binary)?;

    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let old_path = old_path.canonicalize().map_err(|error| error.to_string())?;
    let new_path = new_path.canonicalize().map_err(|error| error.to_string())?;
    let database = root.join("ee.db");
    precreate_workspace_database(&database, &workspace)?;

    let scrub = scrubbed_workspace_hash(&workspace);
    emit_event(
        &log_path,
        scenario,
        "setup",
        "pass",
        &scrub,
        &json!({
            "database_path": database.display().to_string(),
            "old_session_path": old_path.display().to_string(),
            "new_session_path": new_path.display().to_string(),
        }),
    )?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let cass_binary_arg = cass_binary.to_string_lossy().into_owned();
    let path_env = path_with_fake_cass(&fake_bin_dir)?;
    let new_ts = format_now_minus_hours(2);

    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args([
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "import",
            "cass",
            "--database",
            database_arg.as_str(),
            "--limit",
            "10",
            "--since",
            "24h",
            "--no-spans",
        ])
        .env("PATH", &path_env)
        .env("EE_CASS_BINARY", &cass_binary_arg)
        .env(
            "EE_FAKE_SESSION_OLD_PATH",
            old_path.to_string_lossy().as_ref(),
        )
        .env("EE_FAKE_SESSION_OLD_TS", "2020-01-01T00:00:00Z")
        .env(
            "EE_FAKE_SESSION_NEW_PATH",
            new_path.to_string_lossy().as_ref(),
        )
        .env("EE_FAKE_SESSION_NEW_TS", new_ts.as_str())
        .env("EE_FAKE_SESSION_WORKSPACE", workspace_arg.as_str())
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee import cass --since 24h: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    emit_event(
        &log_path,
        scenario,
        "run_since_filter",
        if output.status.success() {
            "pass"
        } else {
            "fail"
        },
        &scrub,
        &json!({
            "exit_code": output.status.code(),
            "stdout_bytes": stdout.len(),
            "stderr_bytes": stderr.len(),
            "fake_new_ts": new_ts,
        }),
    )?;

    ensure(
        output.status.success(),
        format!("--since import must succeed; stderr: {stderr}; stdout: {stdout}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("--since stderr must stay clean; got: {stderr}"),
    )?;

    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("--since stdout must be JSON: {error}; stdout: {stdout}"))?;
    ensure_equal(
        &report["schema"],
        &json!("ee.response.v2"),
        "since response schema",
    )?;
    ensure_equal(&report["success"], &json!(true), "since response success")?;
    ensure_equal(
        &report["data"]["dryRun"],
        &json!(false),
        "since data.dryRun",
    )?;
    ensure_equal(
        &report["data"]["status"],
        &json!("completed"),
        "since data.status",
    )?;

    // `data.since` must be a valid RFC3339-Z string set to ~24h ago.
    let since_str = report["data"]["since"]
        .as_str()
        .ok_or_else(|| "since data.since must be a string".to_string())?;
    let since_parsed: DateTime<Utc> = DateTime::parse_from_rfc3339(since_str)
        .map_err(|error| format!("data.since must parse as RFC3339: {error}; got `{since_str}`"))?
        .with_timezone(&Utc);
    let now = Utc::now();
    ensure(
        since_parsed <= now,
        format!("data.since {since_str} must be <= now {now}"),
    )?;
    // Sanity: should be within (now - 25h, now - 23h) — 24h ± 1h slack.
    let lower = now - chrono::Duration::hours(25);
    let upper = now - chrono::Duration::hours(23);
    ensure(
        since_parsed >= lower && since_parsed <= upper,
        format!(
            "data.since {since_str} should be within ~24h of now ({} .. {})",
            lower.to_rfc3339(),
            upper.to_rfc3339()
        ),
    )?;

    // Discovered count is the POST-filter count (only the new session survived).
    let discovered = report["data"]["sessionsDiscovered"]
        .as_u64()
        .ok_or_else(|| "since data.sessionsDiscovered must be u64".to_string())?;
    ensure_equal(
        &discovered,
        &1,
        "since filter must drop the pre-cutoff session before discovery counting",
    )?;
    let imported = report["data"]["sessionsImported"]
        .as_u64()
        .ok_or_else(|| "since data.sessionsImported must be u64".to_string())?;
    ensure_equal(
        &imported,
        &1,
        "since must import exactly one (post-cutoff) session",
    )?;
    let skipped = report["data"]["sessionsSkipped"]
        .as_u64()
        .ok_or_else(|| "since data.sessionsSkipped must be u64".to_string())?;
    ensure_equal(&skipped, &0, "fresh DB has nothing to skip")?;
    let sessions = report["data"]["sessions"]
        .as_array()
        .ok_or_else(|| "since data.sessions must be array".to_string())?;
    ensure_equal(&sessions.len(), &1, "since reports one persisted session")?;
    let only = &sessions[0];
    ensure_equal(&only["status"], &json!("imported"), "since session status")?;
    let reported_path = only["sourcePath"]
        .as_str()
        .ok_or_else(|| "since session sourcePath must be string".to_string())?;
    // sourcePath is REDACTED by data_json; raw path comparison would fail.
    // Instead assert the redaction sentinel landed and the OLD path's basename
    // is NOT present in the reported session list.
    ensure(
        reported_path.contains("[REDACTED_PATH]") || reported_path.contains("session-post-cutoff"),
        format!(
            "reported sourcePath should reference the post-cutoff session or be redacted; got {reported_path}"
        ),
    )?;
    let full_payload = report.to_string();
    ensure(
        !full_payload.contains("session-pre-cutoff"),
        format!("--since report must not mention the pre-cutoff session: {full_payload}"),
    )?;

    emit_event(
        &log_path,
        scenario,
        "verify_since_response",
        "pass",
        &scrub,
        &json!({
            "since_string": since_str,
            "sessions_discovered": discovered,
            "sessions_imported": imported,
        }),
    )?;

    // Verify the real DB: only one session row should exist, and it must be
    // the post-cutoff one. The redacted source path travels through the
    // import; only the workspace-relative basename remains in the stored
    // `cass_session_id`.
    let connection = DbConnection::open(DatabaseConfig::file(database.clone()))
        .map_err(|error| error.to_string())?;
    let workspace_id = stable_workspace_id(&workspace_arg);
    let stored_sessions = connection
        .list_sessions(&workspace_id)
        .map_err(|error| error.to_string())?;
    ensure_equal(
        &stored_sessions.len(),
        &1,
        "exactly one session row in real DB",
    )?;
    let stored = &stored_sessions[0];
    ensure(
        stored.cass_session_id.contains("session-post-cutoff"),
        format!(
            "stored cass_session_id should reference post-cutoff source; got {}",
            stored.cass_session_id
        ),
    )?;
    ensure(
        !stored.cass_session_id.contains("session-pre-cutoff"),
        format!(
            "stored cass_session_id must NOT reference pre-cutoff source; got {}",
            stored.cass_session_id
        ),
    )?;
    let ledgers = connection
        .list_import_ledgers(&workspace_id)
        .map_err(|error| error.to_string())?;
    ensure_equal(&ledgers.len(), &1, "exactly one import ledger row")?;
    let ledger = &ledgers[0];
    ensure_equal(
        &ledger.status.as_str(),
        &"completed",
        "import ledger status",
    )?;
    ensure_equal(
        &u64::from(ledger.imported_session_count),
        &1,
        "ledger imported_session_count matches report",
    )?;
    connection.close().map_err(|error| error.to_string())?;

    emit_event(
        &log_path,
        scenario,
        "verify_since_db",
        "pass",
        &scrub,
        &json!({
            "stored_session_count": stored_sessions.len(),
            "stored_cass_session_id_excerpt": &stored.cass_session_id,
            "ledger_status": ledger.status.as_str(),
            "ledger_imported_session_count": ledger.imported_session_count,
        }),
    )?;

    assert_required_phases(
        &log_path,
        &[
            "setup",
            "run_since_filter",
            "verify_since_response",
            "verify_since_db",
        ],
    )?;
    Ok(())
}

// --- helpers ---

fn write_fake_cass_multi_session_binary(path: &Path) -> TestResult {
    // The script emits a two-session manifest for `cass sessions` and a minimal
    // empty span list for `cass view`. Timestamps and source paths are driven
    // by env vars so the same script supports the dry-run and since tests.
    let script = r#"#!/bin/sh
set -eu
cmd="${1:-}"
case "$cmd" in
  sessions)
    printf '{"sessions":[{"path":"%s","workspace":"%s","agent":"codex","started_at":"%s","message_count":3,"token_count":42,"content_hash":"hash-old"},{"path":"%s","workspace":"%s","agent":"codex","started_at":"%s","message_count":5,"token_count":120,"content_hash":"hash-new"}]}\n' \
      "$EE_FAKE_SESSION_OLD_PATH" "$EE_FAKE_SESSION_WORKSPACE" "$EE_FAKE_SESSION_OLD_TS" \
      "$EE_FAKE_SESSION_NEW_PATH" "$EE_FAKE_SESSION_WORKSPACE" "$EE_FAKE_SESSION_NEW_TS"
    ;;
  view)
    printf '{"lines":[]}\n'
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

fn precreate_workspace_database(database: &Path, workspace: &Path) -> TestResult {
    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let workspace_path = workspace.to_string_lossy().into_owned();
    let connection = DbConnection::open(DatabaseConfig::file(database.to_path_buf()))
        .map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
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
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())
}

fn unique_artifact_dir(scenario: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = base
        .join("ee-cass-import-dry-run-since")
        .join(format!("{scenario}-{now}-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn set_dir_executable(dir: &Path) -> TestResult {
    let mut permissions = fs::metadata(dir)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(dir, permissions).map_err(|error| error.to_string())
}

fn path_with_fake_cass(fake_dir: &Path) -> Result<OsString, String> {
    let mut paths = vec![fake_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).map_err(|error| error.to_string())
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

fn scrubbed_workspace_hash(workspace: &Path) -> String {
    format!(
        "blake3:{}",
        blake3::hash(workspace.display().to_string().as_bytes()).to_hex()
    )
}

fn format_now_minus_hours(hours: i64) -> String {
    (Utc::now() - chrono::Duration::hours(hours)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn emit_event(
    log_path: &Path,
    scenario: &'static str,
    phase: &'static str,
    status: &'static str,
    scrub: &str,
    details: &Value,
) -> TestResult {
    let event = TestEvent {
        schema: TEST_EVENT_SCHEMA,
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        bead_id: BEAD_ID,
        scenario,
        phase,
        status,
        scrubbed_workspace_path_hash: scrub.to_string(),
        details,
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("open jsonl log {}: {error}", log_path.display()))?;
    serde_json::to_writer(&mut file, &event)
        .map_err(|error| format!("serialize test event: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("newline: {error}"))?;
    Ok(())
}

fn assert_required_phases(log_path: &Path, required: &[&str]) -> TestResult {
    let text = fs::read_to_string(log_path)
        .map_err(|error| format!("read jsonl log {}: {error}", log_path.display()))?;
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines() {
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("jsonl line must parse: {error}; line: {line}"))?;
        ensure_equal(
            &value["schema"],
            &json!(TEST_EVENT_SCHEMA),
            "every event has ee.test_event.v1 schema",
        )?;
        if let Some(phase) = value["phase"].as_str() {
            seen.insert(phase.to_string());
        }
    }
    for phase in required {
        ensure(
            seen.contains(*phase),
            format!("required phase `{phase}` missing from jsonl log; saw {seen:?}"),
        )?;
    }
    Ok(())
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
