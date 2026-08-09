//! Real-binary e2e pin for bundled embedding model registration.

use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize};
#[cfg(unix)]
use std::thread::{self, JoinHandle};
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
            .get("active")
            .and_then(|v| v.pointer("/source"))
            .and_then(Value::as_str)
            .ok_or_else(|| "missing active.source".to_string())?,
        "frankensearch_hash_fallback",
        "status active.source before artifact download",
    )?;
    ensure_eq_bool(
        status_data
            .get("active")
            .and_then(|v| v.pointer("/semantic"))
            .and_then(Value::as_bool)
            .ok_or_else(|| "missing active.semantic".to_string())?,
        false,
        "status active.semantic before artifact download",
    )?;
    let lifecycle_models = status_data
        .get("modelLifecycle")
        .and_then(|v| v.pointer("/models"))
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
            .get("embeddingMetadata")
            .and_then(|v| v.pointer("/dimension"))
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
    let entry_id = string_member(entry, "id")?.to_owned();
    ensure_eq_usize(
        count_objects_by_string(entries, "modelName", BUNDLED_EMBEDDING_MODEL_ID)?,
        1,
        "bundled model list duplicate count",
    )?;
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

    let status_again = run_ee(
        &workspace,
        "act_model_status_again",
        &[
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
            "model",
            "status",
        ],
    )?;
    ensure_success(&status_again, "second ee model status")?;
    ensure_empty_stderr(&status_again, "second ee model status")?;
    let status_again_json = stdout_json(&status_again, "second ee model status")?;
    let status_again_data = response_data(&status_again_json, "second ee model status")?;
    ensure_eq_u64(
        u64_member(status_again_data, "availableCount")?,
        0,
        "second status availableCount before artifact download",
    )?;

    let list_again = run_ee(
        &workspace,
        "act_model_list_again",
        &[
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
            "model",
            "list",
        ],
    )?;
    ensure_success(&list_again, "second ee model list")?;
    ensure_empty_stderr(&list_again, "second ee model list")?;
    let list_again_json = stdout_json(&list_again, "second ee model list")?;
    let list_again_data = response_data(&list_again_json, "second ee model list")?;
    let entries_again = list_again_data
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing second model list entries".to_string())?;
    ensure_eq_usize(
        count_objects_by_string(entries_again, "modelName", BUNDLED_EMBEDDING_MODEL_ID)?,
        1,
        "second bundled model list duplicate count",
    )?;
    let entry_again = find_object_by_string(
        entries_again,
        "entries_again",
        "modelName",
        BUNDLED_EMBEDDING_MODEL_ID,
    )?;
    let entry_again_id = string_member(entry_again, "id")?;
    workspace.log(
        "assert_idempotent_registry_row",
        json!({
            "event": "idempotent_model_list_observed",
            "firstEntryId": entry_id.as_str(),
            "secondEntryId": entry_again_id,
            "secondEntryCount": entries_again.len(),
        }),
    )?;
    ensure_eq_str(
        entry_again_id,
        entry_id.as_str(),
        "bundled row id stable across repeated status/list",
    )?;
    ensure_eq_str(
        string_member(entry_again, "status")?,
        "unavailable",
        "second entry status before artifact download",
    )?;
    ensure_eq_u64(
        u64_member(entry_again, "dimension")?,
        u64::from(BUNDLED_EMBEDDING_DIMENSION),
        "second entry dimension",
    )?;
    workspace.log(
        "complete",
        json!({
            "event": "model_bundled_embedding_cli_e2e_passed",
            "logPath": path_string(&workspace.log_path),
        }),
    )
}

#[cfg(unix)]
#[test]
#[ignore = "requires the real potion-multilingual-128M fixture"]
fn registered_model2vec_fixture_is_neural_without_overrides_or_network() -> TestResult {
    let fixture_root = std::env::var_os("EE_EMBED_MODEL_FIXTURE_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "EE_EMBED_MODEL_FIXTURE_DIR must name the real model fixture".to_string())?;
    let fixture_model_dir = resolve_fixture_model_dir(&fixture_root)?;
    let workspace = E2eWorkspace::create("registered-model2vec-offline")?;
    let registered_parent = workspace
        .xdg_data
        .join("ee")
        .join("models")
        .join("model2vec");
    fs::create_dir_all(&registered_parent)
        .map_err(|error| format!("create {}: {error}", registered_parent.display()))?;
    let registered_model_dir = registered_parent.join("potion-multilingual-128M");
    std::os::unix::fs::symlink(&fixture_model_dir, &registered_model_dir).map_err(|error| {
        format!(
            "link real model fixture {} at {}: {error}",
            fixture_model_dir.display(),
            registered_model_dir.display()
        )
    })?;

    let network_trap = NetworkTrap::start()?;
    let proxy = network_trap.proxy_url();
    let proxy_env = network_proxy_env(&proxy);

    let init = run_ee_with_env(
        &workspace,
        "registered_init",
        &["init", "--workspace", workspace.workspace_arg()?, "--json"],
        &proxy_env,
    )?;
    ensure_success(&init, "registered ee init")?;

    let remember = run_ee_with_env(
        &workspace,
        "registered_remember",
        &[
            "remember",
            "Verified local semantic models must remain usable when downloads are disabled.",
            "--workspace",
            workspace.workspace_arg()?,
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--no-auto-link",
            "--no-propose-candidates",
            "--json",
        ],
        &proxy_env,
    )?;
    ensure_success(&remember, "registered ee remember")?;

    let rebuild = run_ee_with_env(
        &workspace,
        "registered_rebuild",
        &[
            "index",
            "rebuild",
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
        ],
        &proxy_env,
    )?;
    ensure_success(&rebuild, "registered ee index rebuild")?;
    let rebuild_json = stdout_json(&rebuild, "registered ee index rebuild")?;
    let rebuild_data = response_data(&rebuild_json, "registered ee index rebuild")?;
    ensure_eq_bool(
        rebuild_data
            .get("embedding")
            .and_then(Value::as_object)
            .and_then(|embedding| embedding.get("semantic"))
            .and_then(Value::as_bool)
            .ok_or_else(|| "missing rebuild embedding.semantic".to_string())?,
        true,
        "registered rebuild semantic",
    )?;

    let query = "local semantic model download policy";
    let search = run_ee_with_env(
        &workspace,
        "registered_search",
        &[
            "search",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--relevance-floor",
            "0",
            "--json",
        ],
        &proxy_env,
    )?;
    ensure_success(&search, "registered ee search")?;
    ensure_response_embed_backend(&search, "registered ee search", "neural_local")?;

    let pack = run_ee_with_env(
        &workspace,
        "registered_pack",
        &[
            "pack",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--max-tokens",
            "800",
            "--json",
        ],
        &proxy_env,
    )?;
    ensure_success(&pack, "registered ee pack")?;
    ensure_response_embed_backend(&pack, "registered ee pack", "neural_local")?;

    let orient = run_ee_with_env(
        &workspace,
        "registered_orient",
        &[
            "orient",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--fast",
            "--json",
        ],
        &proxy_env,
    )?;
    ensure_success(&orient, "registered ee orient")?;
    ensure_response_embed_backend(&orient, "registered ee orient", "neural_local")?;

    let mut download_off_env = proxy_env.clone();
    download_off_env.push(("EE_EMBED_DOWNLOAD".to_string(), "off".to_string()));
    let search_download_off = run_ee_with_env(
        &workspace,
        "registered_search_download_off",
        &[
            "search",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--relevance-floor",
            "0",
            "--json",
        ],
        &download_off_env,
    )?;
    ensure_success(
        &search_download_off,
        "registered ee search with downloads off",
    )?;
    ensure_response_embed_backend(
        &search_download_off,
        "registered ee search with downloads off",
        "neural_local",
    )?;

    for (name, output) in [
        ("init", &init),
        ("remember", &remember),
        ("rebuild", &rebuild),
        ("search", &search),
        ("pack", &pack),
        ("orient", &orient),
        ("search_download_off", &search_download_off),
    ] {
        ensure_text_absent(
            &output.stderr,
            "downloading the local embedding model",
            &format!("{name} download notice"),
        )?;
    }

    ensure_eq_usize(
        network_trap.finish()?,
        0,
        "registered fixture network connection attempts",
    )
}

#[cfg(unix)]
fn resolve_fixture_model_dir(root: &Path) -> TestResult<PathBuf> {
    for candidate in [
        root.to_path_buf(),
        root.join("potion-multilingual-128M"),
        root.join("model2vec").join("potion-multilingual-128M"),
        root.join("models")
            .join("model2vec")
            .join("potion-multilingual-128M"),
    ] {
        if candidate.join("model.safetensors").is_file()
            && candidate.join("tokenizer.json").is_file()
        {
            return Ok(candidate);
        }
    }
    Err(format!(
        "{} does not contain a potion-multilingual-128M fixture",
        root.display()
    ))
}

#[cfg(unix)]
struct NetworkTrap {
    address: std::net::SocketAddr,
    attempts: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl NetworkTrap {
    fn start() -> TestResult<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("bind network trap: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("make network trap nonblocking: {error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("read network trap address: {error}"))?;
        let attempts = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_attempts = Arc::clone(&attempts);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((_stream, _peer)) => {
                        thread_attempts.fetch_add(1, Ordering::AcqRel);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            address,
            attempts,
            stop,
            thread: Some(thread),
        })
    }

    fn proxy_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn finish(mut self) -> TestResult<usize> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "network trap thread panicked".to_string())?;
        }
        Ok(self.attempts.load(Ordering::Acquire))
    }
}

#[cfg(unix)]
impl Drop for NetworkTrap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
fn network_proxy_env(proxy: &str) -> Vec<(String, String)> {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .map(|name| (name.to_string(), proxy.to_string()))
    .collect()
}

fn run_ee(workspace: &E2eWorkspace, phase: &str, args: &[&str]) -> TestResult<Output> {
    run_ee_with_env(workspace, phase, args, &[])
}

fn run_ee_with_env(
    workspace: &E2eWorkspace,
    phase: &str,
    args: &[&str],
    env: &[(String, String)],
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
        .env("EE_NO_COLOR", "1")
        .env_remove("EE_EMBED_MODEL_DIR")
        .env_remove("EE_EMBED_MODEL_PATH")
        .env_remove("EE_EMBED_DOWNLOAD")
        .env_remove("FRANKENSEARCH_MODEL_DIR")
        .env_remove("FRANKENSEARCH_OFFLINE")
        .env_remove("FRANKENSEARCH_ALLOW_DOWNLOAD")
        .env_remove("FRANKENSEARCH_API_PROVIDER")
        .env_remove("FRANKENSEARCH_API_MODEL")
        .env_remove("FRANKENSEARCH_API_DIMENSION")
        .env_remove("FRANKENSEARCH_API_IDENTITY_JSON")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY");
    for (name, value) in env {
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

#[cfg(unix)]
fn ensure_response_embed_backend(output: &Output, context: &str, expected: &str) -> TestResult {
    let value = stdout_json(output, context)?;
    let data = response_data(&value, context)?;
    ensure_eq_str(string_member(data, "embed_backend")?, expected, context)
}

#[cfg(unix)]
fn ensure_text_absent(bytes: &[u8], needle: &str, context: &str) -> TestResult {
    let text = String::from_utf8_lossy(bytes);
    if !text.contains(needle) {
        return Ok(());
    }
    Err(format!("{context} unexpectedly contained {needle}"))
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

fn count_objects_by_string(items: &[Value], field: &str, expected: &str) -> TestResult<usize> {
    let mut count = 0;
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("entries[{index}] is not an object"))?;
        if string_member(object, field)? == expected {
            count += 1;
        }
    }
    Ok(count)
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
