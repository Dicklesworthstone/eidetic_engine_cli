use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, String>;

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct E2eWorkspace {
    path: PathBuf,
    home: PathBuf,
    log_path: PathBuf,
}

impl E2eWorkspace {
    fn create(test_name: &str) -> TestResult<Self> {
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock before UNIX_EPOCH: {error}"))?
            .as_nanos();
        let counter = WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join("ee-review-e2e").join(format!(
            "{test_name}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        let home = path.join("home");
        fs::create_dir_all(&home).map_err(|error| format!("create {}: {error}", home.display()))?;
        let log_path = path.join("doctor_capabilities_golden.events.jsonl");
        Ok(Self {
            path,
            home,
            log_path,
        })
    }

    fn as_str(&self) -> TestResult<&str> {
        self.path
            .to_str()
            .ok_or_else(|| format!("workspace path is not UTF-8: {}", self.path.display()))
    }

    fn log(&self, phase: &str, payload: Value) -> TestResult {
        let entry = json!({
            "schema": "ee.test_event.v1",
            "suite": "doctor_capabilities_golden_e2e",
            "phase": phase,
            "payload": payload,
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|error| format!("open {}: {error}", self.log_path.display()))?;
        writeln!(file, "{entry}")
            .map_err(|error| format!("write {}: {error}", self.log_path.display()))
    }
}

fn run_ee(workspace: &E2eWorkspace, phase: &str, args: &[&str]) -> Result<Output, String> {
    workspace.log(
        phase,
        json!({
            "event": "command_start",
            "argv": args,
        }),
    )?;
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env("HOME", &workspace.home)
        .env("EE_NO_COLOR", "1")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    workspace.log(
        phase,
        json!({
            "event": "command_finish",
            "argv": args,
            "status": output.status.code(),
            "success": output.status.success(),
            "durationMs": started.elapsed().as_millis(),
            "stdoutBytes": output.stdout.len(),
            "stderrBytes": output.stderr.len(),
        }),
    )?;
    Ok(output)
}

fn ensure_success(output: &Output, context: &str) -> TestResult {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{context} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn write_doctor_run_state(
    workspace: &E2eWorkspace,
    run_id: &str,
    started_at: &str,
    finished_at: Option<&str>,
    action_count: u64,
) -> TestResult<PathBuf> {
    let run_dir = workspace.path.join(".doctor").join("runs").join(run_id);
    fs::create_dir_all(&run_dir)
        .map_err(|error| format!("create {}: {error}", run_dir.display()))?;
    let state = json!({
        "schema": "ee.doctor.run_state.v1",
        "run_id": run_id,
        "target_sha": format!("sha256:{run_id}"),
        "workspace": path_string(&workspace.path),
        "started_at": started_at,
        "finished_at": finished_at,
        "status": "completed_ok",
        "action_count": action_count,
        "dry_run": false,
    });
    let state_path = run_dir.join("state.json");
    fs::write(&state_path, state.to_string())
        .map_err(|error| format!("write {}: {error}", state_path.display()))?;
    Ok(state_path)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("doctor_capabilities_scrubbed.snap")
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}

fn scrub_capabilities(mut value: Value, workspace: &E2eWorkspace) -> TestResult<String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "doctor capabilities output must be a JSON object".to_string())?;

    let doctor_version = object
        .get("doctor_version")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing doctor_version".to_string())?;
    if doctor_version != env!("CARGO_PKG_VERSION") {
        return Err(format!(
            "doctor_version drifted: expected {}, got {doctor_version}",
            env!("CARGO_PKG_VERSION")
        ));
    }

    let tool_version = object
        .get("tool_version")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing tool_version".to_string())?;
    if tool_version != env!("CARGO_PKG_VERSION") {
        return Err(format!(
            "tool_version drifted: expected {}, got {tool_version}",
            env!("CARGO_PKG_VERSION")
        ));
    }

    object.insert(
        "doctor_version".to_string(),
        Value::String("[CARGO_PKG_VERSION]".to_string()),
    );
    object.insert(
        "tool_version".to_string(),
        Value::String("[CARGO_PKG_VERSION]".to_string()),
    );

    let blast_radius = object
        .get_mut("blast_radius")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "missing blast_radius array".to_string())?;
    let expected = [
        (
            path_string(&workspace.path.join(".ee")),
            "[WORKSPACE]/.ee".to_string(),
        ),
        (
            path_string(&workspace.path.join(".doctor")),
            "[WORKSPACE]/.doctor".to_string(),
        ),
        (
            path_string(&workspace.home.join(".local").join("share").join("ee")),
            "[HOME]/.local/share/ee".to_string(),
        ),
    ];
    for entry in blast_radius.iter_mut() {
        let raw = entry
            .as_str()
            .ok_or_else(|| "blast_radius entries must be strings".to_string())?;
        let replacement = expected
            .iter()
            .find_map(|(actual, scrubbed)| (raw == actual).then_some(scrubbed.as_str()))
            .ok_or_else(|| format!("unexpected blast_radius entry: {raw}"))?;
        *entry = Value::String(replacement.to_string());
    }

    let mut scrubbed = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("serialize scrubbed capabilities JSON: {error}"))?;
    scrubbed.push('\n');
    Ok(scrubbed)
}

fn assert_golden(actual: &str) -> TestResult {
    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        fs::write(&path, actual).map_err(|error| format!("write {}: {error}", path.display()))?;
        return Ok(());
    }
    let expected =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "doctor capabilities golden mismatch: {}\n--- expected\n{}\n--- actual\n{}",
        path.display(),
        expected,
        actual
    ))
}

#[test]
fn doctor_capabilities_cli_output_matches_scrubbed_golden() -> TestResult {
    let workspace = E2eWorkspace::create("doctor-capabilities-golden")?;
    let workspace_arg = workspace.as_str()?.to_string();

    let init = run_ee(
        &workspace,
        "init",
        &["--workspace", &workspace_arg, "init", "--json"],
    )?;
    ensure_success(&init, "ee init")?;

    let output = run_ee(
        &workspace,
        "doctor_capabilities",
        &[
            "--workspace",
            &workspace_arg,
            "doctor",
            "--capabilities",
            "--json",
        ],
    )?;
    ensure_success(&output, "ee doctor --capabilities --json")?;

    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse doctor capabilities JSON: {error}"))?;
    let scrubbed = scrub_capabilities(json, &workspace)?;
    workspace.log(
        "golden_compare",
        json!({
            "artifact": "tests/golden/doctor_capabilities_scrubbed.snap",
            "scrubbedBytes": scrubbed.len(),
        }),
    )?;
    assert_golden(&scrubbed)
}

#[test]
fn doctor_run_index_and_gc_plan_cli_outputs_are_read_only_and_ordered() -> TestResult {
    let workspace = E2eWorkspace::create("doctor-runs-index-gc")?;
    let workspace_arg = workspace.as_str()?.to_string();
    let old_state = write_doctor_run_state(
        &workspace,
        "run-old",
        "2000-01-01T00:00:00Z",
        Some("2000-01-01T00:00:01Z"),
        2,
    )?;
    let future_state = write_doctor_run_state(
        &workspace,
        "run-future",
        "2099-01-01T00:00:00Z",
        Some("2099-01-01T00:00:01Z"),
        1,
    )?;

    let list_output = run_ee(
        &workspace,
        "doctor_list_runs",
        &[
            "--workspace",
            &workspace_arg,
            "doctor",
            "--list-runs",
            "--json",
        ],
    )?;
    ensure_success(&list_output, "ee doctor --list-runs --json")?;
    if !list_output.stderr.is_empty() {
        return Err(format!(
            "ee doctor --list-runs --json must keep stderr empty; stderr={}",
            String::from_utf8_lossy(&list_output.stderr)
        ));
    }
    let list_json: Value = serde_json::from_slice(&list_output.stdout)
        .map_err(|error| format!("parse doctor list-runs JSON: {error}"))?;
    if list_json.get("schema").and_then(Value::as_str) != Some("ee.doctor.run_index.v1") {
        return Err(format!("unexpected run index schema: {list_json}"));
    }
    if list_json.get("count").and_then(Value::as_u64) != Some(2) {
        return Err(format!(
            "expected exactly two indexed doctor runs: {list_json}"
        ));
    }
    let runs = list_json
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("run index missing runs[]: {list_json}"))?;
    let run_ids: Vec<&str> = runs
        .iter()
        .filter_map(|run| run.get("runId").and_then(Value::as_str))
        .collect();
    if run_ids != ["run-future", "run-old"] {
        return Err(format!(
            "doctor run index must sort by started_at descending; got {run_ids:?}"
        ));
    }

    let gc_output = run_ee(
        &workspace,
        "doctor_gc_plan",
        &[
            "--workspace",
            &workspace_arg,
            "doctor",
            "--gc-plan",
            "30",
            "--json",
        ],
    )?;
    ensure_success(&gc_output, "ee doctor --gc-plan 30 --json")?;
    if !gc_output.stderr.is_empty() {
        return Err(format!(
            "ee doctor --gc-plan --json must keep stderr empty; stderr={}",
            String::from_utf8_lossy(&gc_output.stderr)
        ));
    }
    let gc_json: Value = serde_json::from_slice(&gc_output.stdout)
        .map_err(|error| format!("parse doctor gc-plan JSON: {error}"))?;
    if gc_json.get("schema").and_then(Value::as_str) != Some("ee.doctor.gc_plan.v1") {
        return Err(format!("unexpected GC plan schema: {gc_json}"));
    }
    if gc_json.get("sideEffectFree") != Some(&Value::Bool(true))
        || gc_json.get("configMutation").and_then(Value::as_str) != Some("never")
    {
        return Err(format!(
            "doctor GC plan must declare read-only posture: {gc_json}"
        ));
    }
    if gc_json.get("eligibleCount").and_then(Value::as_u64) != Some(1) {
        return Err(format!(
            "expected only the old run to be GC-eligible: {gc_json}"
        ));
    }
    let eligible = gc_json
        .get("eligible")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("GC plan missing eligible[]: {gc_json}"))?;
    if eligible
        .first()
        .and_then(|entry| entry.get("runId"))
        .and_then(Value::as_str)
        != Some("run-old")
    {
        return Err(format!("old run should be first eligible entry: {gc_json}"));
    }
    let ineligible = gc_json
        .get("ineligible")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("GC plan missing ineligible[]: {gc_json}"))?;
    if !ineligible.iter().any(|entry| {
        entry
            .get("runId")
            .and_then(Value::as_str)
            .is_some_and(|run_id| run_id == "run-future")
    }) {
        return Err(format!("future run should remain ineligible: {gc_json}"));
    }
    if !old_state.exists() || !future_state.exists() {
        return Err("doctor GC plan must not delete run state files".to_string());
    }
    Ok(())
}
