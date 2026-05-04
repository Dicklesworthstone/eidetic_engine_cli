//! No-mocks end-to-end coverage for the walking-skeleton memory loop.
//!
//! This test runs the real `ee` binary against an isolated workspace, writes
//! every command result to structured JSONL, and asserts that the durable
//! FrankenSQLite database plus Frankensearch-derived index can support
//! init -> remember -> search -> context -> why without mocks.

use serde::Serialize;
use serde_json::Value as JsonValue;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
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
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
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

fn run_step(
    scenario_id: &'static str,
    log_path: &Path,
    artifact_dir: &Path,
    workspace: &Path,
    spec: StepSpec,
) -> Result<(CommandEvent, JsonValue), String> {
    let started_at_unix_ms = unix_ms_now()?;
    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(&spec.args)
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
    let mut event = CommandEvent {
        schema: "ee.e2e.command_event.v1",
        scenario_id,
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
            "{} failed no-mocks contract: {}; log={}",
            event.step,
            failure,
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
