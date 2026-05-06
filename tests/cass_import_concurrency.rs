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
fn cass_import_redacts_sensitive_spans_and_rerun_is_idempotent() -> TestResult {
    let root = unique_artifact_dir("cass-import-redaction")?;
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    let mut bin_permissions = fs::metadata(&fake_bin_dir)
        .map_err(|error| error.to_string())?
        .permissions();
    bin_permissions.set_mode(0o755);
    fs::set_permissions(&fake_bin_dir, bin_permissions).map_err(|error| error.to_string())?;

    let session_path = workspace.join("session-pii.jsonl");
    fs::write(&session_path, "{}\n").map_err(|error| error.to_string())?;
    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;

    let sensitive = SensitiveCassFixture::new();
    let view_json_path = root.join("cass-view.json");
    fs::write(
        &view_json_path,
        serde_json::json!({
            "lines": [{
                "line": 1,
                "content": sensitive.session_line,
            }]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

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
    let view_json_arg = view_json_path.to_string_lossy().into_owned();
    let path = path_with_fake_cass(&fake_bin_dir)?;

    let first = run_import_once(
        &workspace_arg,
        &database_arg,
        &session_arg,
        &cass_binary_arg,
        &view_json_arg,
        path.clone(),
    )?;
    ensure_equal(
        &report_count(&first, "sessionsImported")?,
        &1,
        "first import session count",
    )?;
    ensure_equal(
        &report_count(&first, "spansImported")?,
        &1,
        "first import span count",
    )?;
    ensure_import_output_omits_raw_values(&first, &sensitive.raw_values)?;

    let second = run_import_once(
        &workspace_arg,
        &database_arg,
        &session_arg,
        &cass_binary_arg,
        &view_json_arg,
        path,
    )?;
    ensure_equal(
        &report_count(&second, "sessionsImported")?,
        &0,
        "second import session count",
    )?;
    ensure_equal(
        &report_count(&second, "sessionsSkipped")?,
        &1,
        "second import skipped count",
    )?;
    ensure_equal(
        &report_count(&second, "spansImported")?,
        &0,
        "second import span count",
    )?;
    ensure_import_output_omits_raw_values(&second, &sensitive.raw_values)?;

    let connection =
        DbConnection::open(DatabaseConfig::file(database.clone())).map_err(|e| e.to_string())?;
    let workspace_id = stable_workspace_id(&workspace_arg);
    let sessions = connection
        .list_sessions(&workspace_id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&sessions.len(), &1, "stored session count after rerun")?;
    let spans = connection
        .list_evidence_spans_for_session(&sessions[0].id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&spans.len(), &1, "stored redacted span count after rerun")?;
    let span = spans
        .first()
        .ok_or_else(|| "redacted evidence span should exist".to_string())?;
    for raw in &sensitive.raw_values {
        ensure(
            !span.excerpt.contains(raw),
            format!("stored span leaked raw sensitive value {raw:?}"),
        )?;
    }
    for placeholder in &sensitive.placeholders {
        ensure_equal(
            &occurrences(&span.excerpt, placeholder),
            &1,
            &format!("placeholder appears exactly once: {placeholder}"),
        )?;
    }

    let metadata: serde_json::Value = serde_json::from_str(
        span.metadata_json
            .as_deref()
            .ok_or_else(|| "span metadata missing".to_string())?,
    )
    .map_err(|error| format!("span metadata should be JSON: {error}"))?;
    ensure_equal(
        &metadata["redactionStatus"],
        &serde_json::json!("redacted"),
        "span redaction metadata",
    )?;
    for class in &sensitive.redaction_classes {
        ensure(
            metadata["redactionClasses"]
                .as_array()
                .is_some_and(|classes| classes.iter().any(|actual| actual == class)),
            format!("span metadata missing redaction class {class}"),
        )?;
    }

    let audits = connection
        .list_audit_by_action("cass.evidence.redacted", None)
        .map_err(|e| e.to_string())?;
    ensure_equal(&audits.len(), &1, "redaction audit count after rerun")?;
    let audit = audits
        .first()
        .ok_or_else(|| "redaction audit should exist".to_string())?;
    ensure_equal(
        &audit.target_type.as_deref(),
        &Some("evidence_span"),
        "redaction audit target type",
    )?;
    ensure_equal(
        &audit.target_id.as_deref(),
        &Some(span.id.as_str()),
        "redaction audit target id",
    )?;
    let audit_details: serde_json::Value = serde_json::from_str(
        audit
            .details
            .as_deref()
            .ok_or_else(|| "audit details missing".to_string())?,
    )
    .map_err(|error| format!("audit details should be JSON: {error}"))?;
    ensure_equal(
        &audit_details["schema"],
        &serde_json::json!("ee.cass.redaction_audit.v1"),
        "redaction audit schema",
    )?;
    for raw in &sensitive.raw_values {
        ensure(
            !audit.details.as_deref().unwrap_or_default().contains(raw),
            format!("redaction audit leaked raw sensitive value {raw:?}"),
        )?;
    }
    connection.close().map_err(|e| e.to_string())?;

    ensure_database_files_omit_raw_values(&database, &sensitive.raw_values)
}

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

fn run_import_once(
    workspace_arg: &str,
    database_arg: &str,
    session_arg: &str,
    cass_binary_arg: &str,
    view_json_arg: &str,
    path: OsString,
) -> Result<serde_json::Value, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args([
            "--workspace",
            workspace_arg,
            "--json",
            "import",
            "cass",
            "--database",
            database_arg,
            "--limit",
            "1",
        ])
        .env("PATH", path)
        .env("EE_CASS_BINARY", cass_binary_arg)
        .env("EE_FAKE_CASS_SESSION", session_arg)
        .env("EE_FAKE_CASS_WORKSPACE", workspace_arg)
        .env("EE_FAKE_CASS_VIEW_JSON_PATH", view_json_arg)
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee import cass: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        format!(
            "import should succeed; stdout: {}; stderr: {stderr}",
            String::from_utf8_lossy(&output.stdout),
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!("import stderr clean: {stderr}"),
    )?;
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("import stdout should be JSON: {error}"))
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
    if [ -n "${EE_FAKE_CASS_VIEW_JSON_PATH:-}" ]; then
      cat "$EE_FAKE_CASS_VIEW_JSON_PATH"
      exit 0
    fi
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

fn report_count(report: &serde_json::Value, field: &str) -> Result<u64, String> {
    report["data"][field]
        .as_u64()
        .ok_or_else(|| format!("report field data.{field} must be an unsigned integer"))
}

fn ensure_import_output_omits_raw_values(
    report: &serde_json::Value,
    raw_values: &[String],
) -> TestResult {
    let output = report.to_string();
    for raw in raw_values {
        ensure(
            !output.contains(raw),
            format!("import output leaked raw sensitive value {raw:?}"),
        )?;
    }
    Ok(())
}

fn ensure_database_files_omit_raw_values(database: &Path, raw_values: &[String]) -> TestResult {
    let mut paths = vec![database.to_path_buf()];
    paths.push(database.with_file_name("ee.db-wal"));
    paths.push(database.with_file_name("ee.db-shm"));

    for path in paths {
        if !path.exists() {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| error.to_string())?;
        let haystack = String::from_utf8_lossy(&bytes);
        for raw in raw_values {
            ensure(
                !haystack.contains(raw),
                format!("{} leaked raw sensitive value {raw:?}", path.display()),
            )?;
        }
    }
    Ok(())
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

struct SensitiveCassFixture {
    session_line: String,
    raw_values: Vec<String>,
    placeholders: Vec<String>,
    redaction_classes: Vec<serde_json::Value>,
}

impl SensitiveCassFixture {
    fn new() -> Self {
        let email = ["cass-redaction", "@", "example", ".", "test"].concat();
        let api_key_value = ["api", "-", "fixture", "-", "alpha"].concat();
        let password_value = ["password", "-", "fixture", "-", "beta"].concat();
        let token_value = ["token", "-", "fixture", "-", "gamma"].concat();
        let secret_key_value = ["secret", "-", "fixture", "-", "delta"].concat();
        let ssh_key_value = ["ssh", "-", "fixture", "-", "epsilon"].concat();

        let content = format!(
            "contact={email} api_key={api_key_value} password={password_value} token={token_value} secret_key={secret_key_value} ssh_key={ssh_key_value}"
        );
        let session_line = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content,
            }
        })
        .to_string();

        Self {
            session_line,
            raw_values: vec![
                email,
                api_key_value,
                password_value,
                token_value,
                secret_key_value,
                ssh_key_value,
            ],
            placeholders: [
                "email_address",
                "api_key",
                "password",
                "token",
                "secret_key",
                "ssh_key",
            ]
            .into_iter()
            .map(|class| format!("[REDACTED:{class}]"))
            .collect(),
            redaction_classes: [
                "email_address",
                "api_key",
                "password",
                "token",
                "secret_key",
                "ssh_key",
            ]
            .into_iter()
            .map(serde_json::Value::from)
            .collect(),
        }
    }
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
