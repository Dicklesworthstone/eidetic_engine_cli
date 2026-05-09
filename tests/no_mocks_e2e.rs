//! No-mocks end-to-end coverage for the walking-skeleton memory loop.
//!
//! This test runs the real `ee` binary against an isolated workspace, writes
//! every command result to structured JSONL, and asserts that the durable
//! FrankenSQLite database plus Frankensearch-derived index can support
//! init -> remember -> search -> context -> why without mocks.

use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use ee::db::{DatabaseConfig, DbConnection};
use ee::policy::redact_secret_like_content;

type TestResult = Result<(), String>;

#[derive(Clone, Debug)]
struct StepSpec {
    name: &'static str,
    args: Vec<String>,
    expected_exit_code: i32,
    expected_schema: &'static str,
    expect_clean_stderr: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandEvent {
    schema: &'static str,
    scenario_id: &'static str,
    step: String,
    command: &'static str,
    args: Vec<String>,
    cwd: String,
    workspace: String,
    started_at_unix_ms: u128,
    elapsed_ms: u128,
    exit_code: i32,
    stdout: String,
    stderr: String,
    stdout_artifact_path: String,
    stderr_artifact_path: String,
    stdout_json_valid: bool,
    stdout_schema: Option<String>,
    first_failure: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryEvent {
    schema: &'static str,
    scenario_id: &'static str,
    event: &'static str,
    command_count: usize,
    workspace: String,
    database_path: String,
    index_metadata_path: String,
    rule_memory_id: String,
    failure_memory_id: String,
    pack_hash: String,
    context_item_ids: Vec<String>,
}

#[derive(Clone, Copy)]
struct SecretProbe {
    class: &'static str,
    raw: &'static str,
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
    T: std::fmt::Debug + PartialEq + ?Sized,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn unique_log_dir(scenario_id: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-no-mocks-e2e-logs")
        .join(format!("{scenario_id}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create log dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn unix_ms_now() -> Result<u128, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_millis())
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create parent {}: {error}", parent.display()))?;
    }
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn append_jsonl<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: Serialize,
{
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open JSONL log {}: {error}", path.display()))?;
    serde_json::to_writer(&mut file, value)
        .map_err(|error| format!("failed to serialize JSONL event: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("failed to write JSONL newline: {error}"))
}

fn sanitize_step_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn schema_from_json(value: &JsonValue) -> Option<String> {
    value
        .get("schema")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn first_failure(
    expected_exit_code: i32,
    expected_schema: &str,
    expect_clean_stderr: bool,
    event: &CommandEvent,
) -> Option<String> {
    if event.exit_code != expected_exit_code {
        return Some(format!(
            "exit_code: expected {}, got {}",
            expected_exit_code, event.exit_code
        ));
    }
    if !event.stdout_json_valid {
        return Some("stdout is not valid JSON".to_owned());
    }
    if event.stdout_schema.as_deref() != Some(expected_schema) {
        return Some(format!(
            "schema: expected {}, got {:?}",
            expected_schema, event.stdout_schema
        ));
    }
    if expect_clean_stderr && !event.stderr.is_empty() {
        return Some("stderr was not empty".to_owned());
    }
    None
}

fn stream_snippet(text: &str) -> String {
    let trimmed = text.trim();
    let mut snippet = trimmed.chars().take(1200).collect::<String>();
    if trimmed.chars().nth(1200).is_some() {
        snippet.push_str("...");
    }
    snippet
}

fn run_step(
    scenario_id: &'static str,
    log_path: &Path,
    artifact_dir: &Path,
    workspace: &Path,
    spec: StepSpec,
) -> Result<(CommandEvent, JsonValue), String> {
    run_step_with_env(scenario_id, log_path, artifact_dir, workspace, spec, &[])
}

fn run_step_with_env(
    scenario_id: &'static str,
    log_path: &Path,
    artifact_dir: &Path,
    workspace: &Path,
    spec: StepSpec,
    envs: &[(&str, OsString)],
) -> Result<(CommandEvent, JsonValue), String> {
    let started_at_unix_ms = unix_ms_now()?;
    let start = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(&spec.args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to execute step {}: {error}", spec.name))?;
    let elapsed_ms = start.elapsed().as_millis();

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout UTF-8 decode failed for {}: {error}", spec.name))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr UTF-8 decode failed for {}: {error}", spec.name))?;
    let step_slug = sanitize_step_name(spec.name);
    let stdout_path = artifact_dir.join(format!("{step_slug}.stdout.json"));
    let stderr_path = artifact_dir.join(format!("{step_slug}.stderr.log"));
    write_text(&stdout_path, &stdout)?;
    write_text(&stderr_path, &stderr)?;

    let parsed_stdout = serde_json::from_str::<JsonValue>(&stdout).ok();
    let stdout_schema = parsed_stdout.as_ref().and_then(schema_from_json);
    let logged_args = spec
        .args
        .iter()
        .map(|arg| redact_secret_like_content(arg).content)
        .collect::<Vec<_>>();
    let mut event = CommandEvent {
        schema: "ee.e2e.command_event.v1",
        scenario_id,
        step: spec.name.to_owned(),
        command: "ee",
        args: logged_args,
        cwd: env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_owned()),
        workspace: workspace.display().to_string(),
        started_at_unix_ms,
        elapsed_ms,
        exit_code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
        stdout_artifact_path: stdout_path.display().to_string(),
        stderr_artifact_path: stderr_path.display().to_string(),
        stdout_json_valid: parsed_stdout.is_some(),
        stdout_schema,
        first_failure: None,
    };
    event.first_failure = first_failure(
        spec.expected_exit_code,
        spec.expected_schema,
        spec.expect_clean_stderr,
        &event,
    );
    append_jsonl(log_path, &event)?;

    let parsed = parsed_stdout.ok_or_else(|| {
        format!(
            "{} stdout was not valid JSON; see {}",
            event.step, event.stdout_artifact_path
        )
    })?;
    if let Some(failure) = &event.first_failure {
        return Err(format!(
            "{} failed no-mocks contract: {}; stdout={}; stderr={}; log={}",
            event.step,
            failure,
            stream_snippet(&event.stdout),
            stream_snippet(&event.stderr),
            log_path.display()
        ));
    }
    Ok((event, parsed))
}

fn string_at(value: &JsonValue, pointer: &str, context: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{context} missing string at {pointer}"))
}

fn memory_ids_from_context(value: &JsonValue) -> Result<Vec<String>, String> {
    value
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "context pack items missing".to_owned())?
        .iter()
        .map(|item| {
            item.get("memoryId")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "context pack item missing memoryId".to_owned())
        })
        .collect()
}

fn json_string<'a>(value: &'a JsonValue, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{context} missing string at {pointer}"))
}

fn json_array<'a>(
    value: &'a JsonValue,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<JsonValue>, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{context} missing array at {pointer}"))
}

fn cass_sessions_array<'a>(
    value: &'a JsonValue,
    context: &str,
) -> Result<&'a Vec<JsonValue>, String> {
    value
        .get("sessions")
        .and_then(JsonValue::as_array)
        .or_else(|| value.pointer("/data/sessions").and_then(JsonValue::as_array))
        .or_else(|| value.get("hits").and_then(JsonValue::as_array))
        .or_else(|| value.as_array())
        .ok_or_else(|| {
            let payload =
                serde_json::to_string(value).unwrap_or_else(|error| format!("<unprintable: {error}>"));
            format!(
                "{context} missing sessions array at /sessions, /data/sessions, or top level; payload={payload}"
            )
        })
}

fn cass_session_source_path(value: &JsonValue) -> Option<&str> {
    value
        .get("path")
        .or_else(|| value.get("source_path"))
        .and_then(JsonValue::as_str)
}

fn degradation_codes(value: &JsonValue) -> Result<Vec<String>, String> {
    let mut codes = json_array(value, "/data/degraded", "status degraded")?
        .iter()
        .filter_map(|item| item.get("code").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codes.sort();
    Ok(codes)
}

fn ensure_no_ansi(text: &str, context: &str) -> TestResult {
    ensure(
        !text.contains("\u{1b}["),
        format!("{context} must not contain ANSI escape sequences"),
    )
}

fn egress_secret_probes() -> Vec<SecretProbe> {
    vec![
        SecretProbe {
            class: "anthropic_api_key",
            raw: "sk-ant-api03-redactionegressredactionegressredactionegress",
        },
        SecretProbe {
            class: "aws_secret_access_key",
            raw: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        },
        SecretProbe {
            class: "jwt_token",
            raw: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.Rq8IjqberX03cRIZHg7v0Rq8IjqberX03cRIZHg7v0",
        },
        SecretProbe {
            class: "pem_private_key_header",
            raw: "-----BEGIN RSA PRIVATE KEY-----",
        },
        SecretProbe {
            class: "pem_private_key_body",
            raw: "MIIEowIBAAKCAQEAredactionegressredactionegressredaction",
        },
    ]
}

fn ensure_text_omits_secret_probes(
    surface: &str,
    stream: &str,
    artifact_path: &str,
    text: &str,
    probes: &[SecretProbe],
) -> TestResult {
    for probe in probes {
        ensure(
            !text.contains(probe.raw),
            format!(
                "{surface} {stream} leaked secret class {}; artifact={artifact_path}",
                probe.class
            ),
        )?;
    }
    Ok(())
}

fn ensure_step_omits_secret_probes(
    surface: &str,
    event: &CommandEvent,
    json_payload: &JsonValue,
    probes: &[SecretProbe],
) -> TestResult {
    ensure_text_omits_secret_probes(
        surface,
        "stdout",
        &event.stdout_artifact_path,
        &event.stdout,
        probes,
    )?;
    ensure_text_omits_secret_probes(
        surface,
        "stderr",
        &event.stderr_artifact_path,
        &event.stderr,
        probes,
    )?;
    ensure_text_omits_secret_probes(
        surface,
        "json",
        &event.stdout_artifact_path,
        &json_payload.to_string(),
        probes,
    )?;
    let logged_args = serde_json::to_string(&event.args)
        .map_err(|error| format!("failed to serialize logged argv: {error}"))?;
    ensure_text_omits_secret_probes(
        surface,
        "logged-argv",
        "commands.jsonl",
        &logged_args,
        probes,
    )
}

#[cfg(unix)]
fn real_cass_binary_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("EE_CASS_BINARY") {
        let path = PathBuf::from(path);
        ensure(
            path.is_absolute(),
            format!("EE_CASS_BINARY must be absolute for no-mocks CASS e2e: {path:?}"),
        )?;
        ensure(
            path.file_name().and_then(|name| name.to_str()) == Some("cass"),
            format!("EE_CASS_BINARY must point to a real cass executable: {path:?}"),
        )?;
        ensure(path.is_file(), format!("cass binary not found: {path:?}"))?;
        return path.canonicalize().map_err(|error| error.to_string());
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("cass");
            if candidate.is_file() {
                return candidate.canonicalize().map_err(|error| error.to_string());
            }
        }
    }

    Err("real cass binary not found on PATH".to_owned())
}

#[cfg(unix)]
fn path_with_binary_parent(binary: &Path) -> Result<OsString, String> {
    let parent = binary
        .parent()
        .ok_or_else(|| format!("cass binary has no parent directory: {binary:?}"))?;
    let mut entries = vec![parent.to_path_buf()];
    if let Some(existing) = env::var_os("PATH") {
        entries.extend(env::split_paths(&existing));
    }
    env::join_paths(entries).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn write_codex_cass_fixture_session(
    codex_home: &Path,
    workspace: &Path,
) -> Result<PathBuf, String> {
    let sessions_dir = codex_home
        .join("sessions")
        .join("2026")
        .join("05")
        .join("06");
    fs::create_dir_all(&sessions_dir).map_err(|error| error.to_string())?;
    let session_path = sessions_dir.join("rollout-x65f-cass-import.jsonl");
    let workspace_path = workspace.display().to_string();
    let records = [
        json!({
            "timestamp": "2026-05-06T03:40:00Z",
            "type": "session_meta",
            "payload": {
                "id": "x65f-cass-import-fixture",
                "cwd": workspace_path,
                "cli_version": "0.42.0"
            }
        }),
        json!({
            "timestamp": "2026-05-06T03:40:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": "x65f cass import fixture release evidence alpha"
                    }
                ]
            }
        }),
        json!({
            "timestamp": "2026-05-06T03:40:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "x65f imported CASS evidence remains durable and searchable"
                    }
                ]
            }
        }),
    ];

    let mut jsonl = String::new();
    for record in records {
        jsonl.push_str(&serde_json::to_string(&record).map_err(|error| error.to_string())?);
        jsonl.push('\n');
    }
    write_text(&session_path, &jsonl)?;
    Ok(session_path)
}

#[cfg(unix)]
fn run_cass_json(
    args: &[OsString],
    cwd: &Path,
    envs: &[(&str, OsString)],
    context: &str,
) -> Result<JsonValue, String> {
    let mut command = Command::new("cass");
    command.current_dir(cwd).args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run cass for {context}: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        format!(
            "cass {context} failed with exit {:?}: {stderr}",
            output.status.code()
        ),
    )?;
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cass {context} stdout must be JSON: {error}"))
}

#[test]
fn no_mocks_status_json_conformance_logs_capabilities_and_degradations() -> TestResult {
    let scenario_id = "lp4p6_status_json_conformance";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-status-conformance-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");

    let (pre_event, pre_status) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_status_before_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "status".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_no_ansi(&pre_event.stdout, "pre-init status stdout")?;
    ensure_equal(
        &pre_status.pointer("/data/command"),
        &Some(&JsonValue::String("status".to_owned())),
        "pre-init status command",
    )?;
    ensure(
        json_string(&pre_status, "/data/version", "pre-init status")? == env!("CARGO_PKG_VERSION"),
        "pre-init status must report package version",
    )?;
    ensure_equal(
        &json_string(&pre_status, "/data/capabilities/storage", "pre-init status")?,
        &"pending",
        "pre-init storage capability",
    )?;
    ensure_equal(
        &json_string(&pre_status, "/data/capabilities/search", "pre-init status")?,
        &"pending",
        "pre-init search capability",
    )?;
    let pre_codes = degradation_codes(&pre_status)?;
    ensure(
        pre_codes
            .iter()
            .any(|code| code == "storage_not_initialized"),
        format!("pre-init status must diagnose missing storage, got {pre_codes:?}"),
    )?;
    ensure(
        pre_codes
            .iter()
            .any(|code| code == "search_waiting_for_storage"),
        format!("pre-init status must diagnose search waiting for storage, got {pre_codes:?}"),
    )?;

    let (_init_event, _init_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure(
        database_path.is_file(),
        "init must create real database file",
    )?;

    let (post_event, post_status) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_status_after_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "status".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_no_ansi(&post_event.stdout, "post-init status stdout")?;
    ensure_equal(
        &json_string(
            &post_status,
            "/data/capabilities/runtime",
            "post-init status",
        )?,
        &"ready",
        "post-init runtime capability",
    )?;
    ensure_equal(
        &json_string(
            &post_status,
            "/data/capabilities/storage",
            "post-init status",
        )?,
        &"ready",
        "post-init storage capability",
    )?;
    ensure(
        json_string(&post_status, "/data/workspace/root", "post-init status")? == workspace_arg,
        "post-init status must report selected workspace root",
    )?;
    ensure(
        post_status.pointer("/data/memoryHealth").is_some(),
        "post-init status must include memory health object",
    )?;
    ensure(
        post_status.pointer("/data/curationHealth").is_some(),
        "post-init status must include curation health object",
    )?;
    ensure(
        post_status.pointer("/data/feedbackHealth").is_some(),
        "post-init status must include feedback health object",
    )?;
    ensure(
        !json_array(&post_status, "/data/derivedAssets", "post-init status")?.is_empty(),
        "post-init status must report derived assets",
    )?;

    append_jsonl(
        &events_path,
        &json!({
            "schema": "ee.e2e.summary_event.v1",
            "scenarioId": scenario_id,
            "event": "summary",
            "commandCount": 3,
            "workspace": workspace.display().to_string(),
            "databasePath": database_path.display().to_string(),
            "preInitDegradationCodes": pre_codes,
            "postInitStorageCapability": json_string(
                &post_status,
                "/data/capabilities/storage",
                "post-init status",
            )?,
            "stdoutArtifactPaths": [
                pre_event.stdout_artifact_path,
                post_event.stdout_artifact_path,
            ],
        }),
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read status JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    let event_lines = events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &event_lines.len(),
        &4_usize,
        "status conformance JSONL event count",
    )?;
    for (index, line) in event_lines.iter().enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("status JSONL event {index} must parse: {error}"))?;
        ensure(
            event.get("schema").is_some(),
            format!("status JSONL event {index} must include schema"),
        )?;
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn no_mocks_import_cass_fixture_sessions_stores_spans_and_searches() -> TestResult {
    let scenario_id = "x65f_cass_import_e2e";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace = log_dir.join("workspace");
    let home = log_dir.join("home");
    let codex_home = log_dir.join("codex-home");
    let cass_data_dir = log_dir.join("cass-data");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    fs::create_dir_all(&codex_home).map_err(|error| error.to_string())?;
    fs::create_dir_all(&cass_data_dir).map_err(|error| error.to_string())?;

    let cass_binary = real_cass_binary_path()?;
    let cass_path = path_with_binary_parent(&cass_binary)?;
    let session_path = write_codex_cass_fixture_session(&codex_home, &workspace)?;
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let database_arg = database_path.display().to_string();
    let cass_data_arg = cass_data_dir.display().to_string();
    let session_arg = session_path.display().to_string();
    let envs = vec![
        ("HOME", home.as_os_str().to_owned()),
        ("CODEX_HOME", codex_home.as_os_str().to_owned()),
        ("CASS_DATA_DIR", cass_data_dir.as_os_str().to_owned()),
        ("CASS_IGNORE_SOURCES_CONFIG", OsString::from("1")),
        ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", OsString::from("1")),
        ("CASS_INDEX_NO_PROGRESS_EVENTS", OsString::from("1")),
        ("NO_COLOR", OsString::from("1")),
        ("EE_CASS_BINARY", cass_binary.as_os_str().to_owned()),
        ("PATH", cass_path),
    ];

    let cass_index = run_cass_json(
        &[
            OsString::from("index"),
            OsString::from("--full"),
            OsString::from("--data-dir"),
            OsString::from(cass_data_arg.clone()),
            OsString::from("--json"),
        ],
        &workspace,
        &envs,
        "index fixture sessions",
    )?;
    ensure_equal(
        &cass_index.pointer("/success"),
        &Some(&json!(true)),
        "cass fixture index success",
    )?;
    ensure_equal(
        &cass_index.pointer("/conversations"),
        &Some(&json!(1)),
        "cass fixture conversation count",
    )?;

    let cass_sessions = run_cass_json(
        &[
            OsString::from("sessions"),
            OsString::from("--workspace"),
            OsString::from(workspace_arg.clone()),
            OsString::from("--json"),
            OsString::from("--data-dir"),
            OsString::from(cass_data_arg.clone()),
            OsString::from("--limit"),
            OsString::from("5"),
        ],
        &workspace,
        &envs,
        "sessions fixture discovery",
    )?;
    let cass_session = cass_sessions_array(&cass_sessions, "cass sessions")?
        .first()
        .ok_or_else(|| "cass fixture discovery returned no sessions".to_owned())?;
    ensure_equal(
        &cass_session_source_path(cass_session),
        &Some(session_arg.as_str()),
        "cass fixture session path",
    )?;
    ensure_equal(
        &cass_session.get("workspace"),
        &Some(&json!(workspace_arg)),
        "cass fixture workspace path",
    )?;

    let (_init_event, _init_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;

    let (_import_event, import_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_import_cass",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "import".to_owned(),
                "cass".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
                "--limit".to_owned(),
                "5".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    ensure_equal(
        &import_json.pointer("/data/status"),
        &Some(&json!("completed")),
        "CASS import status",
    )?;
    ensure_equal(
        &import_json.pointer("/data/sessionsImported"),
        &Some(&json!(1)),
        "CASS sessions imported",
    )?;
    ensure(
        import_json
            .pointer("/data/spansImported")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0)
            >= 3,
        "CASS import must capture evidence spans from the fixture session",
    )?;
    ensure_equal(
        &import_json.pointer("/data/sessions/0/sourcePath"),
        &Some(&json!(session_arg)),
        "CASS import report source path",
    )?;

    let connection = DbConnection::open(DatabaseConfig::file(database_path.clone()))
        .map_err(|error| error.to_string())?;
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?;
    ensure_equal(&workspaces.len(), &1_usize, "stored workspace count")?;
    ensure_equal(&workspaces[0].path, &workspace_arg, "stored workspace path")?;
    let sessions = connection
        .list_sessions(&workspaces[0].id)
        .map_err(|error| error.to_string())?;
    ensure_equal(&sessions.len(), &1_usize, "stored CASS session count")?;
    ensure_equal(
        &sessions[0].source_path,
        &Some(session_arg.clone()),
        "stored CASS session source path",
    )?;
    ensure_equal(
        &sessions[0].agent_name,
        &Some("codex".to_owned()),
        "stored CASS session agent",
    )?;
    let spans = connection
        .list_evidence_spans_for_session(&sessions[0].id)
        .map_err(|error| error.to_string())?;
    ensure(
        spans.len() >= 3,
        format!("expected at least 3 imported spans, got {}", spans.len()),
    )?;
    let span_text = spans
        .iter()
        .map(|span| span.excerpt.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ensure(
        span_text.contains("x65f cass import fixture release evidence alpha"),
        "stored evidence spans must include the fixture user text",
    )?;
    ensure(
        span_text.contains("x65f imported CASS evidence remains durable and searchable"),
        "stored evidence spans must include the fixture assistant text",
    )?;
    ensure(
        spans.iter().all(|span| {
            span.cass_span_id.starts_with(session_arg.as_str()) && !span.content_hash.is_empty()
        }),
        "evidence spans must preserve CASS source IDs and content hashes",
    )?;
    let stored_session_id = sessions[0].id.clone();
    connection.close().map_err(|error| error.to_string())?;

    let (_index_event, index_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    ensure_equal(
        &index_json.pointer("/data/sessions_indexed"),
        &Some(&json!(1)),
        "indexed CASS session count",
    )?;

    let (_search_event, search_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_search_imported_session",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "search".to_owned(),
                "CASS session codex".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    let search_results = json_array(&search_json, "/data/results", "CASS import search")?;
    ensure(
        search_results.iter().any(|result| {
            result
                .get("docId")
                .and_then(JsonValue::as_str)
                .is_some_and(|doc_id| doc_id.eq(stored_session_id.as_str()))
        }),
        "search must retrieve the imported CASS session document",
    )?;

    Ok(())
}

#[test]
fn no_mocks_public_outputs_redact_secret_like_memory_content() -> TestResult {
    let scenario_id = "redaction_egress_public_outputs";
    let probes = egress_secret_probes();
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-redaction-egress-workspace-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let marker = "rynf redaction egress anchor";
    let jwt = probes
        .iter()
        .find(|probe| probe.class == "jwt_token")
        .map(|probe| probe.raw)
        .ok_or_else(|| "jwt probe missing".to_owned())?;
    let raw_content = format!(
        "{marker}. Use API token {}; AWS_SECRET_ACCESS_KEY={}; Authorization: Bearer {jwt}; PEM block {} {} -----END RSA PRIVATE KEY-----.",
        probes[0].raw, probes[1].raw, probes[3].raw, probes[4].raw
    );
    let redaction_report = redact_secret_like_content(&raw_content);
    ensure(
        redaction_report.redacted,
        "redaction fixture must exercise real secret-like content",
    )?;
    ensure_text_omits_secret_probes(
        "redacted fixture",
        "content",
        "in-memory",
        &redaction_report.content,
        &probes,
    )?;
    let redacted_content = redaction_report
        .content
        .replace("Use API token ", "Scanner alpha placeholder ")
        .replace("AWS_SECRET_ACCESS_KEY=", "Scanner beta placeholder ")
        .replace("Authorization: Bearer ", "Scanner gamma placeholder ")
        .replace("PEM block ", "Scanner delta placeholder ")
        .replace("[REDACTED:anthropic_api_key]", "[REDACTED:alpha]")
        .replace("[REDACTED:aws_secret_access_key]", "[REDACTED:beta]")
        .replace("[REDACTED:bearer_token]", "[REDACTED:gamma]")
        .replace("[REDACTED:pem_block]", "[REDACTED:delta]");
    let storage_policy_check = redact_secret_like_content(&redacted_content);
    ensure(
        !storage_policy_check.redacted,
        format!(
            "redacted fixture must be accepted by remember policy, still matched {:?}",
            storage_policy_check.redacted_reasons
        ),
    )?;

    let (_init_event, _init_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;

    let (deny_event, deny_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_reject_raw_secret_like_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "redaction,egress".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L1050".to_owned(),
                raw_content,
            ],
            expected_exit_code: 7,
            expected_schema: "ee.error.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("remember policy denial", &deny_event, &deny_json, &probes)?;
    ensure_equal(
        &deny_json.pointer("/error/code"),
        &Some(&JsonValue::String("policy_denied".to_owned())),
        "raw secret-like remember denial code",
    )?;

    let (remember_event, remember_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_remember_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "redaction,egress".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L1050".to_owned(),
                redacted_content,
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("remember", &remember_event, &remember_json, &probes)?;
    let memory_id = string_at(
        &remember_json,
        "/data/memory_id",
        "remember redaction egress",
    )?;

    let (index_event, index_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("index rebuild", &index_event, &index_json, &probes)?;

    let (search_event, search_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "05_search_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "search".to_owned(),
                marker.to_owned(),
                "--limit".to_owned(),
                "5".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("search", &search_event, &search_json, &probes)?;
    let search_results = json_array(&search_json, "/data/results", "redaction egress search")?;
    ensure(
        search_results
            .iter()
            .any(|result| result.get("docId").and_then(JsonValue::as_str) == Some(&memory_id)),
        "search should retrieve the redacted memory by non-secret marker",
    )?;

    let (context_event, context_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06_context_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                marker.to_owned(),
                "--max-tokens".to_owned(),
                "2000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("context", &context_event, &context_json, &probes)?;
    let context_item_ids = memory_ids_from_context(&context_json)?;
    ensure(
        context_item_ids.iter().any(|id| id == &memory_id),
        "context should select the redacted memory by non-secret marker",
    )?;

    let (why_event, why_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07_why_redacted_memory",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "why".to_owned(),
                memory_id,
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes("why", &why_event, &why_json, &probes)?;
    ensure_equal(
        &why_json.pointer("/data/found"),
        &Some(&JsonValue::Bool(true)),
        "why should find the redacted memory",
    )?;

    let (support_event, support_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "08_support_bundle_dry_run",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg,
                "--json".to_owned(),
                "support".to_owned(),
                "bundle".to_owned(),
                "--dry-run".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    ensure_step_omits_secret_probes(
        "support bundle dry-run",
        &support_event,
        &support_json,
        &probes,
    )?;
    ensure_equal(
        &support_json.pointer("/data/schema"),
        &Some(&JsonValue::String("ee.support_bundle.v1".to_owned())),
        "support bundle dry-run schema",
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    ensure_text_omits_secret_probes(
        "command event log",
        "jsonl",
        &events_path.display().to_string(),
        &events_text,
        &probes,
    )
}

#[test]
fn no_mocks_init_remember_search_context_why_with_jsonl_command_events() -> TestResult {
    let scenario_id = "phase3_no_mocks_memory_loop";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-no-mocks-workspace-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let index_metadata_path = workspace.join(".ee").join("index").join("meta.json");

    let mut command_count = 0_usize;

    let (_init_event, _init_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_init",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "init".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure(
        database_path.is_file(),
        "init must create real FrankenSQLite database",
    )?;

    let (_rule_event, rule_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_remember_rule",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "procedural".to_owned(),
                "--kind".to_owned(),
                "rule".to_owned(),
                "--tags".to_owned(),
                "release,verification".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L1".to_owned(),
                "Run cargo fmt --check, cargo clippy --all-targets -- -D warnings, and cargo test before release.".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let rule_memory_id = string_at(&rule_json, "/data/memory_id", "remember rule")?;

    let (_failure_event, failure_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_remember_failure",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "episodic".to_owned(),
                "--kind".to_owned(),
                "failure".to_owned(),
                "--tags".to_owned(),
                "release,clippy,workflow".to_owned(),
                "--source".to_owned(),
                "file://tests/no_mocks_e2e.rs#L2".to_owned(),
                "Release validation failed because clippy warnings were skipped before tagging."
                    .to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let failure_memory_id = string_at(&failure_json, "/data/memory_id", "remember failure")?;

    let (_index_event, _index_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_index_rebuild",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "index".to_owned(),
                "rebuild".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure(
        index_metadata_path.is_file(),
        "index rebuild must publish real Frankensearch metadata",
    )?;

    let (_search_event, search_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "05_search",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "search".to_owned(),
                "release clippy".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let search_results = search_json
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "search results missing".to_owned())?;
    ensure(
        !search_results.is_empty(),
        "search must return real indexed results",
    )?;
    let search_ids = search_results
        .iter()
        .filter_map(|result| result.get("docId").and_then(JsonValue::as_str))
        .collect::<Vec<_>>();
    ensure(
        search_ids.contains(&rule_memory_id.as_str())
            && search_ids.contains(&failure_memory_id.as_str()),
        format!(
            "search must surface both remembered records; got {search_ids:?}, wanted {rule_memory_id} and {failure_memory_id}"
        ),
    )?;

    let (_context_first_event, context_first_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06_context_first",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                "prepare a release after clippy failure".to_owned(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let first_pack_hash = string_at(&context_first_json, "/data/pack/hash", "context first")?;
    let first_item_ids = memory_ids_from_context(&context_first_json)?;
    ensure(
        first_pack_hash.starts_with("blake3:"),
        format!("pack hash must use blake3 prefix, got {first_pack_hash}"),
    )?;
    ensure(
        !first_item_ids.is_empty(),
        "context pack must select at least one memory",
    )?;

    let (_context_second_event, context_second_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07_context_second",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                "prepare a release after clippy failure".to_owned(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let second_pack_hash = string_at(&context_second_json, "/data/pack/hash", "context second")?;
    let second_item_ids = memory_ids_from_context(&context_second_json)?;
    ensure_equal(
        &second_pack_hash,
        &first_pack_hash,
        "pack hash deterministic across repeated context calls",
    )?;
    ensure_equal(
        &second_item_ids,
        &first_item_ids,
        "context item order deterministic across repeated context calls",
    )?;

    let (_why_event, why_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "08_why",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "why".to_owned(),
                rule_memory_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &why_json.pointer("/data/found"),
        &Some(&JsonValue::Bool(true)),
        "why should find the remembered rule",
    )?;
    ensure_equal(
        &why_json.pointer("/data/memoryId"),
        &Some(&JsonValue::String(rule_memory_id.clone())),
        "why should echo requested memory id",
    )?;
    ensure(
        why_json.pointer("/data/storage/origin").is_some(),
        "why must explain storage origin",
    )?;
    ensure(
        why_json.pointer("/data/retrieval/confidence").is_some(),
        "why must explain retrieval confidence",
    )?;

    append_jsonl(
        &events_path,
        &SummaryEvent {
            schema: "ee.e2e.summary_event.v1",
            scenario_id,
            event: "summary",
            command_count,
            workspace: workspace.display().to_string(),
            database_path: database_path.display().to_string(),
            index_metadata_path: index_metadata_path.display().to_string(),
            rule_memory_id,
            failure_memory_id,
            pack_hash: first_pack_hash,
            context_item_ids: first_item_ids,
        },
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    let event_lines = events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &event_lines.len(),
        &(command_count + 1),
        "JSONL event count includes commands plus summary",
    )?;
    for (index, line) in event_lines.iter().take(command_count).enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("JSONL command event {index} must parse: {error}"))?;
        ensure(
            event.get("stdout").is_some()
                && event.get("stderr").is_some()
                && event.get("exitCode").is_some(),
            format!("JSONL command event {index} must capture stdout/stderr/exitCode"),
        )?;
    }

    Ok(())
}
