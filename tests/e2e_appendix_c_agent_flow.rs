//! Appendix C agent-flow parity E2E.
//!
//! This test keeps Appendix C executable as a real command trace. It uses
//! fixture Codex/CASS session data, explicit memories, search/context, curation
//! candidate apply, rule show, and outcome feedback, then records the exact
//! current CLI spelling where it differs from the plan text.

use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

#[derive(Clone, Debug)]
struct StepSpec {
    name: &'static str,
    phase: &'static str,
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
    phase: &'static str,
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
    data_schema: Option<String>,
    first_failure: Option<String>,
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
        .join("ee-appendix-c-e2e-logs")
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

fn data_schema_from_json(value: &JsonValue) -> Option<String> {
    value
        .pointer("/data/schema")
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
    let data_schema = parsed_stdout.as_ref().and_then(data_schema_from_json);
    let mut event = CommandEvent {
        schema: "ee.e2e.command_event.v1",
        scenario_id,
        phase: spec.phase,
        step: spec.name.to_owned(),
        command: "ee",
        args: spec.args,
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
        data_schema,
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
            "{} failed Appendix C contract: {}; log={}",
            event.step,
            failure,
            log_path.display()
        ));
    }
    Ok((event, parsed))
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

fn context_memory_ids(value: &JsonValue) -> Result<Vec<String>, String> {
    json_array(value, "/data/pack/items", "context")?
        .iter()
        .map(|item| {
            item.get("memoryId")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "context item missing memoryId".to_owned())
        })
        .collect()
}

fn ensure_context_items_have_provenance(value: &JsonValue) -> TestResult {
    for item in json_array(value, "/data/pack/items", "context provenance")? {
        let memory_id = item
            .get("memoryId")
            .and_then(JsonValue::as_str)
            .unwrap_or("<missing>");
        let provenance = item
            .get("provenance")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| format!("context item {memory_id} missing provenance array"))?;
        ensure(
            !provenance.is_empty(),
            format!("context item {memory_id} must include provenance"),
        )?;
        ensure(
            item.get("why").and_then(JsonValue::as_str).is_some(),
            format!("context item {memory_id} must explain why it was selected"),
        )?;
    }
    Ok(())
}

fn ensure_no_ansi(text: &str, context: &str) -> TestResult {
    ensure(
        !text.contains("\u{1b}["),
        format!("{context} must not contain ANSI escape sequences"),
    )
}

#[cfg(unix)]
fn real_cass_binary_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("EE_CASS_BINARY") {
        let path = PathBuf::from(path);
        ensure(
            path.is_absolute(),
            format!("EE_CASS_BINARY must be absolute for Appendix C e2e: {path:?}"),
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

#[cfg(unix)]
fn write_appendix_c_codex_session(codex_home: &Path, workspace: &Path) -> Result<PathBuf, String> {
    let sessions_dir = codex_home
        .join("sessions")
        .join("2026")
        .join("05")
        .join("07");
    fs::create_dir_all(&sessions_dir).map_err(|error| error.to_string())?;
    let session_path = sessions_dir.join("rollout-appendix-c-agent-flow.jsonl");
    let workspace_path = workspace.display().to_string();
    let records = [
        json!({
            "timestamp": "2026-05-07T06:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "appendix-c-agent-flow-fixture",
                "cwd": workspace_path,
                "cli_version": "0.42.0"
            }
        }),
        json!({
            "timestamp": "2026-05-07T06:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": "Appendix C task: add concurrent rate limiting to the API gateway without breaking governor Send + Sync behavior."
                    }
                ]
            }
        }),
        json!({
            "timestamp": "2026-05-07T06:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "Use governor middleware with bounded concurrency and keep Send + Sync error evidence attached to the rate-limit workflow."
                    }
                ]
            }
        }),
        json!({
            "timestamp": "2026-05-07T06:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "Appendix C outcome note: avoid an in-memory HashMap keyed by IP for distributed API gateway rate limiting."
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

fn candidate_id_from_playbook(value: &JsonValue) -> Result<String, String> {
    json_array(value, "/data/candidates", "playbook extract")?
        .iter()
        .find_map(|candidate| {
            let candidate_type = candidate.get("candidateType").and_then(JsonValue::as_str)?;
            let persisted = candidate.get("persisted").and_then(JsonValue::as_bool)?;
            (candidate_type == "rule" && persisted)
                .then(|| candidate.get("candidateId").and_then(JsonValue::as_str))
                .flatten()
                .map(str::to_owned)
        })
        .ok_or_else(|| "playbook extract did not persist a rule candidate".to_owned())
}

fn created_rule_id_from_apply(value: &JsonValue) -> Result<String, String> {
    json_array(value, "/data/application/changes", "curate apply")?
        .iter()
        .find_map(|change| {
            let field = change.get("field").and_then(JsonValue::as_str)?;
            (field == "ruleId")
                .then(|| change.get("after").and_then(JsonValue::as_str))
                .flatten()
                .map(str::to_owned)
        })
        .ok_or_else(|| "curate apply did not report created ruleId".to_owned())
}

#[cfg(unix)]
#[test]
fn appendix_c_agent_flow_parity_scenario() -> TestResult {
    let scenario_id = "appendix_c_agent_flow_parity";
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
    let session_path = write_appendix_c_codex_session(&codex_home, &workspace)?;
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
        "index Appendix C fixture session",
    )?;
    ensure_equal(
        &cass_index.pointer("/success"),
        &Some(&json!(true)),
        "cass Appendix C index success",
    )?;
    ensure_equal(
        &cass_index.pointer("/conversations"),
        &Some(&json!(1)),
        "cass Appendix C conversation count",
    )?;

    let cass_sessions = run_cass_json(
        &[
            OsString::from("sessions"),
            OsString::from("--workspace"),
            OsString::from(workspace_arg.clone()),
            OsString::from("--json"),
            OsString::from("--limit"),
            OsString::from("5"),
        ],
        &workspace,
        &envs,
        "discover Appendix C fixture session",
    )?;
    ensure_equal(
        &cass_sessions.pointer("/sessions/0/path"),
        &Some(&json!(session_arg)),
        "cass Appendix C session path",
    )?;

    let mut command_count = 0_usize;
    let mut semantic_memory_ids = Vec::new();

    let (_init_event, _init_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "01_setup_init",
            phase: "setup",
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
    command_count += 1;
    ensure(database_path.is_file(), "init must create the database")?;

    let (_import_event, import_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_setup_import_cass",
            phase: "setup",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "import".to_owned(),
                "cass".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
                "--since".to_owned(),
                "90d".to_owned(),
                "--limit".to_owned(),
                "5".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
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
    let spans_imported = import_json
        .pointer("/data/spansImported")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| "CASS import missing spansImported".to_owned())?;
    ensure(
        spans_imported >= 4,
        format!("Appendix C import must capture fixture spans, got {spans_imported}"),
    )?;

    let (_fact_event, fact_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_during_work_remember_fact",
            phase: "during_work",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "remember".to_owned(),
                "--level".to_owned(),
                "episodic".to_owned(),
                "--kind".to_owned(),
                "fact".to_owned(),
                "--tags".to_owned(),
                "governor,performance,appendix-c".to_owned(),
                "--workflow".to_owned(),
                "rate-limit-feb-2026".to_owned(),
                "--source".to_owned(),
                "cass-session://appendix-c-agent-flow-fixture#L1-2".to_owned(),
                "Appendix C fact: governor middleware must preserve Send + Sync behavior while adding concurrent API gateway rate limiting.".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    let fact_memory_id = json_string(&fact_json, "/data/memory_id", "remember fact")?.to_owned();

    let (_failure_event, failure_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_during_work_remember_failure",
            phase: "during_work",
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
                "governor,rate-limit,appendix-c".to_owned(),
                "--workflow".to_owned(),
                "rate-limit-feb-2026".to_owned(),
                "--source".to_owned(),
                "cass-session://appendix-c-agent-flow-fixture#L3-4".to_owned(),
                "Appendix C failure evidence: in-memory HashMap keyed by IP is the wrong API gateway rate-limit design when workers restart.".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    let failure_memory_id =
        json_string(&failure_json, "/data/memory_id", "remember failure")?.to_owned();

    for index in 0..5 {
        let (_semantic_event, semantic_json) = run_step_with_env(
            scenario_id,
            &events_path,
            &artifact_dir,
            &workspace,
            StepSpec {
                name: match index {
                    0 => "05_end_work_semantic_lesson_1",
                    1 => "06_end_work_semantic_lesson_2",
                    2 => "07_end_work_semantic_lesson_3",
                    3 => "08_end_work_semantic_lesson_4",
                    _ => "09_end_work_semantic_lesson_5",
                },
                phase: "end_of_work",
                args: vec![
                    "--workspace".to_owned(),
                    workspace_arg.clone(),
                    "--json".to_owned(),
                    "remember".to_owned(),
                    "--level".to_owned(),
                    "semantic".to_owned(),
                    "--kind".to_owned(),
                    "lesson".to_owned(),
                    "--tags".to_owned(),
                    "appendix-c,governor,search".to_owned(),
                    "--workflow".to_owned(),
                    "rate-limit-feb-2026".to_owned(),
                    "--source".to_owned(),
                    format!(
                        "cass-session://appendix-c-agent-flow-fixture#L{}-{}",
                        index + 5,
                        index + 6
                    ),
                    format!(
                        "Before modifying src/middleware/rate_limit.rs:{}, run `ee search \"Send + Sync error governor\" --json` from workspace root to retrieve governor failures from rate-limit-feb-2026 workflow.",
                        42 + index * 10
                    ),
                ],
                expected_exit_code: 0,
                expected_schema: "ee.response.v1",
                expect_clean_stderr: true,
            },
            &envs,
        )?;
        command_count += 1;
        semantic_memory_ids
            .push(json_string(&semantic_json, "/data/memory_id", "semantic lesson")?.to_owned());
    }

    let (_index_event, index_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "10_task_start_index_rebuild",
            phase: "task_start",
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
    command_count += 1;
    ensure_equal(
        &index_json.pointer("/data/sessions_indexed"),
        &Some(&json!(1)),
        "index rebuild must include imported session",
    )?;

    let (_context_event, context_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "11_task_start_context",
            phase: "task_start",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "context".to_owned(),
                "add concurrent rate limiting to the API gateway".to_owned(),
                "--max-tokens".to_owned(),
                "4000".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    let pack_hash = json_string(&context_json, "/data/pack/hash", "context")?.to_owned();
    let context_item_ids = context_memory_ids(&context_json)?;
    ensure(
        pack_hash.starts_with("blake3:"),
        "context pack hash must be stable",
    )?;
    ensure(
        context_item_ids
            .iter()
            .any(|memory_id| memory_id == &fact_memory_id || memory_id == &failure_memory_id),
        format!("context should include Appendix C task evidence, got {context_item_ids:?}"),
    )?;
    ensure_context_items_have_provenance(&context_json)?;

    let (_search_event, search_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "12_during_work_search",
            phase: "during_work",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "search".to_owned(),
                "Send + Sync error governor".to_owned(),
                "--limit".to_owned(),
                "5".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    let search_results = json_array(&search_json, "/data/results", "search")?;
    ensure(
        !search_results.is_empty(),
        "search must return Appendix C results",
    )?;
    ensure(
        search_results.iter().any(|result| {
            result
                .get("docId")
                .and_then(JsonValue::as_str)
                .is_some_and(|doc_id| {
                    doc_id == fact_memory_id
                        || doc_id == failure_memory_id
                        || semantic_memory_ids
                            .iter()
                            .any(|memory_id| memory_id == doc_id)
                })
        }),
        "search must return remembered Appendix C evidence",
    )?;

    let (_why_event, why_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "13_during_work_why",
            phase: "during_work",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "why".to_owned(),
                fact_memory_id.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    ensure_equal(
        &why_json.pointer("/data/found"),
        &Some(&json!(true)),
        "why should find Appendix C fact memory",
    )?;
    ensure(
        why_json.pointer("/data/storage/origin").is_some(),
        "why must explain storage origin",
    )?;
    ensure(
        why_json.pointer("/data/selection/latestPackSelection").is_some(),
        "why must include latest pack selection state",
    )?;

    let (_playbook_event, playbook_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "14_end_work_playbook_extract",
            phase: "end_of_work",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "playbook".to_owned(),
                "extract".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
                "--limit".to_owned(),
                "50".to_owned(),
                "--actor".to_owned(),
                "AppendixCParity".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    ensure_equal(
        &playbook_json.pointer("/data/schema"),
        &Some(&json!("ee.playbook.extract.v1")),
        "playbook extract data schema",
    )?;
    ensure_equal(
        &playbook_json.pointer("/data/persistedCount"),
        &Some(&json!(1)),
        "playbook extract must persist one rule candidate",
    )?;
    let candidate_id = candidate_id_from_playbook(&playbook_json)?;

    let (_candidates_event, candidates_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "15_end_work_curate_candidates",
            phase: "end_of_work",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "curate".to_owned(),
                "candidates".to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
                "--type".to_owned(),
                "rule".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    ensure_equal(
        &candidates_json.pointer("/data/schema"),
        &Some(&json!("ee.curate.candidates.v1")),
        "curate candidates data schema",
    )?;
    ensure(
        json_array(&candidates_json, "/data/candidates", "curate candidates")?
            .iter()
            .any(|candidate| {
                candidate.get("id").and_then(JsonValue::as_str) == Some(candidate_id.as_str())
                    && candidate.get("type").and_then(JsonValue::as_str) == Some("rule")
            }),
        "curate candidates must list the playbook rule candidate",
    )?;

    let (_validate_event, validate_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "16_end_work_curate_validate",
            phase: "end_of_work",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "curate".to_owned(),
                "validate".to_owned(),
                candidate_id.clone(),
                "--database".to_owned(),
                database_arg.clone(),
                "--actor".to_owned(),
                "AppendixCParity".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    ensure_equal(
        &validate_json.pointer("/data/schema"),
        &Some(&json!("ee.curate.validate.v1")),
        "curate validate data schema",
    )?;

    let validation_passed =
        validate_json.pointer("/data/validation/status") == Some(&json!("passed"));
    let workspace_id =
        json_string(&validate_json, "/data/workspaceId", "curate validate")?.to_owned();

    let (outcome_target_id, outcome_target_type) = if validation_passed {
        ensure_equal(
            &validate_json.pointer("/data/mutation/toStatus"),
            &Some(&json!("approved")),
            "curate validate transition",
        )?;

        let (_apply_event, apply_json) = run_step_with_env(
            scenario_id,
            &events_path,
            &artifact_dir,
            &workspace,
            StepSpec {
                name: "17_end_work_curate_apply",
                phase: "end_of_work",
                args: vec![
                    "--workspace".to_owned(),
                    workspace_arg.clone(),
                    "--json".to_owned(),
                    "curate".to_owned(),
                    "apply".to_owned(),
                    candidate_id.clone(),
                    "--database".to_owned(),
                    database_arg.clone(),
                    "--actor".to_owned(),
                    "AppendixCParity".to_owned(),
                ],
                expected_exit_code: 0,
                expected_schema: "ee.response.v1",
                expect_clean_stderr: true,
            },
            &envs,
        )?;
        command_count += 1;
        ensure_equal(
            &apply_json.pointer("/data/schema"),
            &Some(&json!("ee.curate.apply.v1")),
            "curate apply data schema",
        )?;
        ensure_equal(
            &apply_json.pointer("/data/application/decision"),
            &Some(&json!("create_rule")),
            "curate apply should create a rule",
        )?;
        ensure_equal(
            &apply_json.pointer("/data/durableMutation"),
            &Some(&json!(true)),
            "curate apply must persist",
        )?;
        let created_rule_id = created_rule_id_from_apply(&apply_json)?;

        let (_rule_show_event, rule_show_json) = run_step_with_env(
            scenario_id,
            &events_path,
            &artifact_dir,
            &workspace,
            StepSpec {
                name: "18_end_work_rule_show",
                phase: "end_of_work",
                args: vec![
                    "--workspace".to_owned(),
                    workspace_arg.clone(),
                    "--json".to_owned(),
                    "rule".to_owned(),
                    "show".to_owned(),
                    created_rule_id.clone(),
                    "--database".to_owned(),
                    database_arg.clone(),
                ],
                expected_exit_code: 0,
                expected_schema: "ee.response.v1",
                expect_clean_stderr: true,
            },
            &envs,
        )?;
        command_count += 1;
        ensure_equal(
            &rule_show_json.pointer("/data/found"),
            &Some(&json!(true)),
            "rule show should find the created rule",
        )?;

        (created_rule_id, "rule")
    } else {
        ensure_equal(
            &validate_json.pointer("/data/mutation/toStatus"),
            &Some(&json!("rejected")),
            "curate validate transition (rejected)",
        )?;
        (candidate_id.clone(), "candidate")
    };

    let (_outcome_event, outcome_json) = run_step_with_env(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "19_end_work_outcome_feedback",
            phase: "end_of_work",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "outcome".to_owned(),
                outcome_target_id.clone(),
                "--target-type".to_owned(),
                outcome_target_type.to_owned(),
                "--workspace-id".to_owned(),
                workspace_id.clone(),
                "--signal".to_owned(),
                if validation_passed {
                    "helpful"
                } else {
                    "neutral"
                }
                .to_owned(),
                "--source-id".to_owned(),
                "appendix-c-rate-limit-e2e".to_owned(),
                "--reason".to_owned(),
                if validation_passed {
                    "Task succeeded after applying the Appendix C governor search rule."
                } else {
                    "Candidate rejected due to specificity; flow completed without rule creation."
                }
                .to_owned(),
                "--database".to_owned(),
                database_arg.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v1",
            expect_clean_stderr: true,
        },
        &envs,
    )?;
    command_count += 1;
    ensure_equal(
        &outcome_json.pointer("/data/status"),
        &Some(&json!("recorded")),
        "outcome feedback status",
    )?;
    ensure_equal(
        &outcome_json.pointer("/data/target/type"),
        &Some(&json!(outcome_target_type)),
        "outcome target type",
    )?;

    append_jsonl(
        &events_path,
        &json!({
            "schema": "ee.appendix_c.agent_flow_parity.v1",
            "scenarioId": scenario_id,
            "event": "summary",
            "commandCount": command_count,
            "workspace": workspace.display().to_string(),
            "workspaceId": workspace_id,
            "databasePath": database_path.display().to_string(),
            "cassSessionPath": session_path.display().to_string(),
            "phases": [
                {"name": "setup", "commands": ["ee init", "ee import cass"]},
                {"name": "task_start", "commands": ["ee index rebuild", "ee context --max-tokens 4000 --json"]},
                {"name": "during_work", "commands": ["ee remember --tags", "ee search --limit", "ee why"]},
                {"name": "end_of_work", "commands": ["ee playbook extract", "ee curate candidates --type rule", "ee curate validate", "ee curate apply", "ee rule show", "ee outcome --target-type rule --signal helpful"]}
            ],
            "commandParity": [
                {"appendixC": "ee import cass --since 90d --auto-memorize", "current": "ee import cass --since 90d plus explicit ee remember commands", "reason": "current import stores sessions and spans; task memories are explicit durable records"},
                {"appendixC": "ee remember --tag governor,performance", "current": "ee remember --tags governor,performance", "reason": "current parser uses the plural tags flag"},
                {"appendixC": "ee search ... --top-k 5", "current": "ee search ... --limit 5", "reason": "current parser names the result cap limit"},
                {"appendixC": "ee curate candidates --kind rule", "current": "ee playbook extract; ee curate candidates --type rule", "reason": "current extraction command creates persisted rule candidates before curation review"},
                {"appendixC": "ee outcome --rule <id> --helpful", "current": "ee outcome <id> --target-type rule --signal helpful", "reason": "current feedback command uses explicit target and signal fields"}
            ],
            "assertions": {
                "sessionsImported": 1,
                "spansImported": spans_imported,
                "explicitMemoryIds": [fact_memory_id, failure_memory_id],
                "semanticMemoryIds": semantic_memory_ids,
                "contextPackHash": pack_hash,
                "contextItemIds": context_item_ids,
                "curationCandidateId": candidate_id,
                "validationPassed": validation_passed,
                "outcomeTargetId": outcome_target_id,
                "outcomeTargetType": outcome_target_type,
                "outcomeStatus": "recorded"
            }
        }),
    )?;

    let events_text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "failed to read Appendix C JSONL log {}: {error}",
            events_path.display()
        )
    })?;
    let lines = events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &lines.len(),
        &(command_count + 1),
        "Appendix C JSONL event count includes commands plus summary",
    )?;
    for (index, line) in lines.iter().enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("Appendix C event {index} must parse: {error}"))?;
        ensure(
            event.get("schema").is_some(),
            format!("Appendix C event {index} must include schema"),
        )?;
        if index < command_count {
            ensure(
                event.get("phase").is_some()
                    && event.get("stdout").is_some()
                    && event.get("stderr").is_some(),
                format!("Appendix C command event {index} must include phase/stdout/stderr"),
            )?;
            ensure_no_ansi(
                event
                    .get("stdout")
                    .and_then(JsonValue::as_str)
                    .unwrap_or(""),
                "Appendix C command stdout",
            )?;
        }
    }

    Ok(())
}
