//! Real-binary e2e pin for bundled embedding model registration.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Stdio};
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::thread::{self, JoinHandle};
#[cfg(unix)]
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::core::model::{BUNDLED_EMBEDDING_DIMENSION, BUNDLED_EMBEDDING_MODEL_ID};
#[cfg(unix)]
use ee::daemon::protocol::{DAEMON_SEARCH_REQUEST_SCHEMA_V1, DaemonRequest, METHOD_SEARCH};
#[cfg(unix)]
use ee::daemon::server::{METHOD_SHUTDOWN, client_round_trip, client_round_trip_before};
#[cfg(unix)]
use ee::db::{CreateModelRegistryInput, DbConnection, StoredModelRegistryEntry};
use ee::models::{
    ERROR_SCHEMA_V2, MODEL_LIST_SCHEMA_V1, MODEL_STATUS_SCHEMA_V2, RESPONSE_SCHEMA_V2,
};
#[cfg(unix)]
use ee::models::{ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus};
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

#[test]
fn model_fetch_embedding_default_uses_public_embedding_dispatcher() -> TestResult {
    let workspace = E2eWorkspace::create("fetch-embedding-default-dispatch")?;
    let rerank_only_artifact = workspace.path.join("rerank-only-artifact.tar.zst");
    let rerank_only_artifact_arg = path_string(&rerank_only_artifact);
    let output = run_ee(
        &workspace,
        "model_fetch_embedding_default_dispatch",
        &[
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
            "model",
            "fetch",
            "embedding-default",
            "--from-file",
            &rerank_only_artifact_arg,
        ],
    )?;

    if output.status.success() {
        return Err("embedding-default --from-file unexpectedly succeeded".to_string());
    }
    ensure_empty_stderr(&output, "ee model fetch embedding-default --from-file")?;
    let value = stdout_json(&output, "ee model fetch embedding-default --from-file")?;
    ensure_eq_str(
        string_value(&value, "schema")?,
        ERROR_SCHEMA_V2,
        "embedding fetch error schema",
    )?;
    let error = value
        .get("error")
        .and_then(Value::as_object)
        .ok_or_else(|| "embedding fetch error object missing".to_string())?;
    ensure_eq_str(
        string_member(error, "code")?,
        "usage",
        "embedding fetch error code",
    )?;
    ensure_eq_str(
        string_member(error, "message")?,
        "embedding-default is fetched from the pinned frankensearch manifest; --from-file is only supported for rerank artifacts",
        "embedding fetch dispatcher message",
    )?;
    ensure_eq_str(
        string_member(error, "repair")?,
        "ee model fetch embedding-default",
        "embedding fetch dispatcher repair",
    )
}

struct E2eWorkspace {
    path: PathBuf,
    home: PathBuf,
    xdg_data: PathBuf,
    log_path: PathBuf,
}

impl E2eWorkspace {
    fn create(test_name: &str) -> TestResult<Self> {
        let base = match std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
            Some(target_dir) => fs::canonicalize(&target_dir).map_err(|error| {
                format!(
                    "canonicalize CARGO_TARGET_DIR {}: {error}",
                    target_dir.display()
                )
            })?,
            None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"),
        };
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

#[cfg(unix)]
struct RunningE2eDaemon {
    socket_path: PathBuf,
    workspace_id: String,
    child: Child,
}

#[cfg(unix)]
impl RunningE2eDaemon {
    fn start(workspace: &E2eWorkspace, env: &[(String, String)]) -> TestResult<Self> {
        let socket_parent = workspace.path.join("daemon-sockets");
        fs::create_dir_all(&socket_parent)
            .map_err(|error| format!("create {}: {error}", socket_parent.display()))?;
        fs::set_permissions(&socket_parent, fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                format!(
                    "secure daemon socket parent {}: {error}",
                    socket_parent.display()
                )
            },
        )?;
        let socket_path = socket_parent.join("registry-path-e2e.sock");
        let socket_arg = path_string(&socket_path);
        let mut command = ee_command_with_env(
            workspace,
            &[
                "--workspace",
                workspace.workspace_arg()?,
                "--json",
                "daemon",
                "start",
                "--foreground",
                "--socket",
                &socket_arg,
            ],
            env,
        );
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let child = command
            .spawn()
            .map_err(|error| format!("spawn public foreground ee daemon: {error}"))?;
        let mut running = Self {
            socket_path,
            workspace_id: workspace.workspace_arg()?.to_owned(),
            child,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if UnixStream::connect(&running.socket_path).is_ok() {
                break;
            }
            if let Some(status) = running
                .child
                .try_wait()
                .map_err(|error| format!("poll public foreground ee daemon: {error}"))?
            {
                return Err(format!(
                    "public foreground ee daemon exited before readiness: {status}"
                ));
            }
            if Instant::now() >= deadline {
                return Err(
                    "public foreground ee daemon did not publish its socket within 10s".to_string(),
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
        let stdout = running
            .child
            .stdout
            .take()
            .ok_or_else(|| "public foreground ee daemon stdout was not piped".to_string())?;
        let mut reader = BufReader::new(stdout);
        let mut startup_line = String::new();
        reader
            .read_line(&mut startup_line)
            .map_err(|error| format!("read public daemon startup envelope: {error}"))?;
        let startup: Value = serde_json::from_str(&startup_line).map_err(|error| {
            format!("public daemon startup emitted invalid JSON: {error}; stdout={startup_line}")
        })?;
        ensure_eq_str(
            startup
                .get("schema")
                .and_then(Value::as_str)
                .ok_or_else(|| "public daemon startup schema missing".to_string())?,
            RESPONSE_SCHEMA_V2,
            "public daemon startup schema",
        )?;
        ensure_eq_bool(
            startup
                .get("success")
                .and_then(Value::as_bool)
                .ok_or_else(|| "public daemon startup success missing".to_string())?,
            true,
            "public daemon startup success",
        )?;
        ensure_eq_str(
            startup
                .pointer("/data/socketPath")
                .and_then(Value::as_str)
                .ok_or_else(|| "public daemon startup socketPath missing".to_string())?,
            &path_string(&running.socket_path),
            "public daemon startup socketPath",
        )?;
        Ok(running)
    }

    fn socket_arg(&self) -> String {
        path_string(&self.socket_path)
    }

    fn prewarm_search(&self, workspace: &E2eWorkspace, query: &str) -> TestResult {
        let mut request = DaemonRequest::new(
            format!("registry-path-prewarm-{}", std::process::id()),
            "model-bundled-embedding-cli-e2e",
            METHOD_SEARCH,
            json!({
                "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V1,
                "query": query,
                "workspacePath": workspace.workspace_arg()?,
                "databasePath": path_string(&workspace.path.join(".ee/ee.db")),
                "indexDir": path_string(&workspace.path.join(".ee/index")),
                "limit": 10,
                "speed": "default",
                "relevanceFloor": 0.0,
                "sourceMode": "hybrid",
                "strictSourceMode": true,
                "memoryScope": "swarm"
            }),
        );
        request.workspace_id = Some(workspace.workspace_arg()?.to_owned());
        let response = client_round_trip_before(
            &self.socket_path,
            &request,
            Instant::now() + Duration::from_secs(120),
        )
        .map_err(|error| format!("prewarm public daemon search: {error}"))?;
        if let Some(error) = response.error {
            return Err(format!("prewarm public daemon search failed: {error:?}"));
        }
        let result = response
            .result
            .ok_or_else(|| "prewarm public daemon search result missing".to_string())?;
        ensure_eq_str(
            result
                .pointer("/response/data/embed_backend")
                .and_then(Value::as_str)
                .ok_or_else(|| "prewarm public daemon embed_backend missing".to_string())?,
            "neural_local",
            "prewarm public daemon embed_backend",
        )
    }
}

#[cfg(unix)]
impl Drop for RunningE2eDaemon {
    fn drop(&mut self) {
        let mut request = DaemonRequest::new(
            format!("registry-path-shutdown-{}", std::process::id()),
            "model-bundled-embedding-cli-e2e",
            METHOD_SHUTDOWN,
            json!({}),
        );
        request.workspace_id = Some(self.workspace_id.clone());
        let _ = client_round_trip(&self.socket_path, &request);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    ensure_degraded_code_count(
        &search,
        "registered neural search degradation",
        "embed_model_unavailable",
        0,
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
    ensure_degraded_code_count(
        &pack,
        "registered neural pack degradation",
        "embed_model_unavailable",
        0,
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
    ensure_degraded_code_count(
        &search_download_off,
        "download-off registered neural degradation",
        "embed_model_unavailable",
        0,
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

    let registered_model_source_tokens = [
        path_string(&fixture_model_dir),
        path_string(&registered_model_dir),
    ];
    for (name, output) in [
        ("search", &search),
        ("pack", &pack),
        ("orient", &orient),
        ("search_download_off", &search_download_off),
    ] {
        ensure_model_sources_absent(
            output,
            &format!("registered {name} model-source leak"),
            &registered_model_source_tokens,
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
fn public_reembed_persists_canonical_model2vec_source_and_offline_search_is_neural() -> TestResult {
    let fixture_root = std::env::var_os("EE_EMBED_MODEL_FIXTURE_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "EE_EMBED_MODEL_FIXTURE_DIR must name the real model fixture".to_string())?;
    let fixture_model_dir = resolve_fixture_model_dir(&fixture_root)?;
    verify_dir_cached(&ModelManifest::potion_128m(), &fixture_model_dir).map_err(|error| {
        format!("real model fixture failed frozen manifest verification: {error}")
    })?;

    let workspace = E2eWorkspace::create("public-canonical-model2vec-offline")?;
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
    let corruption_parent = workspace.path.join("corrupt-model-registry-fixtures");
    fs::create_dir_all(&corruption_parent)
        .map_err(|error| format!("create {}: {error}", corruption_parent.display()))?;

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
            "A canonical verified registry path supports offline semantic retrieval.",
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

    let list = run_ee_with_env(
        &workspace,
        "registry_path_model_list",
        &[
            "model",
            "list",
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
        ],
        &bootstrap_env,
    )?;
    ensure_success(&list, "registry-path ee model list")?;
    let list_json = stdout_json(&list, "registry-path ee model list")?;
    let list_entry = list_json
        .pointer("/data/entries")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.get("provider").and_then(Value::as_str) == Some("model2vec")
                    && entry.get("modelName").and_then(Value::as_str)
                        == Some(BUNDLED_EMBEDDING_MODEL_ID)
                    && entry.get("purpose").and_then(Value::as_str) == Some("embedding")
            })
        })
        .ok_or_else(|| "public model list omitted the available Model2Vec row".to_string())?;
    ensure_eq_str(
        string_member(list_entry, "status")?,
        "available",
        "public Model2Vec registry status",
    )?;
    ensure_eq_u64(
        u64_member(list_entry, "dimension")?,
        u64::from(BUNDLED_EMBEDDING_DIMENSION),
        "public Model2Vec dimension",
    )?;
    ensure_eq_str(
        string_member(list_entry, "distanceMetric")?,
        ModelDistanceMetric::Cosine.as_str(),
        "public Model2Vec distance metric",
    )?;
    let public_hash = string_member(list_entry, "contentHash")?.to_string();
    if !public_hash.starts_with("blake3:") || public_hash.len() != "blake3:".len() + 64 {
        return Err(format!(
            "public Model2Vec content hash is not a pinned blake3 identity: {public_hash}"
        ));
    }
    ensure_eq_str(
        string_member(list_entry, "sourceUri")?,
        "[REDACTED_PATH]",
        "public Model2Vec source path redaction",
    )?;
    ensure_degraded_code_count(
        &list,
        "public Model2Vec availability degradation",
        "model_registry_no_available_entry",
        0,
    )?;

    let canonical_fixture_model_dir = fs::canonicalize(&fixture_model_dir).map_err(|error| {
        format!(
            "canonicalize real model fixture {}: {error}",
            fixture_model_dir.display()
        )
    })?;
    let registered_entry = model2vec_registry_entry(&workspace)?;
    let verified_hash = registered_entry
        .content_hash
        .clone()
        .ok_or_else(|| "available Model2Vec registry row missing content hash".to_string())?;
    ensure_eq_str(
        registered_entry.source_uri.as_deref().unwrap_or_default(),
        path_string(&canonical_fixture_model_dir).as_str(),
        "persisted canonical Model2Vec source URI",
    )?;
    ensure_eq_str(
        &verified_hash,
        &public_hash,
        "public and persisted Model2Vec content hash",
    )?;
    ensure_eq_u64(
        u64::from(registered_entry.dimension.unwrap_or_default()),
        u64::from(BUNDLED_EMBEDDING_DIMENSION),
        "persisted Model2Vec dimension",
    )?;
    if registered_entry.distance_metric != Some(ModelDistanceMetric::Cosine) {
        return Err(format!(
            "persisted Model2Vec distance identity drifted: {:?}",
            registered_entry.distance_metric
        ));
    }

    let mut offline_env = network_tripwire.proxy_env();
    offline_env.push(("EE_EMBED_DOWNLOAD".to_string(), "off".to_string()));
    let query = "offline canonical semantic registry path";
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
    ensure_degraded_code_count(
        &search,
        "registry-path neural search degradation",
        "embed_model_unavailable",
        0,
    )?;

    let registry_before_daemon = model2vec_registry_entry(&workspace)?;
    ensure_eq_str(
        registry_before_daemon
            .source_uri
            .as_deref()
            .ok_or_else(|| "registered source missing before daemon search".to_string())?,
        &path_string(&canonical_fixture_model_dir),
        "registered source before daemon search",
    )?;
    let index_manifest_path = workspace.path.join(".ee/index/meta.json");
    let index_manifest_before = fs::read(&index_manifest_path).map_err(|error| {
        format!(
            "read registry-path index manifest {}: {error}",
            index_manifest_path.display()
        )
    })?;
    let index_manifest_json: Value = serde_json::from_slice(&index_manifest_before)
        .map_err(|error| format!("parse registry-path index manifest: {error}"))?;
    ensure_eq_str(
        index_manifest_json
            .get("storedModelId")
            .and_then(Value::as_str)
            .ok_or_else(|| "index manifest storedModelId missing".to_string())?,
        &registry_before_daemon.model_name,
        "index manifest storedModelId",
    )?;
    ensure_eq_str(
        index_manifest_json
            .get("storedModelRevision")
            .and_then(Value::as_str)
            .ok_or_else(|| "index manifest storedModelRevision missing".to_string())?,
        registry_before_daemon
            .version
            .as_deref()
            .ok_or_else(|| "registered model revision missing".to_string())?,
        "index manifest storedModelRevision",
    )?;
    ensure_eq_str(
        index_manifest_json
            .get("storedModelHash")
            .and_then(Value::as_str)
            .ok_or_else(|| "index manifest storedModelHash missing".to_string())?,
        registry_before_daemon
            .content_hash
            .as_deref()
            .ok_or_else(|| "registered model hash missing".to_string())?,
        "index manifest storedModelHash",
    )?;
    ensure_eq_u64(
        index_manifest_json
            .get("storedDimension")
            .and_then(Value::as_u64)
            .ok_or_else(|| "index manifest storedDimension missing".to_string())?,
        u64::from(BUNDLED_EMBEDDING_DIMENSION),
        "index manifest storedDimension",
    )?;
    ensure_eq_str(
        index_manifest_json
            .get("storedDistanceMetric")
            .and_then(Value::as_str)
            .ok_or_else(|| "index manifest storedDistanceMetric missing".to_string())?,
        ModelDistanceMetric::Cosine.as_str(),
        "index manifest storedDistanceMetric",
    )?;

    let (daemon_search_first, daemon_search_second, daemon_search_third) = {
        let daemon = RunningE2eDaemon::start(&workspace, &offline_env)?;
        daemon.prewarm_search(&workspace, query)?;
        let socket_arg = daemon.socket_arg();
        let run_daemon_search = |phase: &str| {
            run_ee_with_env(
                &workspace,
                phase,
                &[
                    "search",
                    query,
                    "--workspace",
                    workspace.workspace_arg()?,
                    "--relevance-floor",
                    "0",
                    "--use-daemon",
                    "--daemon-socket",
                    &socket_arg,
                    "--json",
                ],
                &offline_env,
            )
        };
        let first = run_daemon_search("registry_path_daemon_search_first")?;
        let second = run_daemon_search("registry_path_daemon_search_second")?;
        let third = run_daemon_search("registry_path_daemon_search_third")?;
        let mut stable_results = None;
        for (ordinal, output) in [("first", &first), ("second", &second), ("third", &third)] {
            ensure_success(output, &format!("registry-path {ordinal} daemon search"))?;
            ensure_response_embed_backend(
                output,
                &format!("registry-path {ordinal} daemon search"),
                "neural_local",
            )?;
            ensure_degraded_code_count(
                output,
                &format!("registry-path {ordinal} daemon search"),
                "embed_model_unavailable",
                0,
            )?;
            ensure_degraded_code_count(
                output,
                &format!("registry-path {ordinal} daemon search"),
                "daemon_search_fallback",
                0,
            )?;
            let payload = stdout_json(output, &format!("registry-path {ordinal} daemon search"))?;
            let results = payload
                .pointer("/data/results")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("registry-path {ordinal} daemon search results missing"))?;
            let remembered_hit = results
                .iter()
                .find(|result| {
                    result.get("memoryId").and_then(Value::as_str) == Some(memory_id.as_str())
                })
                .ok_or_else(|| {
                    format!("registry-path {ordinal} daemon search omitted memory {memory_id}")
                })?;
            if remembered_hit
                .get("fastScore")
                .and_then(Value::as_f64)
                .is_none()
            {
                return Err(format!(
                    "registry-path {ordinal} daemon search did not execute a semantic fastScore"
                ));
            }
            let current_results = Value::Array(results.clone());
            if let Some(expected) = &stable_results {
                if &current_results != expected {
                    return Err(format!(
                        "registry-path {ordinal} daemon search results drifted: expected={expected} actual={current_results}"
                    ));
                }
            } else {
                stable_results = Some(current_results);
            }
        }
        (first, second, third)
    };
    let registry_after_daemon = model2vec_registry_entry(&workspace)?;
    if registry_after_daemon != registry_before_daemon {
        return Err(format!(
            "repeated public daemon search mutated the model registry: before={registry_before_daemon:?} after={registry_after_daemon:?}"
        ));
    }
    let index_manifest_after = fs::read(&index_manifest_path).map_err(|error| {
        format!(
            "reread registry-path index manifest {}: {error}",
            index_manifest_path.display()
        )
    })?;
    if index_manifest_after != index_manifest_before {
        return Err("repeated public daemon search mutated the index manifest".to_string());
    }

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
    ensure_degraded_code_count(
        &pack,
        "registry-path neural pack degradation",
        "embed_model_unavailable",
        0,
    )?;

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
    ensure_degraded_code_count(
        &orient,
        "registry-path neural orient degradation",
        "embed_model_unavailable",
        0,
    )?;

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
    ensure_degraded_code_count(
        &why_not,
        "registry-path neural why-not degradation",
        "embed_model_unavailable",
        0,
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
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Cosine),
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
        ModelRegistryStatus::Available,
        None,
        Some(verified_hash.clone()),
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Cosine),
    )?;
    let unregistered = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_unregistered_source",
        query,
        &offline_env,
    )?;

    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Unavailable,
        Some(path_string(&canonical_fixture_model_dir)),
        Some(verified_hash.clone()),
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Cosine),
    )?;
    let unavailable = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_unavailable_row",
        query,
        &offline_env,
    )?;

    let unverified_model_dir = corruption_parent.join("unverified-artifact");
    fs::create_dir_all(&unverified_model_dir)
        .map_err(|error| format!("create {}: {error}", unverified_model_dir.display()))?;
    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some(path_string(&unverified_model_dir)),
        Some(verified_hash.clone()),
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Cosine),
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
        Some(path_string(&canonical_fixture_model_dir)),
        Some(verified_hash.clone()),
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Cosine),
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
        Some(path_string(&canonical_fixture_model_dir)),
        Some(format!("blake3:{}", "0".repeat(64))),
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Cosine),
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
        Some(path_string(&canonical_fixture_model_dir)),
        Some(verified_hash.clone()),
        Some(BUNDLED_EMBEDDING_DIMENSION.saturating_add(1)),
        Some(ModelDistanceMetric::Cosine),
    )?;
    let mismatched_dimension = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_mismatched_dimension",
        query,
        &offline_env,
    )?;

    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some(path_string(&canonical_fixture_model_dir)),
        Some(verified_hash.clone()),
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Dot),
    )?;
    let mismatched_distance = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_mismatched_distance",
        query,
        &offline_env,
    )?;

    let nonlocal_source = "https://models.invalid/potion-multilingual-128M";
    update_model2vec_registry_entry(
        &workspace,
        BUNDLED_EMBEDDING_MODEL_ID,
        ModelRegistryStatus::Available,
        Some(nonlocal_source.to_string()),
        Some(verified_hash.clone()),
        Some(BUNDLED_EMBEDDING_DIMENSION),
        Some(ModelDistanceMetric::Cosine),
    )?;
    let nonlocal = run_offline_registry_fallback_search(
        &workspace,
        "registry_path_nonlocal_source",
        query,
        &offline_env,
    )?;

    // Restore exclusively through the public writer after the negative DB
    // corruption cases, then prove the override-free offline resolver uses it.
    let restore = run_ee_with_env(
        &workspace,
        "registry_path_restore_reembed",
        &[
            "index",
            "reembed",
            "--workspace",
            workspace.workspace_arg()?,
            "--json",
        ],
        &bootstrap_env,
    )?;
    ensure_success(&restore, "registry-path restore reembed")?;
    let restored_entry = model2vec_registry_entry(&workspace)?;
    ensure_eq_str(
        restored_entry.source_uri.as_deref().unwrap_or_default(),
        path_string(&canonical_fixture_model_dir).as_str(),
        "restored canonical Model2Vec source URI",
    )?;
    let settled = run_ee_with_env(
        &workspace,
        "registry_path_settled_search",
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
    ensure_success(&settled, "registry-path settled search")?;
    ensure_response_embed_backend(&settled, "registry-path settled search", "neural_local")?;
    ensure_text_absent(
        &settled.stderr,
        "downloading the local embedding model",
        "settled search download notice",
    )?;

    for (name, output) in [
        ("init", &init),
        ("remember", &remember),
        ("reembed", &reembed),
        ("model_list", &list),
        ("search", &search),
        ("daemon_search_first", &daemon_search_first),
        ("daemon_search_second", &daemon_search_second),
        ("daemon_search_third", &daemon_search_third),
        ("pack", &pack),
        ("orient", &orient),
        ("why_not", &why_not),
        ("missing", &missing),
        ("unregistered", &unregistered),
        ("unavailable", &unavailable),
        ("unverified", &unverified),
        ("mismatched_name", &mismatched_name),
        ("mismatched_hash", &mismatched_hash),
        ("mismatched_dimension", &mismatched_dimension),
        ("mismatched_distance", &mismatched_distance),
        ("nonlocal", &nonlocal),
        ("restore", &restore),
    ] {
        ensure_text_absent(
            &output.stderr,
            "downloading the local embedding model",
            &format!("{name} download notice"),
        )?;
    }

    let model_source_tokens = [
        path_string(&fixture_model_dir),
        path_string(&canonical_fixture_model_dir),
        path_string(&bootstrap_model_dir),
        path_string(&missing_source),
        path_string(&unverified_model_dir),
        nonlocal_source.to_string(),
    ];
    for (name, output) in [
        ("search", &search),
        ("model_list", &list),
        ("daemon_search_first", &daemon_search_first),
        ("daemon_search_second", &daemon_search_second),
        ("daemon_search_third", &daemon_search_third),
        ("pack", &pack),
        ("orient", &orient),
        ("why_not", &why_not),
        ("missing", &missing),
        ("unregistered", &unregistered),
        ("unavailable", &unavailable),
        ("unverified", &unverified),
        ("mismatched_name", &mismatched_name),
        ("mismatched_hash", &mismatched_hash),
        ("mismatched_dimension", &mismatched_dimension),
        ("mismatched_distance", &mismatched_distance),
        ("nonlocal", &nonlocal),
        ("restore", &restore),
        ("settled", &settled),
    ] {
        ensure_model_sources_absent(
            output,
            &format!("registry-path {name} model-source leak"),
            &model_source_tokens,
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
    let connection = DbConnection::open_file_read_only(&database_path)
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
    dimension: Option<u32>,
    distance_metric: Option<ModelDistanceMetric>,
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
                dimension,
                distance_metric,
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
    ensure_degraded_code_count(&output, phase, "embed_model_unavailable", 1)?;
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

/// Base `ee` invocation with the suite's hermetic environment: HOME and
/// XDG_DATA_HOME pinned inside the workspace and every ambient model,
/// download, proxy, and provider variable scrubbed before the caller's
/// explicit env entries are applied.
fn ee_command_with_env(
    workspace: &E2eWorkspace,
    args: &[&str],
    env: &[(String, String)],
) -> Command {
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
    command
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
    let output = ee_command_with_env(workspace, args, env)
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

/// Public retrieval responses must never expose model source paths or URIs
/// on stdout or stderr.
#[cfg(unix)]
fn ensure_model_sources_absent(
    output: &Output,
    context: &str,
    model_sources: &[String],
) -> TestResult {
    for source in model_sources {
        ensure_text_absent(&output.stdout, source, &format!("{context} stdout"))?;
        ensure_text_absent(&output.stderr, source, &format!("{context} stderr"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_degraded_code_count(
    output: &Output,
    context: &str,
    expected_code: &str,
    expected_count: usize,
) -> TestResult {
    let value = stdout_json(output, context)?;
    response_data(&value, context)?;
    let degraded = value
        .get("degraded")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context} missing degraded array"))?;
    let actual_count = count_objects_by_string(degraded, "code", expected_code)?;
    if actual_count == expected_count {
        return Ok(());
    }
    Err(format!(
        "{context} expected {expected_count} degraded[].code={expected_code} entries, got {actual_count}"
    ))
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
