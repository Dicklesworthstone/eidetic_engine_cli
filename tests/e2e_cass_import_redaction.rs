#![cfg(unix)]

use std::ffi::OsString;
use std::fmt::Debug;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ee::db::{CreateWorkspaceInput, DatabaseConfig, DbConnection};
use ee::models::id::WorkspaceId;
use uuid::Uuid;

type TestResult = Result<(), String>;

#[test]
fn cass_import_redaction_round_trip_is_search_safe_and_idempotent() -> TestResult {
    let root = unique_artifact_dir("cass-redaction-round-trip")?;
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    set_executable_dir_permissions(&fake_bin_dir)?;

    let sensitive = SensitiveCassFixture::new();
    let session_path = workspace.join("session-redaction-round-trip.jsonl");
    fs::write(&session_path, format!("{}\n", sensitive.session_line))
        .map_err(|error| error.to_string())?;

    let view_json_path = root.join("cass-view.json");
    fs::write(
        &view_json_path,
        format!(
            "{}\n",
            serde_json::json!({
                "line": 1,
                "content": sensitive.session_line,
            })
        ),
    )
    .map_err(|error| error.to_string())?;

    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;

    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let session_path = session_path
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = root.join("ee.db");
    let index_dir = root.join("index");
    precreate_workspace_database(&database, &workspace)?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let index_arg = index_dir.to_string_lossy().into_owned();
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
        &path,
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
    ensure_output_omits_raw_values("first import", &first.to_string(), &sensitive.raw_values)?;

    let second = run_import_once(
        &workspace_arg,
        &database_arg,
        &session_arg,
        &cass_binary_arg,
        &view_json_arg,
        &path,
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
    ensure_output_omits_raw_values("second import", &second.to_string(), &sensitive.raw_values)?;

    let rebuild = run_ee_json(
        &workspace_arg,
        [
            "index",
            "rebuild",
            "--database",
            database_arg.as_str(),
            "--index-dir",
            index_arg.as_str(),
        ],
        "index rebuild",
    )?;
    ensure_success(&rebuild, "index rebuild")?;
    ensure_equal(
        json_field(&rebuild, &["data", "sessions_indexed"], "sessions indexed")?,
        &serde_json::json!(1),
        "sessions indexed",
    )?;

    let search = run_ee_json(
        &workspace_arg,
        [
            "search",
            "hash-cass-redaction-roundtrip codex",
            "--database",
            database_arg.as_str(),
            "--index-dir",
            index_arg.as_str(),
            "--limit",
            "5",
        ],
        "search imported session",
    )?;
    ensure_success(&search, "search imported session")?;
    ensure(
        json_field(&search, &["data", "resultCount"], "search result count")?
            .as_u64()
            .unwrap_or_default()
            > 0,
        format!("search should return imported session: {search}"),
    )?;
    ensure_output_omits_raw_values("search output", &search.to_string(), &sensitive.raw_values)?;

    let connection = DbConnection::open(DatabaseConfig::file(database.to_path_buf()))
        .map_err(|e| e.to_string())?;
    let workspace_id = stable_workspace_id(&workspace_arg);
    let sessions = connection
        .list_sessions(&workspace_id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&sessions.len(), &1, "stored session count after rerun")?;
    let session = sessions
        .first()
        .ok_or_else(|| "stored session should exist after rerun".to_string())?;
    let spans = connection
        .list_evidence_spans_for_session(&session.id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&spans.len(), &1, "stored span count after rerun")?;
    let span = spans
        .first()
        .ok_or_else(|| "redacted evidence span should exist".to_string())?;
    ensure_output_omits_raw_values(
        "stored span excerpt",
        &span.excerpt,
        &sensitive.secret_values,
    )?;
    for placeholder in &sensitive.secret_placeholders {
        ensure(
            span.excerpt.contains(placeholder),
            format!("stored span should contain redaction placeholder {placeholder}"),
        )?;
    }

    let metadata: serde_json::Value = serde_json::from_str(
        span.metadata_json
            .as_deref()
            .ok_or_else(|| "span metadata missing".to_string())?,
    )
    .map_err(|error| format!("span metadata should be JSON: {error}"))?;
    ensure_equal(
        json_field(&metadata, &["redactionStatus"], "span redaction metadata")?,
        &serde_json::json!("redacted"),
        "span redaction metadata",
    )?;

    let audits = connection
        .list_audit_by_action("cass.evidence.redacted", None)
        .map_err(|e| e.to_string())?;
    ensure_equal(&audits.len(), &1, "redaction audit count after rerun")?;
    let audit = audits
        .first()
        .ok_or_else(|| "redaction audit should exist".to_string())?;
    ensure_equal(
        &audit.target_id.as_deref(),
        &Some(span.id.as_str()),
        "redaction audit target id",
    )?;
    connection.close().map_err(|e| e.to_string())?;

    ensure_database_files_omit_raw_values(&database, &sensitive.secret_values)
}

#[test]
fn cass_import_fake_subprocess_supervision_logs_success_cap_and_timeout() -> TestResult {
    let root = unique_artifact_dir("cass-supervision-logged")?;
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    set_executable_dir_permissions(&fake_bin_dir)?;

    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;

    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = root.join("ee.db");
    precreate_workspace_database(&database, &workspace)?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let cass_binary_arg = cass_binary.to_string_lossy().into_owned();
    let path = path_with_fake_cass(&fake_bin_dir)?;
    let log_path = root.join("cass-supervision-events.jsonl");

    let success_session_path = workspace.join("session-supervision-success.jsonl");
    let success_line = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": "cass supervision success marker",
        }
    })
    .to_string();
    fs::write(&success_session_path, format!("{success_line}\n"))
        .map_err(|error| error.to_string())?;
    let success_view_path = root.join("cass-view-success.jsonl");
    write_fake_view_jsonl(&success_view_path, &success_line)?;
    let success_session_arg = success_session_path.to_string_lossy().into_owned();
    let success_view_arg = success_view_path.to_string_lossy().into_owned();

    let success_run = run_import_command(
        &workspace_arg,
        &database_arg,
        &success_session_arg,
        &cass_binary_arg,
        &success_view_arg,
        &path,
        ImportRunOptions::default(),
    )?;
    let success_json = parse_json_output(&success_run.output, "supervision success import")?;
    ensure_success(&success_json, "supervision success import")?;
    append_supervision_event(
        &log_path,
        SupervisionEventInput {
            step: "streamed_jsonl_success",
            fake_cass_behavior: "sessions_success_view_jsonl_success",
            workspace_arg: &workspace_arg,
            view_json_arg: &success_view_arg,
            elapsed_ms: success_run.elapsed_ms,
            output: &success_run.output,
            response: &success_json,
        },
    )?;

    let cap_session_path = workspace.join("session-supervision-cap.jsonl");
    fs::write(&cap_session_path, format!("{success_line}\n")).map_err(|error| error.to_string())?;
    let cap_view_path = root.join("cass-view-cap.jsonl");
    write_oversized_stdout_line(&cap_view_path)?;
    let cap_marker = root.join("cap-marker");
    let cap_session_arg = cap_session_path.to_string_lossy().into_owned();
    let cap_view_arg = cap_view_path.to_string_lossy().into_owned();

    let cap_run = run_import_command(
        &workspace_arg,
        &database_arg,
        &cap_session_arg,
        &cass_binary_arg,
        &cap_view_arg,
        &path,
        ImportRunOptions {
            after_view_marker: Some(cap_marker.clone()),
            after_view_sleep_secs: Some(2),
            ..ImportRunOptions::default()
        },
    )?;
    ensure_error_output_contains(
        &cap_run.output,
        "supervision stdout cap import",
        "stdout line exceeded",
    )?;
    let cap_json = parse_stdout_json(&cap_run.output, "supervision stdout cap import")?;
    append_supervision_event(
        &log_path,
        SupervisionEventInput {
            step: "stdout_line_cap_error",
            fake_cass_behavior: "view_stdout_line_cap_then_sleep",
            workspace_arg: &workspace_arg,
            view_json_arg: &cap_view_arg,
            elapsed_ms: cap_run.elapsed_ms,
            output: &cap_run.output,
            response: &cap_json,
        },
    )?;
    std::thread::sleep(Duration::from_millis(1_200));
    ensure(
        !cap_marker.exists(),
        "stdout cap run should terminate fake cass before it writes the marker",
    )?;

    let timeout_session_path = workspace.join("session-supervision-timeout.jsonl");
    fs::write(&timeout_session_path, format!("{success_line}\n"))
        .map_err(|error| error.to_string())?;
    let timeout_view_path = root.join("cass-view-timeout.jsonl");
    write_fake_view_jsonl(&timeout_view_path, &success_line)?;
    let timeout_marker = root.join("timeout-marker");
    let timeout_session_arg = timeout_session_path.to_string_lossy().into_owned();
    let timeout_view_arg = timeout_view_path.to_string_lossy().into_owned();

    let timeout_run = run_import_command(
        &workspace_arg,
        &database_arg,
        &timeout_session_arg,
        &cass_binary_arg,
        &timeout_view_arg,
        &path,
        ImportRunOptions {
            subprocess_timeout_ms: Some(100),
            after_view_marker: Some(timeout_marker.clone()),
            after_view_sleep_secs: Some(2),
        },
    )?;
    ensure_error_output_contains(
        &timeout_run.output,
        "supervision timeout import",
        "cass view",
    )?;
    let timeout_json = parse_stdout_json(&timeout_run.output, "supervision timeout import")?;
    append_supervision_event(
        &log_path,
        SupervisionEventInput {
            step: "view_timeout_cleanup",
            fake_cass_behavior: "view_jsonl_then_sleep_past_timeout",
            workspace_arg: &workspace_arg,
            view_json_arg: &timeout_view_arg,
            elapsed_ms: timeout_run.elapsed_ms,
            output: &timeout_run.output,
            response: &timeout_json,
        },
    )?;
    std::thread::sleep(Duration::from_millis(1_200));
    ensure(
        !timeout_marker.exists(),
        "timeout run should terminate fake cass before it writes the marker",
    )?;

    assert_supervision_log(&log_path)
}

fn run_import_once(
    workspace_arg: &str,
    database_arg: &str,
    session_arg: &str,
    cass_binary_arg: &str,
    view_json_arg: &str,
    path: &OsString,
) -> Result<serde_json::Value, String> {
    let run = run_import_command(
        workspace_arg,
        database_arg,
        session_arg,
        cass_binary_arg,
        view_json_arg,
        path,
        ImportRunOptions::default(),
    )?;
    parse_json_output(&run.output, "import cass")
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ImportRunOptions {
    subprocess_timeout_ms: Option<u64>,
    after_view_marker: Option<PathBuf>,
    after_view_sleep_secs: Option<u64>,
}

#[derive(Debug)]
struct TimedOutput {
    output: Output,
    elapsed_ms: u128,
}

fn run_import_command(
    workspace_arg: &str,
    database_arg: &str,
    session_arg: &str,
    cass_binary_arg: &str,
    view_json_arg: &str,
    path: &OsString,
    options: ImportRunOptions,
) -> Result<TimedOutput, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args([
        "--workspace",
        workspace_arg,
        "--json",
        "import",
        "cass",
        "--database",
        database_arg,
        "--limit",
        "1",
    ]);
    if let Some(timeout_ms) = options.subprocess_timeout_ms {
        command
            .arg("--subprocess-timeout-ms")
            .arg(timeout_ms.to_string());
    }
    command
        .env("PATH", path)
        .env("EE_CASS_BINARY", cass_binary_arg)
        .env("EE_FAKE_CASS_SESSION", session_arg)
        .env("EE_FAKE_CASS_WORKSPACE", workspace_arg)
        .env("EE_FAKE_CASS_VIEW_JSON_PATH", view_json_arg)
        .env("NO_COLOR", "1");
    if let Some(marker) = options.after_view_marker {
        command.env("EE_FAKE_CASS_AFTER_VIEW_MARKER", marker);
    }
    if let Some(sleep_secs) = options.after_view_sleep_secs {
        command.env("EE_FAKE_CASS_AFTER_VIEW_SLEEP_SECS", sleep_secs.to_string());
    }

    let started = Instant::now();
    let output = command
        .output()
        .map_err(|error| format!("failed to run ee import cass: {error}"))?;
    Ok(TimedOutput {
        output,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn run_ee_json<'a, I>(
    workspace_arg: &str,
    args: I,
    context: &str,
) -> Result<serde_json::Value, String>
where
    I: IntoIterator<Item = &'a str>,
{
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["--workspace", workspace_arg, "--json"])
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee for {context}: {error}"))?;
    parse_json_output(&output, context)
}

fn parse_json_output(output: &Output, context: &str) -> Result<serde_json::Value, String> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        format!(
            "{context} should succeed; stdout: {}; stderr: {stderr}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!("{context} stderr must stay clean: {stderr}"),
    )?;
    parse_stdout_json(output, context)
}

fn parse_stdout_json(output: &Output, context: &str) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{context} stdout should be JSON: {error}"))
}

fn ensure_error_output_contains(output: &Output, context: &str, expected: &str) -> TestResult {
    ensure(
        !output.status.success(),
        format!(
            "{context} should fail; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "{context} stderr must stay clean: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let json = parse_stdout_json(output, context)?;
    ensure_equal(
        json_field(&json, &["schema"], &format!("{context} schema"))?,
        &serde_json::json!("ee.error.v2"),
        &format!("{context} schema"),
    )?;
    ensure_equal(
        json_field(&json, &["error", "code"], &format!("{context} error code"))?,
        &serde_json::json!("import"),
        &format!("{context} error code"),
    )?;
    ensure(
        json_field(
            &json,
            &["error", "message"],
            &format!("{context} error message"),
        )?
        .as_str()
        .is_some_and(|message| message.contains(expected)),
        format!("{context} error message should contain {expected:?}: {json}"),
    )
}

fn ensure_success(value: &serde_json::Value, context: &str) -> TestResult {
    ensure_equal(
        json_field(value, &["schema"], &format!("{context} response schema"))?,
        &serde_json::json!("ee.response.v2"),
        &format!("{context} response schema"),
    )?;
    ensure_equal(
        json_field(value, &["success"], &format!("{context} success flag"))?,
        &serde_json::json!(true),
        &format!("{context} success flag"),
    )
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
        .join("ee-cass-import-redaction")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

fn set_executable_dir_permissions(path: &Path) -> TestResult {
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
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
    printf '{"sessions":[{"path":"%s","workspace":"%s","agent":"codex","started_at":"2026-05-06T00:00:00Z","message_count":1,"token_count":96,"content_hash":"hash-cass-redaction-roundtrip"}]}\n' "$EE_FAKE_CASS_SESSION" "$EE_FAKE_CASS_WORKSPACE"
    ;;
  view)
    cat "$EE_FAKE_CASS_VIEW_JSON_PATH"
    if [ -n "${EE_FAKE_CASS_AFTER_VIEW_SLEEP_SECS:-}" ]; then
      sleep "$EE_FAKE_CASS_AFTER_VIEW_SLEEP_SECS"
    fi
    if [ -n "${EE_FAKE_CASS_AFTER_VIEW_MARKER:-}" ]; then
      printf 'completed\n' > "$EE_FAKE_CASS_AFTER_VIEW_MARKER"
    fi
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

fn write_fake_view_jsonl(path: &Path, content: &str) -> TestResult {
    fs::write(
        path,
        format!(
            "{}\n",
            serde_json::json!({
                "line": 1,
                "content": content,
            })
        ),
    )
    .map_err(|error| error.to_string())
}

fn write_oversized_stdout_line(path: &Path) -> TestResult {
    let mut line = "x".repeat(1_048_577);
    line.push('\n');
    fs::write(path, line).map_err(|error| error.to_string())
}

struct SupervisionEventInput<'a> {
    step: &'a str,
    fake_cass_behavior: &'a str,
    workspace_arg: &'a str,
    view_json_arg: &'a str,
    elapsed_ms: u128,
    output: &'a Output,
    response: &'a serde_json::Value,
}

fn append_supervision_event(path: &Path, input: SupervisionEventInput<'_>) -> TestResult {
    let event = serde_json::json!({
        "schema": "ee.test_event.v1",
        "surface": "cass_import_subprocess_supervision",
        "step": input.step,
        "command": "ee --workspace <workspace> --json import cass --database <database> --limit 1",
        "workspacePath": input.workspace_arg,
        "fakeCassBehavior": input.fake_cass_behavior,
        "fakeCassViewJsonPath": input.view_json_arg,
        "exitCode": input.output.status.code(),
        "success": input.output.status.success(),
        "elapsedMs": input.elapsed_ms,
        "response": {
            "schema": input.response.get("schema").and_then(serde_json::Value::as_str),
            "success": input.response.get("success").and_then(serde_json::Value::as_bool),
            "errorCode": input.response
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(serde_json::Value::as_str),
        },
    });
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    use std::io::Write as _;
    writeln!(file, "{event}").map_err(|error| error.to_string())
}

fn assert_supervision_log(path: &Path) -> TestResult {
    let log = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let events = log
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .map_err(|error| format!("supervision log line must be JSON: {error}: {line}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ensure_equal(&events.len(), &3, "supervision event count")?;

    let expected = [
        ("streamed_jsonl_success", true, "ee.response.v2"),
        ("stdout_line_cap_error", false, "ee.error.v2"),
        ("view_timeout_cleanup", false, "ee.error.v2"),
    ];
    for (event, (step, success, schema)) in events.iter().zip(expected) {
        ensure_equal(
            json_field(event, &["schema"], "supervision event schema")?,
            &serde_json::json!("ee.test_event.v1"),
            "supervision event schema",
        )?;
        ensure_equal(
            json_field(event, &["step"], "supervision event step")?,
            &serde_json::json!(step),
            "supervision event step",
        )?;
        ensure_equal(
            json_field(event, &["success"], "supervision event success")?,
            &serde_json::json!(success),
            "supervision event success",
        )?;
        ensure_equal(
            json_field(
                event,
                &["response", "schema"],
                "supervision response schema",
            )?,
            &serde_json::json!(schema),
            "supervision response schema",
        )?;
        ensure(
            json_field(event, &["elapsedMs"], "supervision elapsedMs")?
                .as_u64()
                .is_some(),
            format!("supervision elapsedMs must be an integer: {event}"),
        )?;
    }
    Ok(())
}

fn report_count(report: &serde_json::Value, field: &str) -> Result<u64, String> {
    json_field(
        report,
        &["data", field],
        &format!("report field data.{field}"),
    )?
    .as_u64()
    .ok_or_else(|| format!("report field data.{field} must be an unsigned integer"))
}

fn json_field<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
    context: &str,
) -> Result<&'a serde_json::Value, String> {
    let mut current = value;
    for key in path {
        current = current
            .get(*key)
            .ok_or_else(|| format!("{context} missing JSON field `{key}` in {value}"))?;
    }
    Ok(current)
}

fn ensure_output_omits_raw_values(
    context: &str,
    output: &str,
    raw_values: &[String],
) -> TestResult {
    for raw in raw_values {
        ensure(
            !output.contains(raw),
            format!("{context} leaked raw sensitive value {raw:?}"),
        )?;
    }
    Ok(())
}

fn ensure_database_files_omit_raw_values(database: &Path, raw_values: &[String]) -> TestResult {
    let paths = [
        database.to_path_buf(),
        database.with_file_name("ee.db-wal"),
        database.with_file_name("ee.db-shm"),
    ];

    for path in paths {
        if !path.exists() {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| error.to_string())?;
        let haystack = String::from_utf8_lossy(&bytes);
        ensure_output_omits_raw_values(&path.display().to_string(), &haystack, raw_values)?;
    }
    Ok(())
}

struct SensitiveCassFixture {
    session_line: String,
    raw_values: Vec<String>,
    secret_values: Vec<String>,
    secret_placeholders: Vec<String>,
}

impl SensitiveCassFixture {
    fn new() -> Self {
        let email = ["cass-redaction-roundtrip", "@", "example", ".", "test"].concat();
        let api_key = ["api", "-", "roundtrip", "-", "alpha"].concat();
        let password = ["password", "-", "roundtrip", "-", "beta"].concat();
        let secret_key = ["aws", "-", "secret", "-", "roundtrip", "-", "delta"].concat();
        let ssh_key = ["ssh", "-", "roundtrip", "-", "epsilon"].concat();
        let approval_bearer = [["ee", "ap", "1_"].concat(), "a".repeat(151)].concat();
        let jwt = [
            "eyJhbGciOiJIUzI1NiJ9",
            ".",
            "eyJzdWIiOiJjYXNzLXJlZGFjdGlvbiJ9",
            ".",
            "signaturesegmentvalue",
        ]
        .concat();
        let home_path = ["/home/", "cass-redaction-user", "/.ssh/id_rsa"].concat();

        let content = format!(
            "contact={email} api_key={api_key} password={password} jwt={jwt} secret_key={secret_key} ssh_key={ssh_key} approval={approval_bearer} opened_file={home_path}"
        );
        let session_line = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content,
            }
        })
        .to_string();

        let secret_values = vec![
            api_key.clone(),
            password.clone(),
            jwt.clone(),
            secret_key.clone(),
            ssh_key.clone(),
            approval_bearer.clone(),
        ];
        let raw_values = vec![email, home_path]
            .into_iter()
            .chain(secret_values.clone())
            .collect();
        let secret_placeholders = [
            "api_key",
            "password",
            "jwt_token",
            "secret_key",
            "ssh_key",
            "mesh_approval_token",
        ]
        .into_iter()
        .map(|class| format!("[REDACTED:{class}]"))
        .collect();

        Self {
            session_line,
            raw_values,
            secret_values,
            secret_placeholders,
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
