//! No-mock E2E coverage for malformed swarm replay inputs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const TEST_EVENT_SCHEMA_V1: &str = "ee.test_event.v1";

struct ReplayCase {
    fixture_id: &'static str,
    expected_outcome: ExpectedOutcome,
    trace_text: String,
}

enum ExpectedOutcome {
    Error { code: &'static str },
    BlockedLedger { code: &'static str },
}

struct LoggedCommand {
    stdout: String,
    stderr: String,
    exit_code: i32,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    elapsed_ms: u128,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_workspace(label: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let workspace = std::env::temp_dir().join(format!(
        "ee-lab-swarm-replay-malformed-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("create {}: {error}", workspace.display()))?;
    Ok(workspace)
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn hash_text(text: &str) -> String {
    hash_bytes(text.as_bytes())
}

fn path_hash(path: &Path) -> String {
    hash_text(&path.display().to_string())
}

fn path_tail(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .ok()
        .and_then(|relative| relative.to_str())
        .filter(|relative| !relative.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "[PATH]".to_owned())
}

fn sanitized_args(workspace: &Path, args: &[String]) -> Vec<String> {
    let workspace_text = workspace.display().to_string();
    args.iter()
        .map(|arg| {
            if arg == &workspace_text {
                "[WORKSPACE]".to_owned()
            } else if arg.starts_with(&workspace_text) {
                format!("[WORKSPACE]/{}", path_tail(workspace, Path::new(arg)))
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn write_event(
    log_path: &Path,
    test_id: &str,
    kind: &str,
    command: Option<&str>,
    args: Option<&[String]>,
    exit_code: Option<i32>,
    elapsed_ms: Option<u128>,
    fields: Value,
) -> TestResult {
    let mut event = json!({
        "schema": TEST_EVENT_SCHEMA_V1,
        "ts": now_rfc3339(),
        "test_id": test_id,
        "kind": kind,
        "fields": fields,
    });
    if let Some(command) = command {
        event["command"] = Value::String(command.to_owned());
    }
    if let Some(args) = args {
        event["args"] = Value::Array(args.iter().cloned().map(Value::String).collect());
    }
    if let Some(exit_code) = exit_code {
        event["exit_code"] = Value::from(exit_code);
    }
    if let Some(elapsed_ms) = elapsed_ms {
        event["elapsed_ms"] = Value::from(elapsed_ms as u64);
    }
    let mut rendered = serde_json::to_string(&event).map_err(|error| error.to_string())?;
    rendered.push('\n');
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, rendered.as_bytes()))
        .map_err(|error| format!("write {}: {error}", log_path.display()))
}

fn run_ee_logged(
    test_id: &str,
    workspace: &Path,
    artifacts_dir: &Path,
    log_path: &Path,
    fixture_id: &str,
    args: &[String],
) -> Result<LoggedCommand, String> {
    fs::create_dir_all(artifacts_dir)
        .map_err(|error| format!("create {}: {error}", artifacts_dir.display()))?;
    let stdout_path = artifacts_dir.join(format!("{fixture_id}.stdout.json"));
    let stderr_path = artifacts_dir.join(format!("{fixture_id}.stderr.txt"));
    let sanitized_args = sanitized_args(workspace, args);
    let command_text = format!("ee {}", sanitized_args.join(" "));
    let cwd = std::env::current_dir().map_err(|error| format!("read cwd: {error}"))?;
    write_event(
        log_path,
        test_id,
        "command_start",
        Some("ee"),
        Some(&sanitized_args),
        None,
        None,
        json!({
            "fixture_id": fixture_id,
            "command": command_text,
            "cwd_hash": path_hash(&cwd),
            "workspace_hash": path_hash(workspace),
            "workspace_path_hash": path_hash(workspace),
            "sanitized_env_overrides": {
                "NO_COLOR": "1"
            }
        }),
    )?;

    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env("NO_COLOR", "1")
        .env_remove("EE_WORKSPACE")
        .output()
        .map_err(|error| format!("run ee {fixture_id}: {error}"))?;
    let elapsed_ms = start.elapsed().as_millis();
    fs::write(&stdout_path, &output.stdout)
        .map_err(|error| format!("write {}: {error}", stdout_path.display()))?;
    fs::write(&stderr_path, &output.stderr)
        .map_err(|error| format!("write {}: {error}", stderr_path.display()))?;
    let logged = logged_command(output, stdout_path, stderr_path, elapsed_ms)?;

    write_event(
        log_path,
        test_id,
        "command_end",
        Some("ee"),
        Some(&sanitized_args),
        Some(logged.exit_code),
        Some(logged.elapsed_ms),
        json!({
            "fixture_id": fixture_id,
            "cwd_hash": path_hash(&cwd),
            "workspace_hash": path_hash(workspace),
            "stdout_artifact_path": path_tail(workspace, &logged.stdout_path),
            "stderr_artifact_path": path_tail(workspace, &logged.stderr_path),
            "stdout_artifact_path_hash": path_hash(&logged.stdout_path),
            "stderr_artifact_path_hash": path_hash(&logged.stderr_path),
            "stdout_hash": hash_text(&logged.stdout),
            "stderr_hash": hash_text(&logged.stderr),
            "stdout_bytes": logged.stdout.len(),
            "stderr_bytes": logged.stderr.len()
        }),
    )?;

    Ok(logged)
}

fn logged_command(
    output: Output,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    elapsed_ms: u128,
) -> Result<LoggedCommand, String> {
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8: {error}"))?;
    Ok(LoggedCommand {
        stdout,
        stderr,
        exit_code: output.status.code().unwrap_or(255),
        stdout_path,
        stderr_path,
        elapsed_ms,
    })
}

fn base_trace(workspace: &Path, artifacts_dir: &Path, log_path: &Path) -> Result<Value, String> {
    let args = vec![
        "--workspace".to_owned(),
        workspace.display().to_string(),
        "--json".to_owned(),
        "lab".to_owned(),
        "generate-workload".to_owned(),
        "--fixture-seed".to_owned(),
        "malformed_e2e_seed_001".to_owned(),
        "--profile".to_owned(),
        "small".to_owned(),
    ];
    let output = run_ee_logged(
        "swarm_replay_malformed_e2e",
        workspace,
        artifacts_dir,
        log_path,
        "valid-generator",
        &args,
    )?;
    ensure(
        output.exit_code == 0,
        format!(
            "generate-workload failed: exit={} stdout={} stderr={}",
            output.exit_code, output.stdout, output.stderr
        ),
    )?;
    ensure(
        output.stderr.trim().is_empty(),
        format!(
            "generate-workload wrote diagnostics to stderr: {}",
            output.stderr
        ),
    )?;
    serde_json::from_str(&output.stdout)
        .map_err(|error| format!("parse generated workload JSON: {error}: {}", output.stdout))
}

fn command_sequence_mut(value: &mut Value) -> Result<&mut Vec<Value>, String> {
    value["commandSequence"]
        .as_array_mut()
        .ok_or_else(|| "generated trace missing commandSequence array".to_owned())
}

fn replay_cases(base: &Value) -> Result<Vec<ReplayCase>, String> {
    let mut wrong_schema = base.clone();
    wrong_schema["schema"] = Value::String("ee.swarm_workload.v9".to_owned());

    let mut side_effects = base.clone();
    side_effects["sideEffectFree"] = Value::Bool(false);

    let mut empty_commands = base.clone();
    empty_commands["commandSequence"] = Value::Array(Vec::new());

    let mut bad_dependency = base.clone();
    command_sequence_mut(&mut bad_dependency)?[0]["dependsOn"] = json!(["missing_e2e_step"]);

    let mut blocked_command = base.clone();
    blocked_command["resourceProfileHints"]["rchRequired"] = Value::Bool(false);
    let commands = command_sequence_mut(&mut blocked_command)?;
    commands.truncate(1);
    commands[0]["command"]["verbs"] = json!(["bash", "rm", "-rf"]);
    commands[0]["command"]["positionalArity"] = Value::from(0u64);
    commands[0]["command"]["flagNames"] = Value::Array(Vec::new());

    Ok(vec![
        ReplayCase {
            fixture_id: "malformed-json",
            expected_outcome: ExpectedOutcome::Error { code: "usage" },
            trace_text: "{".to_owned(),
        },
        ReplayCase {
            fixture_id: "wrong-schema",
            expected_outcome: ExpectedOutcome::Error { code: "usage" },
            trace_text: serde_json::to_string_pretty(&wrong_schema)
                .map_err(|error| error.to_string())?,
        },
        ReplayCase {
            fixture_id: "side-effect-free-false",
            expected_outcome: ExpectedOutcome::Error {
                code: "policy_denied",
            },
            trace_text: serde_json::to_string_pretty(&side_effects)
                .map_err(|error| error.to_string())?,
        },
        ReplayCase {
            fixture_id: "empty-command-sequence",
            expected_outcome: ExpectedOutcome::Error { code: "usage" },
            trace_text: serde_json::to_string_pretty(&empty_commands)
                .map_err(|error| error.to_string())?,
        },
        ReplayCase {
            fixture_id: "unknown-step-dependency",
            expected_outcome: ExpectedOutcome::Error { code: "usage" },
            trace_text: serde_json::to_string_pretty(&bad_dependency)
                .map_err(|error| error.to_string())?,
        },
        ReplayCase {
            fixture_id: "disallowed-shell-command",
            expected_outcome: ExpectedOutcome::BlockedLedger {
                code: "swarm_replay_command_not_allowlisted",
            },
            trace_text: serde_json::to_string_pretty(&blocked_command)
                .map_err(|error| error.to_string())?,
        },
    ])
}

fn replay_args(workspace: &Path, trace_path: &Path) -> Vec<String> {
    vec![
        "--workspace".to_owned(),
        workspace.display().to_string(),
        "--json".to_owned(),
        "lab".to_owned(),
        "swarm".to_owned(),
        "replay".to_owned(),
        "--trace".to_owned(),
        trace_path.display().to_string(),
        "--dry-run".to_owned(),
    ]
}

fn parsed_stdout(output: &LoggedCommand, fixture_id: &str) -> Result<Value, String> {
    serde_json::from_str(&output.stdout).map_err(|error| {
        format!(
            "{fixture_id}: stdout must be JSON: {error}: {}",
            output.stdout
        )
    })
}

fn first_failure_diagnosis(value: &Value) -> String {
    value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .map(|code| format!("error:{code}"))
        .or_else(|| {
            value
                .pointer("/firstFailure/code")
                .and_then(Value::as_str)
                .map(|code| format!("firstFailure:{code}"))
        })
        .unwrap_or_else(|| "none".to_owned())
}

fn validate_case_output(case: &ReplayCase, output: &LoggedCommand, parsed: &Value) -> TestResult {
    ensure(
        output.exit_code != 0,
        format!("{} should fail or degrade, got exit 0", case.fixture_id),
    )?;
    ensure(
        output.stderr.trim().is_empty(),
        format!(
            "{} leaked diagnostics to stderr: {}",
            case.fixture_id, output.stderr
        ),
    )?;

    match case.expected_outcome {
        ExpectedOutcome::Error { code } => {
            ensure(
                parsed["schema"] == "ee.error.v2",
                format!("{} should emit ee.error.v2: {parsed}", case.fixture_id),
            )?;
            ensure(
                parsed["error"]["code"] == code,
                format!(
                    "{} expected error code {code}, got {parsed}",
                    case.fixture_id
                ),
            )
        }
        ExpectedOutcome::BlockedLedger { code } => {
            ensure(
                parsed["schema"] == "ee.swarm_replay_result.v1",
                format!(
                    "{} should emit swarm replay result: {parsed}",
                    case.fixture_id
                ),
            )?;
            ensure(
                parsed["status"] == "blocked",
                format!("{} should be blocked: {parsed}", case.fixture_id),
            )?;
            ensure(
                parsed["firstFailure"]["code"] == code,
                format!(
                    "{} expected first failure {code}, got {parsed}",
                    case.fixture_id
                ),
            )
        }
    }
}

fn assert_redacted(case: &ReplayCase, workspace: &Path, output: &LoggedCommand) -> TestResult {
    for forbidden in [
        workspace.display().to_string(),
        "/Users/".to_owned(),
        "/data/projects/".to_owned(),
        "SECRET_TOKEN".to_owned(),
        "raw task content".to_owned(),
        "raw query text".to_owned(),
        "memory body payload".to_owned(),
        "mail body payload".to_owned(),
    ] {
        ensure(
            !output.stdout.contains(&forbidden),
            format!("{} leaked forbidden marker {forbidden}", case.fixture_id),
        )?;
    }
    ensure(
        !output.stdout.contains("rm -rf"),
        format!("{} leaked raw destructive command", case.fixture_id),
    )
}

#[test]
fn lab_swarm_replay_malformed_inputs_emit_logged_machine_safe_results() -> TestResult {
    let workspace = unique_workspace("cases")?;
    let artifacts_dir = workspace.join("artifacts");
    let log_path = workspace.join("events.jsonl");
    let base = base_trace(&workspace, &artifacts_dir, &log_path)?;
    let cases = replay_cases(&base)?;
    ensure(cases.len() >= 6, "malformed e2e corpus too small")?;

    for case in &cases {
        let trace_path = workspace.join(format!("{}.json", case.fixture_id));
        fs::write(&trace_path, &case.trace_text)
            .map_err(|error| format!("write {}: {error}", trace_path.display()))?;
        let fixture_hash = hash_text(&case.trace_text);
        let args = replay_args(&workspace, &trace_path);
        let output = run_ee_logged(
            "swarm_replay_malformed_e2e",
            &workspace,
            &artifacts_dir,
            &log_path,
            case.fixture_id,
            &args,
        )?;
        let parsed = parsed_stdout(&output, case.fixture_id)?;
        validate_case_output(case, &output, &parsed)?;
        assert_redacted(case, &workspace, &output)?;

        write_event(
            &log_path,
            "swarm_replay_malformed_e2e",
            "assert_ok",
            None,
            None,
            None,
            None,
            json!({
                "label": format!("{} classified", case.fixture_id),
                "fixture_id": case.fixture_id,
                "fixture_hash": fixture_hash,
                "schema_validation_status": match case.expected_outcome {
                    ExpectedOutcome::Error { .. } => "invalid_or_policy_denied",
                    ExpectedOutcome::BlockedLedger { .. } => "schema_valid_runner_refused",
                },
                "golden_validation_status": "not_required_for_malformed_e2e",
                "schema_or_golden_validation_status": "schema_checked_golden_not_required",
                "redaction_status": "passed",
                "cwd_hash": path_hash(&std::env::current_dir().map_err(|error| format!("read cwd: {error}"))?),
                "workspace_hash": path_hash(&workspace),
                "stdout_artifact_path": path_tail(&workspace, &output.stdout_path),
                "stderr_artifact_path": path_tail(&workspace, &output.stderr_path),
                "stdout_artifact_path_hash": path_hash(&output.stdout_path),
                "stderr_artifact_path_hash": path_hash(&output.stderr_path),
                "stdout_machine_data": true,
                "stderr_diagnostics_empty": output.stderr.trim().is_empty(),
                "exit_code": output.exit_code,
                "elapsed_ms": output.elapsed_ms as u64,
                "first_failure_diagnosis": first_failure_diagnosis(&parsed)
            }),
        )?;
    }

    let log_text = fs::read_to_string(&log_path).map_err(|error| format!("read log: {error}"))?;
    let mut line_count = 0usize;
    for line in log_text.lines() {
        line_count += 1;
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("log line JSON: {error}: {line}"))?;
        ensure(
            value["schema"] == TEST_EVENT_SCHEMA_V1,
            format!("event schema mismatch: {value}"),
        )?;
        ensure(
            value["test_id"] == "swarm_replay_malformed_e2e",
            format!("event test id mismatch: {value}"),
        )?;
    }
    ensure(line_count > cases.len() * 3, "missing e2e log events")?;
    ensure(
        !log_text.contains("/Users/") && !log_text.contains("rm -rf"),
        "e2e log leaked private path or raw destructive command",
    )?;
    ensure(
        !log_text.contains(&workspace.display().to_string()),
        "e2e log leaked raw temp workspace path",
    )
}
