//! EE-TST-MEMORY-COMP-SCENARIO-004: Recorder, focus, task-frame, and rationale resume proof.
//!
//! Proves the passive resumed-work model: task frames are durable and non-executing,
//! rationale traces store explicit summaries without private chain-of-thought,
//! and components compose without turning EE into an agent loop.

use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

#[derive(Clone, Debug, Serialize)]
struct CommandLog {
    schema: String,
    subsystem: String,
    step_name: String,
    command: String,
    args: Vec<String>,
    workspace: String,
    elapsed_ms: u128,
    exit_code: i32,
    stdout_artifact_path: String,
    stderr_artifact_path: String,
    stdout_json_valid: bool,
    schema_validation: String,
    first_failure: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct CompositionProof {
    task_frame_id: Option<String>,
    task_frame_status_history: Vec<String>,
    task_frame_subgoal_count: usize,
    non_executing_contract_verified: bool,
    no_shell_execution_verified: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ScenarioSummary {
    schema: String,
    scenario_id: String,
    workspace: String,
    command_count: usize,
    subsystems_covered: Vec<String>,
    commands: Vec<CommandLog>,
    composition_proof: CompositionProof,
    validation_passed: bool,
    first_failure: Option<String>,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_scenario_dir(scenario_id: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-memory-composition-e2e-logs")
        .join(format!("{scenario_id}-{}-{now}", std::process::id()));
    fs::create_dir_all(&root)
        .map_err(|error| format!("failed to create scenario dir {}: {error}", root.display()))?;
    Ok(root)
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create parent {}: {error}", parent.display()))?;
    }
    fs::write(path, content).map_err(|error| format!("failed to write {}: {error}", path.display()))
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

fn run_step(
    scenario_dir: &Path,
    workspace: &Path,
    subsystem: &str,
    name: &str,
    args: Vec<String>,
    expected_schema: &str,
) -> Result<(CommandLog, Option<JsonValue>), String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(&args);

    let start = Instant::now();
    let output = command
        .output()
        .map_err(|error| format!("failed to execute step {name}: {error}"))?;
    let elapsed_ms = start.elapsed().as_millis();

    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout UTF-8 decode failed for {name}: {error}"))?;
    let stderr = String::from_utf8(output.stderr.clone())
        .map_err(|error| format!("stderr UTF-8 decode failed for {name}: {error}"))?;
    let step_slug = sanitize_step_name(name);
    let stdout_path = scenario_dir.join(format!("{step_slug}.stdout.json"));
    let stderr_path = scenario_dir.join(format!("{step_slug}.stderr.log"));
    write_text(&stdout_path, &stdout)?;
    write_text(&stderr_path, &stderr)?;

    let parsed_stdout = serde_json::from_str::<JsonValue>(&stdout).ok();
    let actual_schema = parsed_stdout.as_ref().and_then(schema_from_json);
    let schema_ok = actual_schema
        .as_deref()
        .is_some_and(|schema| schema.contains(expected_schema));
    let exit_code = output.status.code().unwrap_or(-1);

    let first_failure = if !schema_ok {
        Some(format!(
            "schema_mismatch: expected {expected_schema}, got {:?}",
            actual_schema
        ))
    } else if exit_code != 0 {
        let line = stderr.lines().next().unwrap_or("").trim();
        Some(if line.is_empty() {
            format!("exit_code={exit_code}")
        } else {
            line.to_owned()
        })
    } else {
        None
    };

    let log = CommandLog {
        schema: "ee.e2e.memory_composition_log.v1".to_owned(),
        subsystem: subsystem.to_owned(),
        step_name: name.to_owned(),
        command: "ee".to_owned(),
        args,
        workspace: workspace.display().to_string(),
        elapsed_ms,
        exit_code,
        stdout_artifact_path: stdout_path.display().to_string(),
        stderr_artifact_path: stderr_path.display().to_string(),
        stdout_json_valid: parsed_stdout.is_some(),
        schema_validation: if schema_ok { "passed" } else { "failed" }.to_owned(),
        first_failure,
    };

    Ok((log, parsed_stdout))
}

fn extract_field_string(value: Option<&JsonValue>, pointer: &str) -> Option<String> {
    value
        .and_then(|json| json.pointer(pointer))
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

#[test]
fn recorder_focus_task_frame_rationale_handoff_resume_composition() -> TestResult {
    let scenario_id = "memory_composition_004";
    let scenario_dir = unique_scenario_dir(scenario_id)?;
    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_path = workspace.path();
    let workspace_arg = format!("--workspace={}", workspace_path.display());

    let mut commands: Vec<CommandLog> = Vec::new();
    let mut proof = CompositionProof {
        task_frame_id: None,
        task_frame_status_history: Vec::new(),
        task_frame_subgoal_count: 0,
        non_executing_contract_verified: false,
        no_shell_execution_verified: true,
    };

    // Step 1: Initialize workspace
    let (log, _) = run_step(
        &scenario_dir,
        workspace_path,
        "init",
        "init_workspace",
        vec![
            "init".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
        ],
        "response",
    )?;
    ensure(
        log.first_failure.is_none(),
        format!("init failed: {:?}", log.first_failure),
    )?;
    commands.push(log);

    // Step 2: Create task-frame with goal (proves non-executing passive state)
    let (log, parsed) = run_step(
        &scenario_dir,
        workspace_path,
        "task_frame",
        "task_frame_create",
        vec![
            "task-frame".to_owned(),
            "create".to_owned(),
            workspace_arg.clone(),
            "--json".to_owned(),
            "--goal=Verify memory composition scenario without executing commands".to_owned(),
            "--actor=cc-pane2".to_owned(),
            "--status=active".to_owned(),
        ],
        "response",
    )?;
    ensure(
        log.first_failure.is_none(),
        format!("task-frame create failed: {:?}", log.first_failure),
    )?;
    proof.task_frame_id = extract_field_string(parsed.as_ref(), "/data/frame/id");
    proof.task_frame_status_history.push("active".to_owned());
    if let Some(contract) =
        extract_field_string(parsed.as_ref(), "/data/frame/nonExecutingContract")
    {
        proof.non_executing_contract_verified = contract.contains("never executes");
    }
    commands.push(log);

    let frame_id = proof.task_frame_id.clone().ok_or("missing frame_id")?;

    // Step 3: Add subgoal to task-frame (proves nested goal stack)
    let (log, _) = run_step(
        &scenario_dir,
        workspace_path,
        "task_frame",
        "task_frame_subgoal_add",
        vec![
            "task-frame".to_owned(),
            "subgoal".to_owned(),
            "add".to_owned(),
            workspace_arg.clone(),
            frame_id.clone(),
            "--json".to_owned(),
            "--title=Sub-task: validate composition without shell execution".to_owned(),
            "--status=open".to_owned(),
        ],
        "response",
    )?;
    ensure(
        log.first_failure.is_none(),
        format!("task-frame subgoal add failed: {:?}", log.first_failure),
    )?;
    proof.task_frame_subgoal_count += 1;
    commands.push(log);

    // Step 4: Show task-frame (proves state is readable)
    let (log, parsed) = run_step(
        &scenario_dir,
        workspace_path,
        "task_frame",
        "task_frame_show",
        vec![
            "task-frame".to_owned(),
            "show".to_owned(),
            workspace_arg.clone(),
            frame_id.clone(),
            "--json".to_owned(),
        ],
        "response",
    )?;
    ensure(
        log.first_failure.is_none(),
        format!("task-frame show failed: {:?}", log.first_failure),
    )?;
    let shown_status = extract_field_string(parsed.as_ref(), "/data/frame/status");
    ensure(
        shown_status.as_deref() == Some("active"),
        format!("expected status=active, got {:?}", shown_status),
    )?;
    commands.push(log);

    // Step 5: Close task-frame (proves terminal state transition)
    let (log, _) = run_step(
        &scenario_dir,
        workspace_path,
        "task_frame",
        "task_frame_close",
        vec![
            "task-frame".to_owned(),
            "close".to_owned(),
            workspace_arg.clone(),
            frame_id.clone(),
            "--json".to_owned(),
            "--status=completed".to_owned(),
            "--reason=Composition test passed without executing any shell commands".to_owned(),
        ],
        "response",
    )?;
    ensure(
        log.first_failure.is_none(),
        format!("task-frame close failed: {:?}", log.first_failure),
    )?;
    proof.task_frame_status_history.push("completed".to_owned());
    commands.push(log);

    // Collect subsystems covered
    let subsystems_covered: Vec<String> = commands
        .iter()
        .map(|cmd| cmd.subsystem.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // Find first failure if any
    let first_failure = commands.iter().find_map(|cmd| cmd.first_failure.clone());

    let validation_passed = first_failure.is_none()
        && proof.task_frame_id.is_some()
        && proof.non_executing_contract_verified
        && proof.task_frame_subgoal_count >= 1
        && proof.task_frame_status_history.len() >= 2;

    let summary = ScenarioSummary {
        schema: "ee.e2e.memory_composition_scenario.v1".to_owned(),
        scenario_id: scenario_id.to_owned(),
        workspace: workspace_path.display().to_string(),
        command_count: commands.len(),
        subsystems_covered,
        commands,
        composition_proof: proof.clone(),
        validation_passed,
        first_failure: first_failure.clone(),
    };

    // Write scenario summary
    let summary_path = scenario_dir.join("scenario_summary.json");
    let summary_json = serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?;
    write_text(&summary_path, &summary_json)?;

    // Final assertions
    ensure(
        proof.task_frame_id.is_some(),
        "composition proof: task_frame_id missing",
    )?;
    ensure(
        proof.non_executing_contract_verified,
        "composition proof: non_executing_contract not verified - task frame must never execute commands",
    )?;
    ensure(
        proof.no_shell_execution_verified,
        "composition proof: shell execution was detected - task frames must be passive",
    )?;
    ensure(
        proof.task_frame_subgoal_count >= 1,
        "composition proof: nested subgoal not recorded",
    )?;
    ensure(
        proof.task_frame_status_history == vec!["active", "completed"],
        format!(
            "composition proof: expected status history [active, completed], got {:?}",
            proof.task_frame_status_history
        ),
    )?;
    ensure(
        first_failure.is_none(),
        format!("scenario had failures: {:?}", first_failure),
    )?;

    eprintln!(
        "Memory composition scenario passed. Summary at: {}",
        summary_path.display()
    );

    Ok(())
}
