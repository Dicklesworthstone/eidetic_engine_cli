//! Real-binary e2e pin for bundled embedding model registration.

use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::core::model::{BUNDLED_EMBEDDING_DIMENSION, BUNDLED_EMBEDDING_MODEL_ID};
#[cfg(unix)]
use ee::db::{CreateModelRegistryInput, DbConnection, StoredModelRegistryEntry};
use ee::models::{MODEL_LIST_SCHEMA_V1, MODEL_STATUS_SCHEMA_V2, RESPONSE_SCHEMA_V2};
#[cfg(unix)]
use ee::models::{ModelProvider, ModelPurpose, ModelRegistryStatus};
#[cfg(unix)]
use frankensearch::embed::{ModelManifest, verify_dir_cached};
use serde_json::{Map, Value, json};

type TestResult<T = ()> = Result<T, String>;

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
struct NetworkTripwire {
    address: SocketAddr,
    proxy_url: String,
    connection_count: Arc<AtomicUsize>,
    accept_failed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl NetworkTripwire {
    fn start() -> TestResult<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("bind network tripwire: {error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("inspect network tripwire address: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("set network tripwire nonblocking: {error}"))?;

        let connection_count = Arc::new(AtomicUsize::new(0));
        let accept_failed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_connections = Arc::clone(&connection_count);
        let thread_failed = Arc::clone(&accept_failed);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("ee-embedding-network-tripwire".to_string())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            thread_connections.fetch_add(1, Ordering::AcqRel);
                            drop(stream);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => {
                            thread_failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            })
            .map_err(|error| format!("spawn network tripwire: {error}"))?;

        Ok(Self {
            address,
            proxy_url: format!("http://{address}"),
            connection_count,
            accept_failed,
            stop,
            thread: Some(thread),
        })
    }

    fn proxy_env(&self) -> Vec<(String, String)> {
        let mut env = [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ]
        .into_iter()
        .map(|name| (name.to_string(), self.proxy_url.clone()))
        .collect::<Vec<_>>();
        env.push(("NO_PROXY".to_string(), String::new()));
        env.push(("no_proxy".to_string(), String::new()));
        env
    }

    fn assert_unused(&self) -> TestResult {
        thread::sleep(std::time::Duration::from_millis(20));
        if self.accept_failed.load(Ordering::Acquire) {
            return Err("network tripwire accept loop failed".to_string());
        }
        let connection_count = self.connection_count.load(Ordering::Acquire);
        if connection_count == 0 {
            return Ok(());
        }
        Err(format!(
            "registered local-model commands attempted {connection_count} proxied network connection(s)"
        ))
    }
}

#[cfg(unix)]
impl Drop for NetworkTripwire {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
#[test]
fn network_tripwire_detects_a_proxied_connection() -> TestResult {
    let network_tripwire = NetworkTripwire::start()?;
    let stream = TcpStream::connect(network_tripwire.address)
        .map_err(|error| format!("connect to network tripwire: {error}"))?;
    drop(stream);
    for _ in 0..100 {
        if network_tripwire.connection_count.load(Ordering::Acquire) != 0 {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(5));
    }
    let error = match network_tripwire.assert_unused() {
        Ok(()) => return Err("planted network connection was not detected".to_string()),
        Err(error) => error,
    };
    if error.contains("attempted 1 proxied network connection") {
        return Ok(());
    }
    Err(format!("unexpected network tripwire error: {error}"))
}

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
        "ee_model2vec_download_pending",
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
fn registered_model2vec_fixture_is_neural_without_overrides_or_download_path() -> TestResult {
    let fixture_root = std::env::var_os("EE_EMBED_MODEL_FIXTURE_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "EE_EMBED_MODEL_FIXTURE_DIR must name the real model fixture".to_string())?;
    let fixture_model_dir = resolve_fixture_model_dir(&fixture_root)?;
    verify_dir_cached(&ModelManifest::potion_128m(), &fixture_model_dir).map_err(|error| {
        format!("real model fixture failed frozen manifest verification: {error}")
    })?;
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
    let registered_entries_before = sorted_directory_entry_names(&registered_parent)?;
    if registered_entries_before != ["potion-multilingual-128M"] {
        return Err(format!(
            "registry fixture parent contains unexpected entries: {registered_entries_before:?}"
        ));
    }
    let network_tripwire = NetworkTripwire::start()?;
    let network_env = network_tripwire.proxy_env();

    let init = run_ee_with_env(
        &workspace,
        "registered_init",
        &["init", "--workspace", workspace.workspace_arg()?, "--json"],
        &network_env,
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
        &network_env,
    )?;
    ensure_success(&remember, "registered ee remember")?;
    let remember_json = stdout_json(&remember, "registered ee remember")?;
    let remember_data = response_data(&remember_json, "registered ee remember")?;
    let memory_id = remember_data
        .get("memoryId")
        .or_else(|| remember_data.get("memory_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "registered remember response missing memory ID".to_string())?
        .to_owned();

    let reembed = run_ee_with_env(
        &workspace,
        "registered_reembed",
        &[
            "index",
            "reembed",
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
        ],
        &network_env,
    )?;
    ensure_success(&reembed, "registered ee index reembed")?;
    let reembed_json = stdout_json(&reembed, "registered ee index reembed")?;
    let reembed_data = response_data(&reembed_json, "registered ee index reembed")?;
    ensure_eq_str(
        string_member(reembed_data, "status")?,
        "success",
        "registered reembed status",
    )?;
    ensure_eq_str(
        string_member(reembed_data, "job_status")?,
        "completed",
        "registered reembed job status",
    )?;
    ensure_u64_at_least(
        u64_member(reembed_data, "documents_total")?,
        1,
        "registered reembed document count",
    )?;
    ensure_eq_bool(
        reembed_data
            .get("embedding")
            .and_then(Value::as_object)
            .and_then(|embedding| embedding.get("semantic"))
            .and_then(Value::as_bool)
            .ok_or_else(|| "missing reembed embedding.semantic".to_string())?,
        true,
        "registered reembed semantic",
    )?;
    ensure_eq_str(
        reembed_data
            .get("embedding")
            .and_then(Value::as_object)
            .and_then(|embedding| embedding.get("source"))
            .and_then(Value::as_str)
            .ok_or_else(|| "missing reembed embedding.source".to_string())?,
        "registry_observed",
        "registered reembed source",
    )?;
    let fast_model_id = reembed_data
        .get("embedding")
        .and_then(Value::as_object)
        .and_then(|embedding| embedding.get("fast_model_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "missing reembed embedding.fast_model_id".to_string())?;
    if !fast_model_id.contains("potion-multilingual-128M") {
        return Err(format!(
            "registered reembed selected unexpected fast model {fast_model_id}"
        ));
    }
    ensure_u64_at_least(
        reembed_data
            .get("embedding")
            .and_then(Value::as_object)
            .and_then(|embedding| embedding.get("registered_model_count"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "missing reembed embedding.registered_model_count".to_string())?,
        1,
        "registered reembed model count",
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
        &network_env,
    )?;
    ensure_success(&search, "registered ee search")?;
    ensure_response_embed_backend(&search, "registered ee search", "neural_local")?;
    let search_json = stdout_json(&search, "registered ee search")?;
    let search_data = response_data(&search_json, "registered ee search")?;
    ensure_eq_str(
        string_member(search_data, "status")?,
        "success",
        "registered search status",
    )?;
    let search_results = search_data
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "registered search missing results array".to_string())?;
    let remembered_search_result = search_results.iter().find(|result| {
        result
            .get("memoryId")
            .or_else(|| result.get("memory_id"))
            .and_then(Value::as_str)
            == Some(memory_id.as_str())
    });
    let Some(remembered_search_result) = remembered_search_result else {
        return Err(format!(
            "registered neural search did not return remembered memory {memory_id}"
        ));
    };
    ensure_eq_str(
        remembered_search_result
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "registered search result missing content".to_string())?,
        "Verified local semantic models must remain usable when downloads are disabled.",
        "registered search content",
    )?;
    if remembered_search_result
        .get("fastScore")
        .and_then(Value::as_f64)
        .is_none()
    {
        return Err("registered search result did not carry a semantic fastScore".to_string());
    }
    if !remembered_search_result
        .get("provenance")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("kind").and_then(Value::as_str) == Some("search_document")
                    && entry.get("docId").and_then(Value::as_str) == Some(memory_id.as_str())
            })
        })
    {
        return Err(
            "registered search provenance did not bind the remembered document".to_string(),
        );
    }
    ensure_text_absent(
        &search.stdout,
        "embed_model_unavailable",
        "registered neural search degradation",
    )?;

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
        &network_env,
    )?;
    ensure_success(&pack, "registered ee pack")?;
    ensure_response_embed_backend(&pack, "registered ee pack", "neural_local")?;
    let pack_json = stdout_json(&pack, "registered ee pack")?;
    let packed_item = pack_json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                item.get("memoryId").and_then(Value::as_str) == Some(memory_id.as_str())
            })
        })
        .ok_or_else(|| format!("registered neural pack omitted memory {memory_id}"))?;
    ensure_eq_str(
        packed_item
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "registered packed item missing content".to_string())?,
        "Verified local semantic models must remain usable when downloads are disabled.",
        "registered packed content",
    )?;
    let expected_pack_provenance = format!("ee://memory/{memory_id}");
    if !packed_item
        .get("provenance")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("uri").and_then(Value::as_str) == Some(expected_pack_provenance.as_str())
            })
        })
    {
        return Err(format!(
            "registered packed item provenance did not bind {expected_pack_provenance}"
        ));
    }
    ensure_text_absent(
        &pack.stdout,
        "embed_model_unavailable",
        "registered neural pack degradation",
    )?;

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
        &network_env,
    )?;
    ensure_success(&orient, "registered ee orient")?;
    ensure_response_embed_backend(&orient, "registered ee orient", "neural_local")?;

    // Hostile ambient remote-provider intent is deliberate: download-off must
    // select the verified local registry artifact directly, without invoking
    // general auto-detection, a remote provider, or the ee downloader.
    let mut download_off_env = network_env.clone();
    download_off_env.extend([
        ("EE_EMBED_DOWNLOAD".to_string(), "off".to_string()),
        (
            "FRANKENSEARCH_API_PROVIDER".to_string(),
            "openai".to_string(),
        ),
        (
            "FRANKENSEARCH_API_MODEL".to_string(),
            "ambient-remote-must-not-win".to_string(),
        ),
        (
            "OPENAI_API_KEY".to_string(),
            "network-tripwire-not-a-real-key".to_string(),
        ),
    ]);
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
    let download_off_json = stdout_json(
        &search_download_off,
        "registered ee search with downloads off",
    )?;
    let download_off_results = download_off_json
        .pointer("/data/results")
        .and_then(Value::as_array)
        .ok_or_else(|| "download-off registered search missing results".to_string())?;
    let download_off_result = download_off_results
        .iter()
        .find(|result| result.get("memoryId").and_then(Value::as_str) == Some(memory_id.as_str()));
    let Some(download_off_result) = download_off_result else {
        return Err(format!(
            "download-off registered neural search did not return {memory_id}"
        ));
    };
    if download_off_result
        .get("fastScore")
        .and_then(Value::as_f64)
        .is_none()
    {
        return Err("download-off search did not carry a semantic fastScore".to_string());
    }
    ensure_text_absent(
        &search_download_off.stdout,
        "embed_model_unavailable",
        "download-off registered neural degradation",
    )?;

    for (name, output) in [
        ("init", &init),
        ("remember", &remember),
        ("reembed", &reembed),
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

    let legacy_download_destination = workspace
        .xdg_data
        .join("ee")
        .join("models")
        .join("potion-multilingual-128M");
    if legacy_download_destination.exists() {
        return Err(format!(
            "download-off created the legacy download destination {}",
            legacy_download_destination.display()
        ));
    }
    let registered_entries_after = sorted_directory_entry_names(&registered_parent)?;
    if registered_entries_after != registered_entries_before {
        return Err(format!(
            "registered local-model commands created downloader staging artifacts: before={registered_entries_before:?} after={registered_entries_after:?}"
        ));
    }
    network_tripwire.assert_unused()?;
    Ok(())
}

#[cfg(unix)]
#[test]
#[ignore = "requires the real potion-multilingual-128M fixture"]
fn registered_noncanonical_model2vec_source_is_neural_without_egress() -> TestResult {
    let fixture_root = std::env::var_os("EE_EMBED_MODEL_FIXTURE_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "EE_EMBED_MODEL_FIXTURE_DIR must name the real model fixture".to_string())?;
    let fixture_model_dir = resolve_fixture_model_dir(&fixture_root)?;
    verify_dir_cached(&ModelManifest::potion_128m(), &fixture_model_dir).map_err(|error| {
        format!("real model fixture failed frozen manifest verification: {error}")
    })?;

    let workspace = E2eWorkspace::create("registered-noncanonical-model2vec-offline")?;
    let bootstrap_model_parent = workspace.path.join("explicit-model-override");
    fs::create_dir_all(&bootstrap_model_parent)
        .map_err(|error| format!("create {}: {error}", bootstrap_model_parent.display()))?;
    let bootstrap_model_dir = bootstrap_model_parent.join(BUNDLED_EMBEDDING_MODEL_ID);
    std::os::unix::fs::symlink(&fixture_model_dir, &bootstrap_model_dir).map_err(|error| {
        format!(
            "link real model fixture {} for explicit bootstrap at {}: {error}",
            fixture_model_dir.display(),
            bootstrap_model_dir.display()
        )
    })?;
    let noncanonical_parent = workspace.path.join("private-model-store");
    fs::create_dir_all(&noncanonical_parent)
        .map_err(|error| format!("create {}: {error}", noncanonical_parent.display()))?;
    let noncanonical_model_dir = noncanonical_parent.join("verified-artifact");
    std::os::unix::fs::symlink(&fixture_model_dir, &noncanonical_model_dir).map_err(|error| {
        format!(
            "link real model fixture {} at noncanonical path {}: {error}",
            fixture_model_dir.display(),
            noncanonical_model_dir.display()
        )
    })?;

    let network_tripwire = NetworkTripwire::start()?;
    let mut bootstrap_env = network_tripwire.proxy_env();
    bootstrap_env.extend([
        (
            "EE_EMBED_MODEL_DIR".to_string(),
            path_string(&bootstrap_model_dir),
        ),
        ("EE_EMBED_DOWNLOAD".to_string(), "off".to_string()),
    ]);
    let init = run_ee_with_env(
        &workspace,
        "registry_path_init",
        &["init", "--workspace", workspace.workspace_arg()?, "--json"],
        &bootstrap_env,
    )?;
    ensure_success(&init, "registry-path ee init")?;
    let remember = run_ee_with_env(
        &workspace,
        "registry_path_remember",
        &[
            "remember",
            "A verified noncanonical registry path supports offline semantic retrieval.",
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
        &bootstrap_env,
    )?;
    ensure_success(&remember, "registry-path ee remember")?;
    let remember_json = stdout_json(&remember, "registry-path ee remember")?;
    let remember_data = response_data(&remember_json, "registry-path ee remember")?;
    let memory_id = remember_data
        .get("memoryId")
        .or_else(|| remember_data.get("memory_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "registry-path remember response missing memory ID".to_string())?
        .to_owned();
    let reembed = run_ee_with_env(
        &workspace,
        "registry_path_reembed",
        &[
            "index",
            "reembed",
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
        ],
        &bootstrap_env,
    )?;
    ensure_success(&reembed, "registry-path ee index reembed")?;

    let initial_entry = model2vec_registry_entry(&workspace)?;
    let verified_hash = initial_entry
        .content_hash
        .clone()
        .ok_or_else(|| "available Model2Vec registry row missing content hash".to_string())?;
    let registered_entry = update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some(path_string(&noncanonical_model_dir)),
        Some(verified_hash.clone()),
    )?;
    ensure_eq_str(
        registered_entry.source_uri.as_deref().unwrap_or_default(),
        path_string(&noncanonical_model_dir).as_str(),
        "persisted noncanonical Model2Vec source URI",
    )?;

    let mut offline_env = network_tripwire.proxy_env();
    offline_env.push(("EE_EMBED_DOWNLOAD".to_string(), "off".to_string()));
    let query = "offline noncanonical semantic registry path";
    let search = run_ee_with_env(
        &workspace,
        "registry_path_search",
        &[
            "search",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--relevance-floor",
            "0",
            "--json",
        ],
        &offline_env,
    )?;
    ensure_success(&search, "registry-path ee search")?;
    ensure_response_embed_backend(&search, "registry-path ee search", "neural_local")?;
    let search_json = stdout_json(&search, "registry-path ee search")?;
    let remembered_hit = search_json
        .pointer("/data/results")
        .and_then(Value::as_array)
        .and_then(|results| {
            results.iter().find(|result| {
                result.get("memoryId").and_then(Value::as_str) == Some(memory_id.as_str())
            })
        })
        .ok_or_else(|| format!("registry-path neural search omitted memory {memory_id}"))?;
    if remembered_hit
        .get("fastScore")
        .and_then(Value::as_f64)
        .is_none()
    {
        return Err("registry-path neural search did not execute a semantic fastScore".to_string());
    }
    ensure_text_absent(
        &search.stdout,
        "embed_model_unavailable",
        "registry-path neural search degradation",
    )?;

    let pack = run_ee_with_env(
        &workspace,
        "registry_path_pack",
        &[
            "pack",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--max-tokens",
            "800",
            "--json",
        ],
        &offline_env,
    )?;
    ensure_success(&pack, "registry-path ee pack")?;
    ensure_response_embed_backend(&pack, "registry-path ee pack", "neural_local")?;

    let orient = run_ee_with_env(
        &workspace,
        "registry_path_orient",
        &[
            "orient",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
        ],
        &offline_env,
    )?;
    ensure_success(&orient, "registry-path full ee orient")?;
    ensure_response_embed_backend(&orient, "registry-path full ee orient", "neural_local")?;

    let database_state_before_why_not = database_artifact_state(&workspace)?;
    let why_not = run_ee_with_env(
        &workspace,
        "registry_path_why_not",
        &[
            "why-not",
            &memory_id,
            "--task",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
        ],
        &offline_env,
    )?;
    ensure_success(&why_not, "registry-path ee why-not")?;
    ensure_text_absent(
        &why_not.stdout,
        "embed_model_unavailable",
        "registry-path neural why-not degradation",
    )?;
    let database_state_after_why_not = database_artifact_state(&workspace)?;
    if database_state_after_why_not != database_state_before_why_not {
        return Err(format!(
            "registry-path why-not mutated database artifacts: before={database_state_before_why_not:?} after={database_state_after_why_not:?}"
        ));
    }

    let missing_source = workspace.path.join("missing-model-source");
    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some(path_string(&missing_source)),
        Some(verified_hash.clone()),
    )?;
    let missing = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_missing_source",
        query,
        &offline_env,
    )?;

    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Unavailable,
        Some(path_string(&noncanonical_model_dir)),
        Some(verified_hash.clone()),
    )?;
    let unavailable = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_unavailable_row",
        query,
        &offline_env,
    )?;

    let unverified_model_dir = noncanonical_parent.join("unverified-artifact");
    fs::create_dir_all(&unverified_model_dir)
        .map_err(|error| format!("create {}: {error}", unverified_model_dir.display()))?;
    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some(path_string(&unverified_model_dir)),
        Some(verified_hash.clone()),
    )?;
    let unverified = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_unverified_source",
        query,
        &offline_env,
    )?;

    update_model2vec_registry_entry(
        &workspace,
        "wrong-model-name",
        ModelRegistryStatus::Available,
        Some(path_string(&noncanonical_model_dir)),
        Some(verified_hash.clone()),
    )?;
    let mismatched_name = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_mismatched_name",
        query,
        &offline_env,
    )?;

    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some(path_string(&noncanonical_model_dir)),
        Some(format!("blake3:{}", "0".repeat(64))),
    )?;
    let mismatched_hash = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_mismatched_hash",
        query,
        &offline_env,
    )?;

    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some("https://models.invalid/potion-multilingual-128M".to_string()),
        Some(verified_hash),
    )?;
    let nonlocal = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_nonlocal_source",
        query,
        &offline_env,
    )?;

    for (name, output) in [
        ("init", &init),
        ("remember", &remember),
        ("reembed", &reembed),
        ("search", &search),
        ("pack", &pack),
        ("orient", &orient),
        ("why_not", &why_not),
        ("missing", &missing),
        ("unavailable", &unavailable),
        ("unverified", &unverified),
        ("mismatched_name", &mismatched_name),
        ("mismatched_hash", &mismatched_hash),
        ("nonlocal", &nonlocal),
    ] {
        ensure_text_absent(
            &output.stderr,
            "downloading the local embedding model",
            &format!("{name} download notice"),
        )?;
    }

    for unexpected_cache_path in [
        workspace
            .xdg_data
            .join("ee/models/model2vec/potion-multilingual-128M"),
        workspace
            .xdg_data
            .join("ee/models/potion-multilingual-128M"),
    ] {
        if unexpected_cache_path.exists() {
            return Err(format!(
                "offline registry resolution created cache path {}",
                unexpected_cache_path.display()
            ));
        }
    }
    network_tripwire.assert_unused()
}

#[cfg(unix)]
fn model2vec_registry_entry(workspace: &E2eWorkspace) -> TestResult<StoredModelRegistryEntry> {
    let database_path = workspace.path.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    let stored_workspace = connection
        .get_workspace_by_path(workspace.workspace_arg()?)
        .map_err(|error| format!("read registry workspace: {error}"))?
        .ok_or_else(|| "registry workspace row missing".to_string())?;
    connection
        .list_model_registry_entries(&stored_workspace.id)
        .map_err(|error| format!("list model registry entries: {error}"))?
        .into_iter()
        .find(|entry| {
            entry.provider == ModelProvider::Model2Vec && entry.purpose == ModelPurpose::Embedding
        })
        .ok_or_else(|| "Model2Vec embedding registry row missing".to_string())
}

#[cfg(unix)]
fn update_model2vec_registry_entry(
    workspace: &E2eWorkspace,
    model_name: &str,
    status: ModelRegistryStatus,
    source_uri: Option<String>,
    content_hash: Option<String>,
) -> TestResult<StoredModelRegistryEntry> {
    let entry = model2vec_registry_entry(workspace)?;
    let database_path = workspace.path.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    let updated = connection
        .update_model_registry_entry(
            &entry.id,
            &CreateModelRegistryInput {
                workspace_id: entry.workspace_id.clone(),
                provider: entry.provider,
                model_name: model_name.to_string(),
                purpose: entry.purpose,
                dimension: entry.dimension,
                distance_metric: entry.distance_metric,
                status,
                version: entry.version.clone(),
                source_uri,
                content_hash,
                metadata_json: entry.metadata_json.clone(),
                last_checked_at: entry.last_checked_at.clone(),
            },
        )
        .map_err(|error| format!("update Model2Vec registry entry: {error}"))?;
    if !updated {
        return Err(format!(
            "Model2Vec registry row {} was not updated",
            entry.id
        ));
    }
    connection
        .get_model_registry_entry(&entry.id)
        .map_err(|error| format!("read updated Model2Vec registry entry: {error}"))?
        .ok_or_else(|| format!("updated Model2Vec registry row {} missing", entry.id))
}

#[cfg(unix)]
fn run_offline_registry_fallback_search(
    workspace: &E2eWorkspace,
    phase: &str,
    query: &str,
    env: &[(String, String)],
) -> TestResult<Output> {
    let output = run_ee_with_env(
        workspace,
        phase,
        &[
            "search",
            query,
            "--workspace",
            workspace.workspace_arg()?,
            "--relevance-floor",
            "0",
            "--json",
        ],
        env,
    )?;
    ensure_success(&output, phase)?;
    ensure_response_embed_backend(&output, phase, "hash_fallback")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("embed_model_unavailable") {
        return Err(format!(
            "{phase} did not report explicit embed_model_unavailable fallback"
        ));
    }
    Ok(output)
}

#[cfg(unix)]
fn database_artifact_state(workspace: &E2eWorkspace) -> TestResult<Vec<(String, Option<String>)>> {
    let database = workspace.path.join(".ee").join("ee.db");
    [
        database.clone(),
        database.with_file_name("ee.db-wal"),
        database.with_file_name("ee.db-shm"),
    ]
    .into_iter()
    .map(|path| {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                format!(
                    "database artifact path is not valid UTF-8: {}",
                    path.display()
                )
            })?
            .to_string();
        if !path.exists() {
            return Ok((name, None));
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("read database artifact {}: {error}", path.display()))?;
        Ok((name, Some(blake3::hash(&bytes).to_hex().to_string())))
    })
    .collect()
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
fn sorted_directory_entry_names(path: &Path) -> TestResult<Vec<String>> {
    let mut names = fs::read_dir(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .map_err(|error| format!("read entry under {}: {error}", path.display()))
        })
        .collect::<TestResult<Vec<_>>>()?;
    names.sort_unstable();
    Ok(names)
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
        .env_remove("EE_EMBED_MODEL_FIXTURE_DIR")
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
        .env_remove("GEMINI_API_KEY")
        .env_remove("EE_MAX_OUTPUT_TOKENS")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
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
