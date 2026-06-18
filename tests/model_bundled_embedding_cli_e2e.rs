//! Real-binary e2e pin for bundled embedding model registration.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::core::model::{BUNDLED_EMBEDDING_DIMENSION, BUNDLED_EMBEDDING_MODEL_ID};
use ee::models::{MODEL_LIST_SCHEMA_V1, MODEL_STATUS_SCHEMA_V2, RESPONSE_SCHEMA_V2};
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
        let path = base.join("ee-model-bundled-cli-e2e").join(format!(
            "{test_name}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        let home = path.join("home");
        let xdg_data = path.join("xdg-data");
        fs::create_dir_all(&home).map_err(|error| format!("create {}: {error}", home.display()))?;
        fs::create_dir_all(&xdg_data)
            .map_err(|error| format!("create {}: {error}", xdg_data.display()))?;
        Ok(Self {
            log_path: path.join("model_bundled_embedding.events.jsonl"),
            path,
            home,
            xdg_data,
        })
    }

    fn workspace_arg(&self) -> TestResult<&str> {
        self.path
            .to_str()
            .ok_or_else(|| "workspace path must be valid UTF-8".to_string())
    }

    fn log(&self, phase: &str, payload: Value) -> TestResult {
        let entry = json!({
            "schema": "ee.test_event.v1",
            "suite": "model_bundled_embedding_cli_e2e",
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
fn model_cli_auto_declares_bundled_embedding_without_claiming_download() -> TestResult {
    let workspace = E2eWorkspace::create("auto-declare-bundled-embedding")?;
    workspace.log(
        "arrange",
        json!({
            "event": "workspace_created",
            "workspace": path_string(&workspace.path),
            "home": path_string(&workspace.home),
            "xdgData": path_string(&workspace.xdg_data),
            "expectedBundledModel": BUNDLED_EMBEDDING_MODEL_ID,
            "expectedDimension": BUNDLED_EMBEDDING_DIMENSION,
        }),
    )?;

    let init = run_ee(
        &workspace,
        "act_init",
        &["init", "--workspace", workspace.workspace_arg()?, "--json"],
    )?;
    ensure_success(&init, "ee init")?;
    ensure_empty_stderr(&init, "ee init")?;

    let status = run_ee(
        &workspace,
        "act_model_status",
        &[
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
            "model",
            "status",
        ],
    )?;
    ensure_success(&status, "ee model status")?;
    ensure_empty_stderr(&status, "ee model status")?;
    let status_json = stdout_json(&status, "ee model status")?;
    let status_data = response_data(&status_json, "ee model status")?;
    workspace.log(
        "assert_model_status",
        json!({
            "event": "model_status_observed",
            "registeredCount": status_data.get("registeredCount"),
            "availableCount": status_data.get("availableCount"),
            "activeSource": status_data.get("active").and_then(|v| v.pointer("/source")),
            "semanticReadiness": status_data.get("modelLifecycle").and_then(|v| v.pointer("/semanticReadiness/state")),
        }),
    )?;

    ensure_eq_str(
        string_member(status_data, "schema")?,
        MODEL_STATUS_SCHEMA_V2,
        "status data schema",
    )?;
    ensure_u64_at_least(
        u64_member(status_data, "registeredCount")?,
        1,
        "status registeredCount",
    )?;
    ensure_eq_u64(
        u64_member(status_data, "availableCount")?,
        0,
        "status availableCount before artifact download",
    )?;
    ensure_eq_str(
        status_data
            .get("active").and_then(|v| v.pointer("/source"))
            .and_then(Value::as_str)
            .ok_or_else(|| "missing active.source".to_string())?,
        "frankensearch_hash_fallback",
        "status active.source before artifact download",
    )?;
    ensure_eq_bool(
        status_data
            .get("active").and_then(|v| v.pointer("/semantic"))
            .and_then(Value::as_bool)
            .ok_or_else(|| "missing active.semantic".to_string())?,
        false,
        "status active.semantic before artifact download",
    )?;
    let lifecycle_models = status_data
        .get("modelLifecycle").and_then(|v| v.pointer("/models"))
        .and_then(Value::as_array)
        .ok_or_else(|| "missing modelLifecycle.models".to_string())?;
    let lifecycle_entry = find_object_by_string(
        lifecycle_models,
        "modelLifecycle.models",
        "modelId",
        BUNDLED_EMBEDDING_MODEL_ID,
    )?;
    ensure_eq_str(
        string_member(lifecycle_entry, "provider")?,
        "model2vec",
        "lifecycle provider",
    )?;
    ensure_eq_str(
        string_member(lifecycle_entry, "registryStatus")?,
        "unavailable",
        "bundled registryStatus before artifact download",
    )?;
    ensure_eq_u64(
        lifecycle_entry
            .get("embeddingMetadata").and_then(|v| v.pointer("/dimension"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "missing embeddingMetadata.dimension".to_string())?,
        u64::from(BUNDLED_EMBEDDING_DIMENSION),
        "bundled embedding dimension",
    )?;

    let list = run_ee(
        &workspace,
        "act_model_list",
        &[
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
            "model",
            "list",
        ],
    )?;
    ensure_success(&list, "ee model list")?;
    ensure_empty_stderr(&list, "ee model list")?;
    let list_json = stdout_json(&list, "ee model list")?;
    let list_data = response_data(&list_json, "ee model list")?;
    workspace.log(
        "assert_model_list",
        json!({
            "event": "model_list_observed",
            "entryCount": list_data.get("entries").and_then(Value::as_array).map(Vec::len),
            "degradationCount": list_data.get("degradations").and_then(Value::as_array).map(Vec::len),
        }),
    )?;

    ensure_eq_str(
        string_member(list_data, "schema")?,
        MODEL_LIST_SCHEMA_V1,
        "list data schema",
    )?;
    let entries = list_data
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing model list entries".to_string())?;
    let entry = find_object_by_string(entries, "entries", "modelName", BUNDLED_EMBEDDING_MODEL_ID)?;
    ensure_eq_str(
        string_member(entry, "provider")?,
        "model2vec",
        "entry provider",
    )?;
    ensure_eq_str(
        string_member(entry, "purpose")?,
        "embedding",
        "entry purpose",
    )?;
    ensure_eq_str(
        string_member(entry, "status")?,
        "unavailable",
        "entry status before artifact download",
    )?;
    ensure_eq_u64(
        u64_member(entry, "dimension")?,
        u64::from(BUNDLED_EMBEDDING_DIMENSION),
        "entry dimension",
    )?;
    workspace.log(
        "complete",
        json!({
            "event": "model_bundled_embedding_cli_e2e_passed",
            "logPath": path_string(&workspace.log_path),
        }),
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

fn response_data<'a>(value: &'a Value, context: &str) -> TestResult<&'a Map<String, Value>> {
    ensure_eq_str(string_value(value, "schema")?, RESPONSE_SCHEMA_V2, context)?;
    ensure_eq_bool(bool_value(value, "success")?, true, context)?;
    value
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{context} missing data object"))
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

fn string_value<'a>(value: &'a Value, field: &str) -> TestResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {field}"))
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

fn ensure_u64_at_least(actual: u64, floor: u64, field: &str) -> TestResult {
    if actual >= floor {
        return Ok(());
    }
    Err(format!(
        "{field} mismatch: expected at least {floor}, got {actual}"
    ))
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}
