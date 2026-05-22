//! No-mocks end-to-end coverage for the handoff capsule CLI flow.
//!
//! The test runs the real `ee` binary against an isolated workspace and
//! exercises create/inspect/preview/resume/rotate-key through process
//! boundaries. It records command artifacts plus ee.test_event.v1 rows so the
//! evidence is inspectable without depending on mocks.

use serde::Serialize;
use serde_json::{Value as JsonValue, json};
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
    schema_validation_status: &'static str,
    redaction_status: &'static str,
    first_failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct TestEvent {
    schema: &'static str,
    ts: String,
    test_id: &'static str,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<f64>,
    fields: JsonValue,
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
        .join("ee-handoff-no-mocks-e2e-logs")
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

fn test_event_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn blake3_hash_field(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn schema_from_json(value: &JsonValue) -> Option<String> {
    value
        .get("schema")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn sanitize_step_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn stream_snippet(text: &str) -> String {
    let trimmed = text.trim();
    let mut snippet = trimmed.chars().take(1200).collect::<String>();
    if trimmed.chars().nth(1200).is_some() {
        snippet.push_str("...");
    }
    snippet
}

fn first_failure(
    expected_exit_code: i32,
    expected_schema: &str,
    expect_clean_stderr: bool,
    event: &CommandEvent,
) -> Option<String> {
    if event.exit_code != expected_exit_code {
        return Some(format!(
            "exit code mismatch: expected {}, got {}",
            expected_exit_code, event.exit_code
        ));
    }
    if !event.stdout_json_valid {
        return Some("stdout was not valid JSON".to_owned());
    }
    if event.stdout_schema.as_deref() != Some(expected_schema) {
        return Some(format!(
            "schema mismatch: expected {}, got {:?}",
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(&spec.args).env("NO_COLOR", "1");
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
        schema_validation_status: "not_checked",
        redaction_status: "not_checked",
        first_failure: None,
    };
    event.schema_validation_status = if parsed_stdout.is_some()
        && event.stdout_schema.as_deref() == Some(spec.expected_schema)
    {
        "passed"
    } else {
        "failed"
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

fn write_test_event_log_from_command_events(
    source_log_path: &Path,
    test_log_path: &Path,
    scenario_id: &'static str,
    command_count: usize,
) -> TestResult {
    let source_text = fs::read_to_string(source_log_path).map_err(|error| {
        format!(
            "failed to read command JSONL log {}: {error}",
            source_log_path.display()
        )
    })?;
    let command_lines = source_text.lines().take(command_count).collect::<Vec<_>>();
    ensure_equal(
        &command_lines.len(),
        &command_count,
        "command event source count for ee.test_event.v1 log",
    )?;
    for (index, line) in command_lines.iter().enumerate() {
        let event: JsonValue = serde_json::from_str(line).map_err(|error| {
            format!("command event {index} must parse before ee.test_event.v1 conversion: {error}")
        })?;
        let stdout = event
            .get("stdout")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let stderr_excerpt = event
            .get("stderr")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect::<String>();
        let args = event
            .get("args")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let test_event = TestEvent {
            schema: "ee.test_event.v1",
            ts: test_event_timestamp(),
            test_id: scenario_id,
            kind: "command_end",
            command: Some("ee".to_owned()),
            args: Some(args),
            stdout_hash: Some(blake3_hash_field(stdout.as_bytes())),
            stderr_excerpt: Some(stderr_excerpt),
            exit_code: event
                .get("exitCode")
                .and_then(JsonValue::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            elapsed_ms: event.get("elapsedMs").and_then(JsonValue::as_f64),
            fields: json!({
                "step": event.get("step").cloned().unwrap_or(JsonValue::Null),
                "workspace": event.get("workspace").cloned().unwrap_or(JsonValue::Null),
                "stdout_artifact_path": event
                    .get("stdoutArtifactPath")
                    .cloned()
                    .unwrap_or(JsonValue::Null),
                "stderr_artifact_path": event
                    .get("stderrArtifactPath")
                    .cloned()
                    .unwrap_or(JsonValue::Null),
                "stdout_schema": event.get("stdoutSchema").cloned().unwrap_or(JsonValue::Null),
                "schema_validation_status": event
                    .get("schemaValidationStatus")
                    .cloned()
                    .unwrap_or(JsonValue::Null),
                "first_failure": event.get("firstFailure").cloned().unwrap_or(JsonValue::Null),
            }),
        };
        append_jsonl(test_log_path, &test_event)?;
    }
    append_jsonl(
        test_log_path,
        &TestEvent {
            schema: "ee.test_event.v1",
            ts: test_event_timestamp(),
            test_id: scenario_id,
            kind: "note",
            command: None,
            args: None,
            stdout_hash: None,
            stderr_excerpt: None,
            exit_code: None,
            elapsed_ms: None,
            fields: json!({
                "message": "handoff_no_mocks_cycle_complete",
                "command_count": command_count,
            }),
        },
    )
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

fn degradation_codes_at(
    value: &JsonValue,
    pointer: &str,
    context: &str,
) -> Result<Vec<String>, String> {
    let mut codes = json_array(value, pointer, context)?
        .iter()
        .filter_map(|item| item.get("code").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codes.sort();
    Ok(codes)
}

fn capsule_json(path: &Path) -> Result<JsonValue, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read capsule {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("capsule JSON malformed at {}: {error}", path.display()))
}

fn integrity_string<'a>(
    capsule: &'a JsonValue,
    field: &str,
    context: &str,
) -> Result<&'a str, String> {
    capsule
        .pointer(&format!("/integrity/{field}"))
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{context} missing integrity field {field}"))
}

#[test]
fn no_mocks_handoff_cli_cycle_verifies_hmac_and_rejects_tampering() -> TestResult {
    let scenario_id = "phase3_no_mocks_handoff_cycle";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifact_dir = log_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let events_path = log_dir.join("commands.jsonl");
    let test_events_path = log_dir.join("ee-test-events.jsonl");

    let workspace_temp = tempfile::Builder::new()
        .prefix("ee-handoff-no-mocks-workspace-")
        .tempdir()
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = workspace_temp.path().to_path_buf();
    let workspace_arg = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db");
    let capsule_path = workspace.join("handoff-capsule.json");
    let tampered_capsule_path = workspace.join("handoff-capsule-tampered.json");
    let capsule_arg = capsule_path.display().to_string();
    let tampered_capsule_arg = tampered_capsule_path.display().to_string();

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
            expected_schema: "ee.response.v2",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure(database_path.is_file(), "init must create a real database")?;

    let (_remember_event, remember_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "02_remember_handoff_rule",
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
                "handoff,integrity,hmac".to_owned(),
                "--source".to_owned(),
                "file://tests/handoff_no_mocks_e2e.rs#L1".to_owned(),
                "Handoff capsules must verify their HMAC before resume.".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.response.v2",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    let handoff_rule_id = json_string(&remember_json, "/data/memory_id", "handoff rule")?;

    let (_preview_event, preview_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "03_handoff_preview",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "handoff".to_owned(),
                "preview".to_owned(),
                "--profile".to_owned(),
                "handoff".to_owned(),
                "--estimates".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.handoff.preview.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &preview_json.pointer("/sufficient_for_resume"),
        &Some(&JsonValue::Bool(true)),
        "handoff preview must be sufficient for resume",
    )?;
    let planned_sections = json_array(&preview_json, "/planned_sections", "handoff preview")?;
    ensure(
        planned_sections.iter().any(|section| {
            section.get("id").and_then(JsonValue::as_str) == Some("swarm_brief_summary")
        }),
        "handoff preview must include a redacted swarm brief summary section",
    )?;

    let (_create_event, create_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "04_handoff_create",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "handoff".to_owned(),
                "create".to_owned(),
                "--out".to_owned(),
                capsule_arg.clone(),
                "--profile".to_owned(),
                "handoff".to_owned(),
                "--redaction".to_owned(),
                "standard".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.handoff.create.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure(
        capsule_path.is_file(),
        "handoff create must write a capsule",
    )?;
    let capsule_id = json_string(&create_json, "/capsule_id", "handoff create")?;
    let create_content_hash = json_string(&create_json, "/content_hash", "handoff create")?;
    ensure(
        create_content_hash.starts_with("blake3:"),
        format!("handoff create content hash must be blake3-prefixed, got {create_content_hash}"),
    )?;
    ensure(
        create_json.pointer("/redaction_summary").is_some(),
        "handoff create must report a redaction summary",
    )?;
    ensure(
        create_json
            .pointer("/swarm_brief_summary/counts/activeConflictCount")
            .is_some(),
        "handoff create must carry reservation-conflict posture counts in the swarm summary",
    )?;

    let capsule_before_rotate = capsule_json(&capsule_path)?;
    ensure_equal(
        &capsule_before_rotate.pointer("/schema"),
        &Some(&json!("ee.handoff.capsule.v1")),
        "capsule schema",
    )?;
    ensure_equal(
        &capsule_before_rotate.pointer("/capsule_id"),
        &Some(&JsonValue::String(capsule_id.to_owned())),
        "capsule id",
    )?;
    ensure_equal(
        &Some(integrity_string(
            &capsule_before_rotate,
            "algorithm",
            "created capsule",
        )?),
        &Some("hmac-sha256"),
        "created capsule HMAC algorithm",
    )?;
    ensure_equal(
        &Some(integrity_string(
            &capsule_before_rotate,
            "keyMode",
            "created capsule",
        )?),
        &Some("workspace_secret"),
        "created capsule HMAC key mode",
    )?;
    let hmac_before = integrity_string(&capsule_before_rotate, "hmac", "created capsule")?;
    let hmac_prefix_before =
        integrity_string(&capsule_before_rotate, "hmacPrefix", "created capsule")?;
    let body_sha_before =
        integrity_string(&capsule_before_rotate, "bodySha256", "created capsule")?;
    ensure(
        hmac_before.starts_with("base64url:"),
        format!("created capsule HMAC must be base64url-tagged, got {hmac_before}"),
    )?;
    ensure_equal(
        &hmac_prefix_before.len(),
        &8_usize,
        "created capsule HMAC prefix length",
    )?;
    ensure(
        body_sha_before.starts_with("sha256:"),
        format!("created capsule body hash must be sha256-prefixed, got {body_sha_before}"),
    )?;

    let (_inspect_event, inspect_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "05_handoff_inspect",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "handoff".to_owned(),
                "inspect".to_owned(),
                capsule_arg.clone(),
                "--verify-hash".to_owned(),
                "--check-evidence".to_owned(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.handoff.inspect.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &inspect_json.pointer("/capsule_id"),
        &Some(&JsonValue::String(capsule_id.to_owned())),
        "inspect must echo capsule id",
    )?;
    ensure_equal(
        &inspect_json.pointer("/validation_status"),
        &Some(&json!("valid")),
        "inspect validation status",
    )?;
    ensure_equal(
        &inspect_json.pointer("/hash_valid"),
        &Some(&JsonValue::Bool(true)),
        "inspect hash_valid",
    )?;
    let section_count = inspect_json
        .pointer("/section_count")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    ensure(
        section_count >= 3,
        format!("inspect must see multiple capsule sections, got {section_count}"),
    )?;

    let (_resume_event, resume_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "06_handoff_resume",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "handoff".to_owned(),
                "resume".to_owned(),
                capsule_arg.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.handoff.resume.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &resume_json.pointer("/capsule_id"),
        &Some(&JsonValue::String(capsule_id.to_owned())),
        "resume must echo capsule id",
    )?;
    ensure(
        !degradation_codes_at(&resume_json, "/degradations", "handoff resume")?
            .iter()
            .any(|code| code == "handoff_hmac_skipped"),
        "normal resume must verify HMAC instead of skipping it",
    )?;
    ensure(
        resume_json.pointer("/prompt_fragment").is_some(),
        "resume must render a prompt fragment",
    )?;
    let status_summary = json_string(&resume_json, "/status_summary", "handoff resume")?;
    ensure(
        status_summary.contains("active_conflicts="),
        "resume status must preserve reservation-conflict posture from the embedded swarm brief",
    )?;

    let (_rotate_event, rotate_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "07_handoff_rotate_key",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "handoff".to_owned(),
                "rotate-key".to_owned(),
                "--capsule".to_owned(),
                capsule_arg.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.handoff.rotate_key.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &rotate_json.pointer("/capsule_id"),
        &Some(&JsonValue::String(capsule_id.to_owned())),
        "rotate-key must echo capsule id",
    )?;
    ensure_equal(
        &rotate_json.pointer("/body_preserved"),
        &Some(&JsonValue::Bool(true)),
        "rotate-key must preserve signed body",
    )?;
    ensure_equal(
        &rotate_json.pointer("/old_hmac_prefix"),
        &Some(&JsonValue::String(hmac_prefix_before.to_owned())),
        "rotate-key must report old hmac prefix",
    )?;
    let hmac_prefix_after = json_string(&rotate_json, "/new_hmac_prefix", "rotate-key")?;
    ensure(
        hmac_prefix_after != hmac_prefix_before,
        "rotate-key must replace the capsule HMAC",
    )?;
    ensure_equal(
        &rotate_json.pointer("/canonical_content_hash_before"),
        &rotate_json.pointer("/canonical_content_hash_after"),
        "rotate-key must preserve canonical body hash",
    )?;

    let capsule_after_rotate = capsule_json(&capsule_path)?;
    ensure_equal(
        &Some(integrity_string(
            &capsule_after_rotate,
            "hmacPrefix",
            "rotated capsule",
        )?),
        &Some(hmac_prefix_after),
        "rotated capsule HMAC prefix must match report",
    )?;

    let (_resume_rotated_event, resume_rotated_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "08_handoff_resume_rotated",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "handoff".to_owned(),
                "resume".to_owned(),
                capsule_arg.clone(),
            ],
            expected_exit_code: 0,
            expected_schema: "ee.handoff.resume.v1",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &resume_rotated_json.pointer("/capsule_id"),
        &Some(&JsonValue::String(capsule_id.to_owned())),
        "resume after rotate must echo capsule id",
    )?;
    ensure(
        !degradation_codes_at(
            &resume_rotated_json,
            "/degradations",
            "handoff resume rotated",
        )?
        .iter()
        .any(|code| code == "handoff_hmac_skipped"),
        "resume after rotate must verify the replacement HMAC",
    )?;

    let mut tampered_capsule = capsule_after_rotate.clone();
    let sections = tampered_capsule
        .get_mut("sections")
        .and_then(JsonValue::as_array_mut)
        .ok_or_else(|| "capsule missing sections for tamper setup".to_owned())?;
    let first_section = sections
        .first_mut()
        .ok_or_else(|| "capsule has no sections to tamper".to_owned())?;
    first_section["content"] =
        JsonValue::String("tampered signed body content from no-mocks E2E".to_owned());
    write_text(
        &tampered_capsule_path,
        &serde_json::to_string_pretty(&tampered_capsule).map_err(|error| error.to_string())?,
    )?;

    let (_resume_tampered_event, resume_tampered_json) = run_step(
        scenario_id,
        &events_path,
        &artifact_dir,
        &workspace,
        StepSpec {
            name: "09_handoff_resume_tampered",
            args: vec![
                "--workspace".to_owned(),
                workspace_arg.clone(),
                "--json".to_owned(),
                "handoff".to_owned(),
                "resume".to_owned(),
                tampered_capsule_arg,
            ],
            expected_exit_code: 6,
            expected_schema: "ee.error.v2",
            expect_clean_stderr: true,
        },
    )?;
    command_count += 1;
    ensure_equal(
        &resume_tampered_json.pointer("/error/code"),
        &Some(&json!("handoff_capsule_tampered")),
        "tampered resume error code",
    )?;
    ensure_equal(
        &resume_tampered_json.pointer("/error/severity"),
        &Some(&json!("high")),
        "tampered resume error severity",
    )?;
    let tampered_repair = json_string(
        &resume_tampered_json,
        "/error/repair",
        "tampered resume error",
    )?;
    ensure(
        tampered_repair.contains("Discard the capsule"),
        "tampered resume repair must tell agents to discard and recreate the capsule",
    )?;

    write_test_event_log_from_command_events(
        &events_path,
        &test_events_path,
        scenario_id,
        command_count,
    )?;
    let test_events_text = fs::read_to_string(&test_events_path).map_err(|error| {
        format!(
            "failed to read ee.test_event.v1 log {}: {error}",
            test_events_path.display()
        )
    })?;
    let test_event_lines = test_events_text.lines().collect::<Vec<_>>();
    ensure_equal(
        &test_event_lines.len(),
        &(command_count + 1),
        "ee.test_event.v1 log count includes commands plus completion note",
    )?;
    for (index, line) in test_event_lines.iter().enumerate() {
        let event: JsonValue = serde_json::from_str(line)
            .map_err(|error| format!("ee.test_event.v1 event {index} must parse: {error}"))?;
        ensure_equal(
            &event.pointer("/schema"),
            &Some(&json!("ee.test_event.v1")),
            "structured test event schema",
        )?;
        ensure(
            event.pointer("/ts").is_some()
                && event.pointer("/test_id") == Some(&json!(scenario_id))
                && event.pointer("/kind").is_some(),
            "structured test event must include ts, test_id, and kind",
        )?;
    }

    append_jsonl(
        &events_path,
        &json!({
            "schema": "ee.e2e.summary_event.v1",
            "scenarioId": scenario_id,
            "event": "summary",
            "commandCount": command_count,
            "workspace": workspace.display().to_string(),
            "databasePath": database_path.display().to_string(),
            "capsulePath": capsule_path.display().to_string(),
            "tamperedCapsulePath": tampered_capsule_path.display().to_string(),
            "capsuleId": capsule_id,
            "handoffRuleMemoryId": handoff_rule_id,
            "contentHash": create_content_hash,
            "oldHmacPrefix": hmac_prefix_before,
            "newHmacPrefix": hmac_prefix_after,
            "testEventLogPath": test_events_path.display().to_string(),
        }),
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
