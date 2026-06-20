use std::collections::BTreeSet;
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
        let path = base.join("ee-capabilities-contract-e2e").join(format!(
            "{test_name}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        let home = path.join("home");
        let xdg_data = path.join("xdg-data");
        fs::create_dir_all(&home).map_err(|error| format!("create {}: {error}", home.display()))?;
        fs::create_dir_all(&xdg_data)
            .map_err(|error| format!("create {}: {error}", xdg_data.display()))?;
        let log_path = path.join("capabilities_contract.events.jsonl");
        Ok(Self {
            path,
            home,
            xdg_data,
            log_path,
        })
    }

    fn log(&self, phase: &str, payload: Value) -> TestResult {
        let entry = json!({
            "schema": "ee.test_event.v1",
            "suite": "capabilities_contract_e2e",
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
fn capabilities_json_contract_reports_inventory_counts_and_registry() -> TestResult {
    let workspace = E2eWorkspace::create("capabilities-json-contract")?;
    workspace.log(
        "arrange",
        json!({
            "event": "workspace_created",
            "workspace": path_string(&workspace.path),
            "home": path_string(&workspace.home),
            "xdgData": path_string(&workspace.xdg_data),
            "assertions": [
                "response_envelope",
                "summary_counts_match_arrays",
                "core_commands_are_available",
                "env_registry_is_machine_readable",
                "json_outputs_are_advertised"
            ],
        }),
    )?;

    let output = run_ee(
        &workspace,
        "act_capabilities_json",
        &["capabilities", "--json"],
    )?;
    ensure_success(&output, "ee capabilities --json")?;
    ensure_empty_stderr(&output, "ee capabilities --json")?;
    let json = stdout_json(&output, "ee capabilities --json")?;
    workspace.log(
        "assert_envelope",
        json!({
            "event": "json_parsed",
            "topLevelKeys": sorted_keys(&json),
            "stdoutBytes": output.stdout.len(),
        }),
    )?;

    ensure_eq_str(string_value(&json, "schema")?, "ee.response.v2", "schema")?;
    ensure_eq_bool(bool_value(&json, "success")?, true, "success")?;
    let data = object_value(&json, "data")?;
    ensure_eq_str(
        string_member(data, "command")?,
        "capabilities",
        "data.command",
    )?;
    ensure_non_empty(string_member(data, "version")?, "data.version")?;

    let subsystems = array_member(data, "subsystems")?;
    let features = array_member(data, "features")?;
    let unimplemented = array_member(data, "unimplemented")?;
    let commands = array_member(data, "commands")?;
    let env_overrides = array_member(data, "envOverrides")?;
    let summary = object_member(data, "summary")?;

    assert_unique_string_field(subsystems, "subsystems", "name")?;
    assert_unique_string_field(features, "features", "name")?;
    assert_unique_string_field(unimplemented, "unimplemented", "code")?;
    assert_unique_string_field(commands, "commands", "name")?;
    assert_unique_string_field(env_overrides, "envOverrides", "name")?;

    assert_summary_count(
        summary,
        "readySubsystems",
        count_status(subsystems, "ready")?,
    )?;
    assert_summary_count(summary, "totalSubsystems", subsystems.len())?;
    assert_summary_count(
        summary,
        "enabledFeatures",
        count_bool_field(features, "enabled")?,
    )?;
    assert_summary_count(summary, "totalFeatures", features.len())?;
    assert_summary_count(summary, "unimplementedCapabilities", unimplemented.len())?;
    assert_summary_count(
        summary,
        "availableCommands",
        count_bool_field(commands, "available")?,
    )?;
    assert_summary_count(summary, "totalCommands", commands.len())?;
    workspace.log(
        "assert_summary_counts",
        json!({
            "event": "summary_counts_match_arrays",
            "subsystems": subsystems.len(),
            "features": features.len(),
            "unimplemented": unimplemented.len(),
            "commands": commands.len(),
        }),
    )?;

    for command in [
        "capabilities",
        "doctor",
        "schema",
        "remember",
        "search",
        "pack",
    ] {
        let entry = find_object_by_string(commands, "commands", "name", command)?;
        ensure_eq_bool(
            bool_member(entry, "available")?,
            true,
            &format!("commands[{command}].available"),
        )?;
    }
    workspace.log(
        "assert_command_inventory",
        json!({
            "event": "core_commands_available",
            "required": ["capabilities", "doctor", "schema", "remember", "search", "pack"],
        }),
    )?;

    for env_name in [
        "EE_AGENT_NAME",
        "EE_DATABASE_PATH",
        "EE_INDEX_DIR",
        "EE_NO_COLOR",
        "EE_TEST_LOG_PATH",
    ] {
        let entry = find_object_by_string(env_overrides, "envOverrides", "name", env_name)?;
        ensure_non_empty(
            string_member(entry, "category")?,
            &format!("envOverrides[{env_name}].category"),
        )?;
        ensure_non_empty(
            string_member(entry, "controls")?,
            &format!("envOverrides[{env_name}].controls"),
        )?;
        ensure_non_empty(
            string_member(entry, "source")?,
            &format!("envOverrides[{env_name}].source"),
        )?;
    }
    let no_color = find_object_by_string(env_overrides, "envOverrides", "name", "EE_NO_COLOR")?;
    ensure_eq_bool(
        bool_member(no_color, "isSet")?,
        true,
        "envOverrides[EE_NO_COLOR].isSet",
    )?;
    ensure_eq_str(
        string_member(no_color, "source")?,
        "process_env",
        "envOverrides[EE_NO_COLOR].source",
    )?;
    workspace.log(
        "assert_env_registry",
        json!({
            "event": "env_registry_contract_asserted",
            "envOverrideCount": env_overrides.len(),
            "eeNoColorSource": string_member(no_color, "source")?,
        }),
    )?;

    let output_metadata = object_member(data, "output")?;
    let formats = array_member(output_metadata, "formats")?;
    assert_unique_string_field(formats, "output.formats", "name")?;
    for format in ["json", "jsonl"] {
        let entry = find_object_by_string(formats, "output.formats", "name", format)?;
        ensure_eq_bool(
            bool_member(entry, "available")?,
            true,
            &format!("output.formats[{format}].available"),
        )?;
        ensure_eq_bool(
            bool_member(entry, "machineReadable")?,
            true,
            &format!("output.formats[{format}].machineReadable"),
        )?;
    }
    let cass = object_member(object_member(data, "binaries")?, "cass")?;
    ensure_non_empty(string_member(cass, "source")?, "binaries.cass.source")?;
    bool_member(cass, "trusted")?;
    object_member(data, "index")?;
    workspace.log(
        "assert_output_and_index_metadata",
        json!({
            "event": "output_contract_asserted",
            "formatCount": formats.len(),
            "jsonFormats": ["json", "jsonl"],
            "cassSource": string_member(cass, "source")?,
        }),
    )?;

    workspace.log(
        "complete",
        json!({
            "event": "capabilities_contract_e2e_passed",
            "logPath": path_string(&workspace.log_path),
        }),
    )
}

#[cfg(unix)]
#[test]
fn capabilities_json_reports_non_utf8_secret_env_as_set_without_value() -> TestResult {
    use std::os::unix::ffi::OsStringExt;

    let workspace = E2eWorkspace::create("capabilities-non-utf8-secret-env")?;
    let secret = std::ffi::OsString::from_vec(vec![b'a', 0x80, b'b']);
    let output = run_ee_with_extra_env(
        &workspace,
        "act_capabilities_non_utf8_secret_env",
        &["capabilities", "--json"],
        &[("EE_SERVE_TOKEN", secret.as_os_str())],
    )?;
    ensure_success(
        &output,
        "ee capabilities --json with non-UTF8 EE_SERVE_TOKEN",
    )?;
    ensure_empty_stderr(
        &output,
        "ee capabilities --json with non-UTF8 EE_SERVE_TOKEN",
    )?;

    let json = stdout_json(
        &output,
        "ee capabilities --json with non-UTF8 EE_SERVE_TOKEN",
    )?;
    let data = object_value(&json, "data")?;
    let env_overrides = array_member(data, "envOverrides")?;
    let serve_token =
        find_object_by_string(env_overrides, "envOverrides", "name", "EE_SERVE_TOKEN")?;
    ensure_eq_bool(
        bool_member(serve_token, "isSet")?,
        true,
        "envOverrides[EE_SERVE_TOKEN].isSet",
    )?;
    ensure_eq_str(
        string_member(serve_token, "source")?,
        "process_env",
        "envOverrides[EE_SERVE_TOKEN].source",
    )?;
    if serve_token.contains_key("currentValue") {
        return Err("envOverrides[EE_SERVE_TOKEN] must not expose currentValue".to_owned());
    }

    Ok(())
}

fn run_ee(workspace: &E2eWorkspace, phase: &str, args: &[&str]) -> TestResult<Output> {
    run_ee_with_extra_env(workspace, phase, args, &[])
}

fn run_ee_with_extra_env(
    workspace: &E2eWorkspace,
    phase: &str,
    args: &[&str],
    extra_env: &[(&str, &std::ffi::OsStr)],
) -> TestResult<Output> {
    workspace.log(
        phase,
        json!({
            "event": "command_start",
            "argv": args,
        }),
    )?;
    let started = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(args)
        .env("HOME", &workspace.home)
        .env("XDG_DATA_HOME", &workspace.xdg_data)
        .env("EE_NO_COLOR", "1");
    for &(name, value) in extra_env {
        command.env(name, value);
    }
    let output = command
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

fn object_member<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> TestResult<&'a Map<String, Value>> {
    object
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("missing object member {field}"))
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

fn bool_value(value: &Value, field: &str) -> TestResult<bool> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing bool field {field}"))
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

fn find_object_by_string<'a>(
    items: &'a [Value],
    array_name: &str,
    field: &str,
    expected: &str,
) -> TestResult<&'a Map<String, Value>> {
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("{array_name}[{index}] is not an object"))?;
        if string_member(object, field)? == expected {
            return Ok(object);
        }
    }
    Err(format!(
        "{array_name} missing entry with {field}={expected}"
    ))
}

fn assert_unique_string_field(items: &[Value], array_name: &str, field: &str) -> TestResult {
    let mut seen = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("{array_name}[{index}] is not an object"))?;
        let value = string_member(object, field)?;
        if !seen.insert(value.to_string()) {
            return Err(format!("{array_name} has duplicate {field} value {value}"));
        }
    }
    Ok(())
}

fn count_status(items: &[Value], expected: &str) -> TestResult<usize> {
    let mut count = 0;
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("subsystems[{index}] is not an object"))?;
        if string_member(object, "status")? == expected {
            count += 1;
        }
    }
    Ok(count)
}

fn count_bool_field(items: &[Value], field: &str) -> TestResult<usize> {
    let mut count = 0;
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("array item {index} is not an object"))?;
        if bool_member(object, field)? {
            count += 1;
        }
    }
    Ok(count)
}

fn assert_summary_count(summary: &Map<String, Value>, field: &str, expected: usize) -> TestResult {
    let actual = u64_member(summary, field)?;
    if actual == expected as u64 {
        return Ok(());
    }
    Err(format!(
        "summary.{field} mismatch: expected {expected}, got {actual}"
    ))
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

fn ensure_non_empty(value: &str, field: &str) -> TestResult {
    if !value.is_empty() {
        return Ok(());
    }
    Err(format!("{field} must not be empty"))
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}
