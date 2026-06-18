use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

type TestResult<T = ()> = Result<T, String>;

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct E2eWorkspace {
    path: PathBuf,
    home: PathBuf,
    xdg_data: PathBuf,
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
        let path = base.join("ee-check-contract-e2e").join(format!(
            "{test_name}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        let home = path.join("home");
        let xdg_data = path.join("xdg-data");
        fs::create_dir_all(&home).map_err(|error| format!("create {}: {error}", home.display()))?;
        fs::create_dir_all(&xdg_data)
            .map_err(|error| format!("create {}: {error}", xdg_data.display()))?;
        let log_path = path.join("check_contract.events.jsonl");
        Ok(Self {
            path,
            home,
            xdg_data,
            log_path,
        })
    }

    fn workspace_arg(&self) -> TestResult<&str> {
        self.path
            .to_str()
            .ok_or_else(|| format!("workspace path is not UTF-8: {}", self.path.display()))
    }

    fn log(&self, phase: &str, payload: Value) -> TestResult {
        let entry = json!({
            "schema": "ee.test_event.v1",
            "suite": "check_contract_e2e",
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

#[test]
fn check_json_contract_reports_uninitialized_workspace_and_field_profiles() -> TestResult {
    let workspace = E2eWorkspace::create("check-uninitialized-contract")?;
    workspace.log(
        "arrange",
        json!({
            "event": "workspace_created_without_ee_db",
            "workspace": path_string(&workspace.path),
            "home": path_string(&workspace.home),
            "xdgData": path_string(&workspace.xdg_data),
            "assertions": [
                "standard_response_envelope",
                "needs_attention_posture",
                "init_recovery_action",
                "full_profile_includes_action_reason",
                "json_stdout_is_machine_clean"
            ],
        }),
    )?;

    let workspace_arg = workspace.workspace_arg()?;
    let standard_output = run_ee(
        &workspace,
        "act_check_standard",
        &["--workspace", workspace_arg, "check", "--json"],
    )?;
    ensure_success(&standard_output, "ee check --json")?;
    ensure_empty_stderr(&standard_output, "ee check --json")?;
    let standard = stdout_json(&standard_output, "ee check --json")?;
    assert_standard_check_payload(&standard)?;
    workspace.log(
        "assert_standard",
        json!({
            "event": "standard_contract_asserted",
            "stdoutBytes": standard_output.stdout.len(),
            "topLevelKeys": sorted_keys(&standard),
            "posture": standard["data"]["posture"],
            "success": standard["success"],
        }),
    )?;

    let full_output = run_ee(
        &workspace,
        "act_check_full",
        &[
            "--workspace",
            workspace_arg,
            "check",
            "--json",
            "--fields",
            "full",
        ],
    )?;
    ensure_success(&full_output, "ee check --json --fields full")?;
    ensure_empty_stderr(&full_output, "ee check --json --fields full")?;
    let full = stdout_json(&full_output, "ee check --json --fields full")?;
    assert_full_check_payload(&full)?;
    workspace.log(
        "assert_full",
        json!({
            "event": "full_profile_contract_asserted",
            "stdoutBytes": full_output.stdout.len(),
            "topLevelKeys": sorted_keys(&full),
            "suggestedActionCount": array_member(object_value(&full, "data")?, "suggestedActions")?.len(),
        }),
    )?;

    workspace.log(
        "complete",
        json!({
            "event": "check_contract_e2e_passed",
            "logPath": path_string(&workspace.log_path),
        }),
    )
}

fn assert_standard_check_payload(value: &Value) -> TestResult {
    assert_envelope(value, false, "standard")?;
    ensure_eq_str(string_value(value, "fields")?, "standard", "fields")?;
    let data = object_value(value, "data")?;
    assert_check_booleans(data)?;
    let actions = array_member(data, "suggestedActions")?;
    ensure_eq_usize(actions.len(), 1, "suggestedActions.len")?;
    let action = object_at(actions, 0, "suggestedActions")?;
    ensure_eq_u64(
        u64_member(action, "priority")?,
        1,
        "suggestedActions[0].priority",
    )?;
    ensure_eq_str(
        string_member(action, "command")?,
        "ee init --workspace .",
        "suggestedActions[0].command",
    )?;
    ensure_absent(action, "reason", "standard suggested action reason")?;
    Ok(())
}

fn assert_full_check_payload(value: &Value) -> TestResult {
    assert_envelope(value, false, "full")?;
    ensure_eq_str(string_value(value, "fields")?, "full", "fields")?;
    let data = object_value(value, "data")?;
    assert_check_booleans(data)?;
    let actions = array_member(data, "suggestedActions")?;
    ensure_eq_usize(actions.len(), 1, "suggestedActions.len")?;
    let action = object_at(actions, 0, "suggestedActions")?;
    ensure_eq_u64(
        u64_member(action, "priority")?,
        1,
        "suggestedActions[0].priority",
    )?;
    ensure_eq_str(
        string_member(action, "command")?,
        "ee init --workspace .",
        "suggestedActions[0].command",
    )?;
    ensure_non_empty(
        string_member(action, "reason")?,
        "suggestedActions[0].reason",
    )
}

fn assert_envelope(value: &Value, expected_success: bool, expected_fields: &str) -> TestResult {
    ensure_eq_str(string_value(value, "schema")?, "ee.response.v2", "schema")?;
    ensure_eq_bool(bool_value(value, "success")?, expected_success, "success")?;
    ensure_eq_str(string_value(value, "fields")?, expected_fields, "fields")?;
    object_value(value, "data")?;
    Ok(())
}

fn assert_check_booleans(data: &Map<String, Value>) -> TestResult {
    ensure_eq_str(string_member(data, "command")?, "check", "data.command")?;
    ensure_non_empty(string_member(data, "version")?, "data.version")?;
    ensure_eq_str(
        string_member(data, "posture")?,
        "needs_attention",
        "data.posture",
    )?;
    ensure_eq_bool(
        bool_member(data, "workspaceInitialized")?,
        false,
        "data.workspaceInitialized",
    )?;
    ensure_eq_bool(
        bool_member(data, "databaseReady")?,
        false,
        "data.databaseReady",
    )?;
    ensure_eq_bool(bool_member(data, "searchReady")?, false, "data.searchReady")?;
    ensure_eq_bool(
        bool_member(data, "runtimeReady")?,
        true,
        "data.runtimeReady",
    )
}

fn run_ee(workspace: &E2eWorkspace, phase: &str, args: &[&str]) -> TestResult<Output> {
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
        .env("XDG_DATA_HOME", &workspace.xdg_data)
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

fn ensure_empty_stderr(output: &Output, context: &str) -> TestResult {
    if output.stderr.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{context} wrote unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn stdout_json(output: &Output, context: &str) -> TestResult<Value> {
    if output.stdout.is_empty() {
        return Err(format!("{context} produced empty stdout"));
    }
    if output.stdout.contains(&0x1b) {
        return Err(format!("{context} stdout contains ANSI escape bytes"));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{context} stdout is not valid JSON: {error}"))
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn object_value<'a>(value: &'a Value, field: &str) -> TestResult<&'a Map<String, Value>> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("missing object field {field}"))
}

fn string_value<'a>(value: &'a Value, field: &str) -> TestResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {field}"))
}

fn bool_value(value: &Value, field: &str) -> TestResult<bool> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing bool field {field}"))
}

fn object_at<'a>(
    items: &'a [Value],
    index: usize,
    array_name: &str,
) -> TestResult<&'a Map<String, Value>> {
    items
        .get(index)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{array_name}[{index}] is not an object"))
}

fn array_member<'a>(object: &'a Map<String, Value>, field: &str) -> TestResult<&'a Vec<Value>> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing array member {field}"))
}

fn string_member<'a>(object: &'a Map<String, Value>, field: &str) -> TestResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string member {field}"))
}

fn bool_member(object: &Map<String, Value>, field: &str) -> TestResult<bool> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing bool member {field}"))
}

fn u64_member(object: &Map<String, Value>, field: &str) -> TestResult<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing unsigned integer member {field}"))
}

fn ensure_absent(object: &Map<String, Value>, field: &str, context: &str) -> TestResult {
    if !object.contains_key(field) {
        return Ok(());
    }
    Err(format!("{context}: unexpected field {field} was present"))
}

fn ensure_eq_str(actual: &str, expected: &str, field: &str) -> TestResult {
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "{field} mismatch: expected {expected}, got {actual}"
    ))
}

fn ensure_eq_bool(actual: bool, expected: bool, field: &str) -> TestResult {
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "{field} mismatch: expected {expected}, got {actual}"
    ))
}

fn ensure_eq_u64(actual: u64, expected: u64, field: &str) -> TestResult {
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "{field} mismatch: expected {expected}, got {actual}"
    ))
}

fn ensure_eq_usize(actual: usize, expected: usize, field: &str) -> TestResult {
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "{field} mismatch: expected {expected}, got {actual}"
    ))
}

fn ensure_non_empty(value: &str, field: &str) -> TestResult {
    if !value.is_empty() {
        return Ok(());
    }
    Err(format!("{field} must not be empty"))
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}
