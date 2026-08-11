//! Daemon dispatch microbenchmarks and the warm-search latency acceptance gate.
//!
//! Group name: `ee_daemon_round_trip`
//!
//! The `--warm-search-gate` mode is the closure-grade phase-1 benchmark for
//! `bd-search-warm-latency-0bh05`. It builds one deterministic 10,000-document
//! database and index before measurement, requires the real local neural
//! backend with downloads disabled, and compares two intentionally different
//! user-visible paths over that exact fixture:
//!
//! - cold: a fresh one-shot `ee search` process for every sample;
//! - warm: a fresh `ee search --use-daemon` client process for every sample,
//!   all hitting one already-running, pre-warmed daemon.
//!
//! Both paths share the same prebuilt database/index and warmed filesystem
//! cache. The architectural process/model reuse difference is the point of the
//! comparison; this benchmark does not claim cold disk-cache performance.

#[cfg(unix)]
use std::hint::black_box;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::io::{BufRead, BufReader};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::os::unix::fs::PermissionsExt;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::os::unix::net::UnixStream;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::time::Duration;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::time::Instant;

#[cfg(unix)]
use criterion::Criterion;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::core::model::{
    BUNDLED_EMBEDDING_DIMENSION, BUNDLED_EMBEDDING_MODEL_ID, BUNDLED_EMBEDDING_MODEL_REVISION,
};
#[cfg(unix)]
use ee::daemon::protocol::DaemonRequest;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::daemon::protocol::{DAEMON_SEARCH_REQUEST_SCHEMA_V1, METHOD_SEARCH};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::daemon::server::{
    DaemonSearchResult, METHOD_SHUTDOWN, client_round_trip, client_round_trip_before,
};
#[cfg(unix)]
use ee::daemon::server::{METHOD_CONTEXT, METHOD_ECHO, dispatch};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::db::{CreateMemoryInput, CreateModelRegistryInput, CreateWorkspaceInput, DbConnection};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::models::{
    ModelDistanceMetric, ModelProvider, ModelPurpose, ModelRegistryStatus, WorkspaceId,
};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use frankensearch::embed::{ModelManifest, verify_dir_cached};
#[cfg(unix)]
use serde_json::{Value as JsonValue, json};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use tempfile::TempDir;

#[cfg(unix)]
const BENCH_GROUP_NAME: &str = "ee_daemon_round_trip";
#[cfg(unix)]
const BENCH_AGENT_ID: &str = "bench-agent";
#[cfg(unix)]
const BENCH_WORKSPACE_ID: &str = "workspace-bench";
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const SEARCH_DOCUMENT_COUNT: usize = 10_000;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const SEARCH_WARMUP_SAMPLES: usize = 3;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const SEARCH_MEASURE_SAMPLES: usize = 21;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const SEARCH_COLD_P50_BUDGET_MS: f64 = 1_500.0;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const SEARCH_WARM_P50_BUDGET_MS: f64 = 500.0;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const SEARCH_QUERY: &str = "daemon warm latency quasar sentinel retrieval";
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const SEARCH_TARGET_MEMORY_ID: &str = "mem_daemon_search_bench_04242";

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct RunningDaemon {
    _socket_dir: Option<TempDir>,
    socket_path: PathBuf,
    handle: ee::daemon::server::DaemonServerHandle,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl RunningDaemon {
    fn start() -> Self {
        let socket_dir = TempDir::new().unwrap_or_else(|error| {
            panic!("daemon benchmark tempdir: {error}");
        });
        let socket_path = socket_dir.path().join("ee-daemon-bench.sock");
        let handle = ee::daemon::server::start_server(&socket_path)
            .unwrap_or_else(|error| panic!("start daemon benchmark server: {error}"));
        Self {
            _socket_dir: Some(socket_dir),
            socket_path,
            handle,
        }
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = self.handle.shutdown();
    }
}

#[cfg(unix)]
fn echo_request() -> DaemonRequest {
    let mut request = DaemonRequest::new(
        "bench-echo-0001",
        BENCH_AGENT_ID,
        METHOD_ECHO,
        json!({
            "message": "daemon round-trip benchmark",
            "workspace": BENCH_WORKSPACE_ID,
        }),
    );
    request.workspace_id = Some(BENCH_WORKSPACE_ID.to_owned());
    request
}

#[cfg(unix)]
fn context_stub_request() -> DaemonRequest {
    let mut request = DaemonRequest::new(
        "bench-context-0001",
        BENCH_AGENT_ID,
        METHOD_CONTEXT,
        json!({
            "task": "measure daemon context stub overhead",
            "maxTokens": 4000,
        }),
    );
    request.workspace_id = Some(BENCH_WORKSPACE_ID.to_owned());
    request
}

#[cfg(unix)]
fn bench_daemon_round_trip(criterion: &mut Criterion) {
    let echo = echo_request();
    let context_stub = context_stub_request();
    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("dispatch_echo_disabled_default", |bench| {
        bench.iter(|| {
            let response = dispatch(black_box(&echo));
            black_box(response);
        });
    });

    group.bench_function("dispatch_context_stub", |bench| {
        bench.iter(|| {
            let response = dispatch(black_box(&context_stub));
            black_box(response);
        });
    });

    bench_live_socket_round_trip(&mut group, &echo, &context_stub);
    group.finish();
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn bench_live_socket_round_trip(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    echo: &DaemonRequest,
    context_stub: &DaemonRequest,
) {
    use ee::daemon::server::client_round_trip;

    let daemon = RunningDaemon::start();
    for (label, request) in [
        ("echo_disabled_default", echo),
        ("context_stub", context_stub),
    ] {
        group.bench_with_input(
            criterion::BenchmarkId::new("client_round_trip", label),
            request,
            |bench, request| {
                bench.iter(|| {
                    let response =
                        client_round_trip(black_box(daemon.socket_path()), black_box(request))
                            .unwrap_or_else(|error| panic!("daemon benchmark round-trip: {error}"));
                    black_box(response);
                });
            },
        );
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_vendor = "apple"))))]
fn bench_live_socket_round_trip(
    _group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    _echo: &DaemonRequest,
    _context_stub: &DaemonRequest,
) {
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct PrebuiltSearchFixture {
    _root: TempDir,
    workspace: PathBuf,
    database: PathBuf,
    index: PathBuf,
    home: PathBuf,
    xdg_data: PathBuf,
    fingerprint: SearchModelFingerprint,
    seed_ms: f64,
    index_ms: f64,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct SearchModelFingerprint {
    model_id: String,
    model_revision: String,
    model_hash: String,
    dimension: u32,
    distance_metric: String,
    vector_dtype: String,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct RunningPublicDaemon {
    socket_path: PathBuf,
    workspace_id: String,
    child: Child,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl RunningPublicDaemon {
    fn start(ee_binary: &Path, fixture: &PrebuiltSearchFixture) -> Self {
        let socket_dir = fixture._root.path().join("public-daemon-socket");
        std::fs::create_dir_all(&socket_dir)
            .unwrap_or_else(|error| panic!("create public daemon socket parent: {error}"));
        std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|error| panic!("secure public daemon socket parent: {error}"));
        let socket_path = socket_dir.join("ee-daemon-search-slo.sock");
        let mut command = Command::new(ee_binary);
        command
            .arg("--workspace")
            .arg(&fixture.workspace)
            .arg("--json")
            .arg("daemon")
            .arg("start")
            .arg("--foreground")
            .arg("--socket")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        configure_hermetic_process(&mut command, &fixture.home, &fixture.xdg_data, None);
        let child = command
            .spawn()
            .unwrap_or_else(|error| panic!("spawn public foreground ee daemon: {error}"));
        let mut running = Self {
            socket_path,
            workspace_id: fixture.workspace.display().to_string(),
            child,
        };
        let readiness_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if UnixStream::connect(&running.socket_path).is_ok() {
                break;
            }
            if let Some(status) = running
                .child
                .try_wait()
                .unwrap_or_else(|error| panic!("poll public foreground ee daemon: {error}"))
            {
                panic!("public foreground ee daemon exited before readiness: {status}");
            }
            assert!(
                Instant::now() < readiness_deadline,
                "public foreground ee daemon did not publish its socket within 10s"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let stdout = running
            .child
            .stdout
            .take()
            .unwrap_or_else(|| panic!("public foreground ee daemon stdout was not piped"));
        let mut reader = BufReader::new(stdout);
        let mut startup_line = String::new();
        reader
            .read_line(&mut startup_line)
            .unwrap_or_else(|error| panic!("read public daemon startup envelope: {error}"));
        let startup: JsonValue = serde_json::from_str(&startup_line).unwrap_or_else(|error| {
            panic!("public daemon startup emitted invalid JSON: {error}; stdout={startup_line}")
        });
        assert_eq!(
            startup.pointer("/schema").and_then(JsonValue::as_str),
            Some("ee.response.v2")
        );
        assert_eq!(
            startup.pointer("/success").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(
            startup
                .pointer("/data/socketPath")
                .and_then(JsonValue::as_str),
            Some(running.socket_path.to_str().unwrap_or_else(|| panic!(
                "public daemon socket path is not valid UTF-8: {}",
                running.socket_path.display()
            )))
        );
        running
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl Drop for RunningPublicDaemon {
    fn drop(&mut self) {
        let mut request = DaemonRequest::new(
            format!("bench-shutdown-{}", std::process::id()),
            BENCH_AGENT_ID,
            METHOD_SHUTDOWN,
            json!({}),
        );
        request.workspace_id = Some(self.workspace_id.clone());
        let _ = client_round_trip(&self.socket_path, &request);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn stable_workspace_id(workspace: &Path) -> String {
    let hash = blake3::hash(format!("daemon-search-bench:{}", workspace.display()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn resolve_model_fixture_dir(root: &Path) -> PathBuf {
    for candidate in [
        root.to_path_buf(),
        root.join(BUNDLED_EMBEDDING_MODEL_ID),
        root.join("model2vec").join(BUNDLED_EMBEDDING_MODEL_ID),
        root.join("models")
            .join("model2vec")
            .join(BUNDLED_EMBEDDING_MODEL_ID),
    ] {
        if candidate.join("model.safetensors").is_file()
            && candidate.join("tokenizer.json").is_file()
        {
            return std::fs::canonicalize(&candidate).unwrap_or_else(|error| {
                panic!(
                    "canonicalize embedding fixture {}: {error}",
                    candidate.display()
                )
            });
        }
    }
    panic!(
        "EE_EMBED_MODEL_FIXTURE_DIR={} does not contain {BUNDLED_EMBEDDING_MODEL_ID}",
        root.display()
    );
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn configure_hermetic_process(
    command: &mut Command,
    home: &Path,
    xdg_data: &Path,
    model_override: Option<&Path>,
) {
    command
        .env("HOME", home)
        .env("XDG_DATA_HOME", xdg_data)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_NO_COLOR", "1")
        .env_remove("EE_EMBED_MODEL_DIR")
        .env_remove("EE_EMBED_MODEL_FIXTURE_DIR")
        .env_remove("EE_EMBED_MODEL_PATH")
        .env_remove("FRANKENSEARCH_MODEL_DIR")
        .env_remove("FRANKENSEARCH_OFFLINE")
        .env_remove("FRANKENSEARCH_ALLOW_DOWNLOAD")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("FRANKENSEARCH_API_PROVIDER")
        .env_remove("FRANKENSEARCH_API_MODEL")
        .env_remove("FRANKENSEARCH_API_DIMENSION")
        .env_remove("FRANKENSEARCH_API_IDENTITY_JSON");
    if let Some(model_dir) = model_override {
        command.env("EE_EMBED_MODEL_DIR", model_dir);
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn install_local_registry_source_and_read_fingerprint(
    database: &Path,
    workspace_id: &str,
    index: &Path,
    model_dir: &Path,
) -> SearchModelFingerprint {
    let connection = DbConnection::open_file(database)
        .unwrap_or_else(|error| panic!("reopen warm-search fixture database: {error}"));
    let entry = connection
        .find_model_registry_entry(
            workspace_id,
            ModelProvider::Model2Vec,
            BUNDLED_EMBEDDING_MODEL_ID,
            ModelPurpose::Embedding,
        )
        .unwrap_or_else(|error| panic!("read warm-search Model2Vec registry row: {error}"))
        .unwrap_or_else(|| panic!("warm-search rebuild did not register its Model2Vec backend"));
    assert_eq!(entry.status, ModelRegistryStatus::Available);
    assert_eq!(entry.dimension, Some(BUNDLED_EMBEDDING_DIMENSION));
    assert_eq!(entry.distance_metric, Some(ModelDistanceMetric::Cosine));
    assert_eq!(
        entry.version.as_deref(),
        Some(BUNDLED_EMBEDDING_MODEL_REVISION)
    );
    let model_hash = entry
        .content_hash
        .clone()
        .unwrap_or_else(|| panic!("warm-search Model2Vec registry row is missing content_hash"));
    assert!(
        model_hash.starts_with("blake3:") && model_hash.len() == "blake3:".len() + 64,
        "warm-search Model2Vec registry content hash has invalid shape: {model_hash}"
    );
    let model_source = model_dir
        .to_str()
        .unwrap_or_else(|| {
            panic!(
                "warm-search model fixture path is not valid UTF-8: {}",
                model_dir.display()
            )
        })
        .to_owned();
    let updated = connection
        .update_model_registry_entry(
            &entry.id,
            &CreateModelRegistryInput {
                workspace_id: entry.workspace_id.clone(),
                provider: entry.provider,
                model_name: entry.model_name.clone(),
                purpose: entry.purpose,
                dimension: entry.dimension,
                distance_metric: entry.distance_metric,
                status: entry.status,
                version: entry.version.clone(),
                source_uri: Some(model_source.clone()),
                content_hash: entry.content_hash.clone(),
                metadata_json: entry.metadata_json.clone(),
                last_checked_at: entry.last_checked_at.clone(),
            },
        )
        .unwrap_or_else(|error| panic!("install exact local Model2Vec registry source: {error}"));
    assert!(
        updated,
        "warm-search Model2Vec registry row was not updated"
    );
    let installed = connection
        .get_model_registry_entry(&entry.id)
        .unwrap_or_else(|error| panic!("read installed Model2Vec registry row: {error}"))
        .unwrap_or_else(|| panic!("installed Model2Vec registry row disappeared"));
    assert_eq!(installed.source_uri.as_deref(), Some(model_source.as_str()));
    drop(connection);

    let meta_path = index.join("meta.json");
    let meta: JsonValue =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap_or_else(|error| {
            panic!("read index metadata {}: {error}", meta_path.display())
        }))
        .unwrap_or_else(|error| panic!("parse index metadata {}: {error}", meta_path.display()));
    let string_field = |name: &str| {
        meta.get(name)
            .and_then(JsonValue::as_str)
            .unwrap_or_else(|| panic!("index metadata missing string field {name}: {meta}"))
            .to_owned()
    };
    let fingerprint = SearchModelFingerprint {
        model_id: string_field("storedModelId"),
        model_revision: string_field("storedModelRevision"),
        model_hash: string_field("storedModelHash"),
        dimension: meta
            .get("storedDimension")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_else(|| panic!("index metadata missing u32 storedDimension: {meta}")),
        distance_metric: string_field("storedDistanceMetric"),
        vector_dtype: string_field("storedVectorDtype"),
    };
    assert_eq!(fingerprint.model_id, installed.model_name);
    assert_eq!(fingerprint.model_revision, BUNDLED_EMBEDDING_MODEL_REVISION);
    assert_eq!(fingerprint.model_hash, model_hash);
    assert_eq!(fingerprint.dimension, BUNDLED_EMBEDDING_DIMENSION);
    assert_eq!(
        fingerprint.distance_metric,
        ModelDistanceMetric::Cosine.as_str()
    );
    assert_eq!(fingerprint.vector_dtype, "float32");
    fingerprint
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn build_prebuilt_search_fixture(ee_binary: &Path) -> PrebuiltSearchFixture {
    assert!(
        matches!(std::env::var("EE_EMBED_DOWNLOAD").as_deref(), Ok("off")),
        "warm-search gate requires EE_EMBED_DOWNLOAD=off so measurement never performs network I/O"
    );
    let fixture_root = std::env::var_os("EE_EMBED_MODEL_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("EE_EMBED_MODEL_FIXTURE_DIR must name the real model fixture"));
    let fixture_model_dir = resolve_model_fixture_dir(&fixture_root);
    verify_dir_cached(&ModelManifest::potion_128m(), &fixture_model_dir).unwrap_or_else(|error| {
        panic!(
            "warm-search model fixture {} failed frozen manifest verification: {error}",
            fixture_model_dir.display()
        )
    });
    let root =
        TempDir::new().unwrap_or_else(|error| panic!("warm-search fixture tempdir: {error}"));
    let workspace = root.path().join("workspace");
    let ee_dir = workspace.join(".ee");
    std::fs::create_dir_all(&ee_dir)
        .unwrap_or_else(|error| panic!("create warm-search fixture layout: {error}"));
    let workspace = std::fs::canonicalize(&workspace)
        .unwrap_or_else(|error| panic!("canonicalize warm-search workspace: {error}"));
    let database = ee_dir.join("ee.db");
    let index = ee_dir.join("index");
    let home = root.path().join("home");
    let xdg_data = root.path().join("xdg-data");
    std::fs::create_dir_all(&home)
        .unwrap_or_else(|error| panic!("create warm-search HOME: {error}"));
    std::fs::create_dir_all(&xdg_data)
        .unwrap_or_else(|error| panic!("create warm-search XDG_DATA_HOME: {error}"));

    let seed_started = Instant::now();
    let connection = DbConnection::open_file(&database)
        .unwrap_or_else(|error| panic!("open warm-search fixture database: {error}"));
    connection
        .migrate()
        .unwrap_or_else(|error| panic!("migrate warm-search fixture database: {error}"));
    let workspace_id = stable_workspace_id(&workspace);
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("daemon-search-10k".to_owned()),
            },
        )
        .unwrap_or_else(|error| panic!("insert warm-search workspace: {error}"));

    let topics = [
        "release",
        "testing",
        "performance",
        "refactoring",
        "debugging",
        "deployment",
        "security",
        "documentation",
        "graph",
        "search",
    ];
    for document_index in 0..SEARCH_DOCUMENT_COUNT {
        let topic = topics[document_index % topics.len()];
        let content = if document_index == 4_242 {
            format!(
                "{SEARCH_QUERY}. Unique target document {document_index} proves cold and warm result parity."
            )
        } else {
            format!(
                "Daemon search fixture document {document_index}: deterministic {topic} evidence for indexed retrieval latency."
            )
        };
        connection
            .insert_memory(
                &format!("mem_daemon_search_bench_{document_index:05}"),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    content,
                    workflow_id: None,
                    confidence: 0.75,
                    utility: 0.75,
                    importance: 0.75,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("daemon-search-slo-bench".to_owned()),
                    tags: vec![
                        "bench".to_owned(),
                        "daemon-search".to_owned(),
                        topic.to_owned(),
                    ],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap_or_else(|error| {
                panic!("insert warm-search fixture document {document_index}: {error}");
            });
    }
    drop(connection);
    let seed_ms = seed_started.elapsed().as_secs_f64() * 1_000.0;

    let index_started = Instant::now();
    let mut rebuild = Command::new(ee_binary);
    rebuild
        .arg("--workspace")
        .arg(&workspace)
        .arg("--json")
        .arg("index")
        .arg("rebuild")
        .arg("--database")
        .arg(&database)
        .arg("--index-dir")
        .arg(&index);
    configure_hermetic_process(&mut rebuild, &home, &xdg_data, Some(&fixture_model_dir));
    let output = rebuild
        .output()
        .unwrap_or_else(|error| panic!("spawn dedicated ee index rebuild: {error}"));
    assert!(
        output.status.success(),
        "dedicated ee index rebuild failed with {:?}: stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let rebuild_json: JsonValue = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "dedicated ee index rebuild emitted invalid JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        rebuild_json.pointer("/schema").and_then(JsonValue::as_str),
        Some("ee.response.v2")
    );
    assert_eq!(
        rebuild_json
            .pointer("/success")
            .and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(
        rebuild_json
            .pointer("/data/status")
            .and_then(JsonValue::as_str),
        Some("success")
    );
    assert_eq!(
        rebuild_json
            .pointer("/data/memories_indexed")
            .and_then(JsonValue::as_u64),
        Some(u64::try_from(SEARCH_DOCUMENT_COUNT).unwrap_or(u64::MAX))
    );
    assert_eq!(
        rebuild_json
            .pointer("/data/documents_total")
            .and_then(JsonValue::as_u64),
        Some(u64::try_from(SEARCH_DOCUMENT_COUNT).unwrap_or(u64::MAX))
    );
    let fingerprint = install_local_registry_source_and_read_fingerprint(
        &database,
        &workspace_id,
        &index,
        &fixture_model_dir,
    );
    let index_ms = index_started.elapsed().as_secs_f64() * 1_000.0;

    PrebuiltSearchFixture {
        _root: root,
        workspace,
        database,
        index,
        home,
        xdg_data,
        fingerprint,
        seed_ms,
        index_ms,
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
#[derive(Clone, Debug)]
struct SearchObservation {
    wall_ms: f64,
    core_search_ms: f64,
    results: JsonValue,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct DaemonDeepTimingObservation {
    daemon_total_ms: f64,
    embedder_preparation_ms: f64,
    index_open_ms: f64,
    query_ms: f64,
    results: JsonValue,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn required_daemon_timing_ms(result: &JsonValue, field: &str) -> f64 {
    result
        .pointer(&format!("/timing/{field}/elapsedMs"))
        .and_then(JsonValue::as_f64)
        .filter(|elapsed| elapsed.is_finite() && *elapsed >= 0.0)
        .unwrap_or_else(|| panic!("daemon timing field {field} missing or invalid: {result}"))
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_daemon_deep_timing_probe(
    fixture: &PrebuiltSearchFixture,
    daemon_socket: &Path,
    sequence: usize,
) -> DaemonDeepTimingObservation {
    let workspace_id = fixture.workspace.display().to_string();
    let mut request = DaemonRequest::new(
        format!("bench-search-timing-{sequence:03}"),
        BENCH_AGENT_ID,
        METHOD_SEARCH,
        json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V1,
            "query": SEARCH_QUERY,
            "workspacePath": workspace_id,
            "databasePath": fixture.database.display().to_string(),
            "indexDir": fixture.index.display().to_string(),
            "limit": 20,
            "speed": "default",
            "relevanceFloor": 0.0,
            "sourceMode": "hybrid",
            "strictSourceMode": true,
            "memoryScope": "swarm"
        }),
    );
    request.workspace_id = Some(fixture.workspace.display().to_string());
    let response = client_round_trip_before(
        daemon_socket,
        &request,
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap_or_else(|error| panic!("daemon deep-timing round trip failed: {error}"));
    assert!(
        response.error.is_none(),
        "daemon deep-timing search returned an error: {response:?}"
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("daemon deep-timing search omitted its result"));
    DaemonSearchResult::from_value(result.clone())
        .unwrap_or_else(|error| panic!("daemon deep-timing response contract drifted: {error}"));
    assert_eq!(
        result
            .pointer("/response/data/embed_backend")
            .and_then(JsonValue::as_str),
        Some("neural_local")
    );
    assert!(!degradation_present(
        result
            .pointer("/response")
            .unwrap_or_else(|| panic!("daemon deep-timing response missing canonical response")),
        "embed_model_unavailable"
    ));
    let results = result
        .pointer("/response/data/results")
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("daemon deep-timing response missing results"));
    assert!(
        results.iter().any(|search_result| {
            search_result.get("docId").and_then(JsonValue::as_str) == Some(SEARCH_TARGET_MEMORY_ID)
                && search_result
                    .get("fastScore")
                    .and_then(JsonValue::as_f64)
                    .is_some()
        }),
        "daemon deep-timing response omitted the semantic target result"
    );
    DaemonDeepTimingObservation {
        daemon_total_ms: required_daemon_timing_ms(&result, "daemonTotal"),
        embedder_preparation_ms: required_daemon_timing_ms(&result, "embedderPreparation"),
        index_open_ms: required_daemon_timing_ms(&result, "indexOpen"),
        query_ms: required_daemon_timing_ms(&result, "query"),
        results: JsonValue::Array(results.clone()),
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn command_for_search(
    ee_binary: &Path,
    fixture: &PrebuiltSearchFixture,
    daemon_socket: Option<&Path>,
) -> Command {
    let mut command = Command::new(ee_binary);
    command
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--json")
        .arg("search")
        .arg(SEARCH_QUERY)
        .arg("--database")
        .arg(&fixture.database)
        .arg("--index-dir")
        .arg(&fixture.index)
        .arg("--limit")
        .arg("20")
        .arg("--relevance-floor")
        .arg("0")
        .arg("--source-mode")
        .arg("hybrid")
        .arg("--strict-source-mode");
    configure_hermetic_process(&mut command, &fixture.home, &fixture.xdg_data, None);
    if let Some(socket) = daemon_socket {
        command
            .arg("--use-daemon")
            .arg("--daemon-socket")
            .arg(socket);
    }
    command
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn degradation_present(payload: &JsonValue, code: &str) -> bool {
    ["/degraded", "/data/degraded"].iter().any(|pointer| {
        payload
            .pointer(pointer)
            .and_then(JsonValue::as_array)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.get("code").and_then(JsonValue::as_str) == Some(code))
            })
    })
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_search_process(
    ee_binary: &Path,
    fixture: &PrebuiltSearchFixture,
    daemon_socket: Option<&Path>,
) -> SearchObservation {
    let started = Instant::now();
    let output = command_for_search(ee_binary, fixture, daemon_socket)
        .output()
        .unwrap_or_else(|error| panic!("spawn ee search benchmark process: {error}"));
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert!(
        output.status.success(),
        "ee search benchmark failed with {:?}: stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "ee search benchmark emitted invalid JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        );
    });
    assert_eq!(
        payload.pointer("/schema").and_then(JsonValue::as_str),
        Some("ee.response.v2")
    );
    assert_eq!(
        payload.pointer("/success").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(
        payload
            .pointer("/data/embed_backend")
            .and_then(JsonValue::as_str),
        Some("neural_local"),
        "warm-search SLO gate requires the real local neural backend"
    );
    assert_eq!(
        payload
            .pointer("/data/metrics/sourceModeRequested")
            .and_then(JsonValue::as_str),
        Some("hybrid")
    );
    assert_eq!(
        payload
            .pointer("/data/metrics/sourceModeApplied")
            .and_then(JsonValue::as_str),
        Some("hybrid")
    );
    assert!(!degradation_present(&payload, "embed_model_unavailable"));
    assert!(!degradation_present(&payload, "daemon_search_fallback"));

    let results = payload
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("search benchmark response missing data.results"));
    assert!(
        results.iter().any(|result| {
            result.get("docId").and_then(JsonValue::as_str) == Some(SEARCH_TARGET_MEMORY_ID)
        }),
        "search benchmark did not retrieve its unique target document"
    );
    assert!(
        results.iter().any(|result| result
            .get("fastScore")
            .and_then(JsonValue::as_f64)
            .is_some()),
        "neural hybrid search results must carry a semantic fastScore"
    );
    let core_search_ms = payload
        .pointer("/data/elapsedMs")
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("search benchmark response missing data.elapsedMs"));
    SearchObservation {
        wall_ms,
        core_search_ms,
        results: JsonValue::Array(results.clone()),
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn p50(mut samples: Vec<f64>) -> f64 {
    assert_eq!(samples.len(), SEARCH_MEASURE_SAMPLES);
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn assert_result_parity(expected: &JsonValue, observation: &SearchObservation, path: &str) {
    assert_eq!(
        &observation.results, expected,
        "{path} search results drifted from the shared fixture baseline"
    );
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_warm_search_gate() {
    let ee_binary = std::env::var_os("EE_BENCH_EE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("EE_BENCH_EE_BIN must name the release ee binary"));
    assert!(
        ee_binary.is_file(),
        "EE_BENCH_EE_BIN does not name a file: {}",
        ee_binary.display()
    );

    let fixture = build_prebuilt_search_fixture(&ee_binary);
    let daemon_started = Instant::now();
    let daemon = RunningPublicDaemon::start(&ee_binary, &fixture);
    let daemon_start_ms = daemon_started.elapsed().as_secs_f64() * 1_000.0;
    let daemon_initialization =
        run_daemon_deep_timing_probe(&fixture, daemon.socket_path(), usize::MAX);

    let baseline_cold = run_search_process(&ee_binary, &fixture, None);
    let baseline_warm = run_search_process(&ee_binary, &fixture, Some(daemon.socket_path()));
    assert_eq!(
        baseline_cold.results, baseline_warm.results,
        "cold and warm warmup results differ"
    );
    let expected_results = baseline_cold.results;
    for _ in 1..SEARCH_WARMUP_SAMPLES {
        let cold = run_search_process(&ee_binary, &fixture, None);
        let warm = run_search_process(&ee_binary, &fixture, Some(daemon.socket_path()));
        assert_result_parity(&expected_results, &cold, "cold warmup");
        assert_result_parity(&expected_results, &warm, "daemon warmup");
    }

    let mut cold_wall = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut cold_core = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut cold_overhead = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut warm_wall = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut warm_core = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut warm_overhead = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut daemon_total = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut embedder_preparation = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut index_open = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut query = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    for sequence in 0..SEARCH_MEASURE_SAMPLES {
        let cold = run_search_process(&ee_binary, &fixture, None);
        assert_result_parity(&expected_results, &cold, "cold measured");
        cold_wall.push(cold.wall_ms);
        cold_core.push(cold.core_search_ms);
        cold_overhead.push((cold.wall_ms - cold.core_search_ms).max(0.0));

        let warm = run_search_process(&ee_binary, &fixture, Some(daemon.socket_path()));
        assert_result_parity(&expected_results, &warm, "warm measured");
        warm_wall.push(warm.wall_ms);
        warm_core.push(warm.core_search_ms);
        warm_overhead.push((warm.wall_ms - warm.core_search_ms).max(0.0));

        let timing = run_daemon_deep_timing_probe(&fixture, daemon.socket_path(), sequence);
        assert_eq!(
            timing.results, expected_results,
            "daemon deep-timing probe results drifted from the shared fixture baseline"
        );
        daemon_total.push(timing.daemon_total_ms);
        embedder_preparation.push(timing.embedder_preparation_ms);
        index_open.push(timing.index_open_ms);
        query.push(timing.query_ms);
    }

    let cold_p50_ms = p50(cold_wall);
    let cold_core_p50_ms = p50(cold_core);
    let cold_overhead_p50_ms = p50(cold_overhead);
    let warm_p50_ms = p50(warm_wall);
    let warm_core_p50_ms = p50(warm_core);
    let warm_overhead_p50_ms = p50(warm_overhead);
    let daemon_total_p50_ms = p50(daemon_total);
    let embedder_preparation_p50_ms = p50(embedder_preparation);
    let index_open_p50_ms = p50(index_open);
    let query_p50_ms = p50(query);
    let passed = cold_p50_ms < SEARCH_COLD_P50_BUDGET_MS && warm_p50_ms < SEARCH_WARM_P50_BUDGET_MS;

    println!("ee_search_cli_cold_10k_p50_ms={cold_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_p50_ms={warm_p50_ms:.3}");
    println!("ee_search_cli_cold_10k_core_p50_ms={cold_core_p50_ms:.3}");
    println!("ee_search_cli_cold_10k_overhead_p50_ms={cold_overhead_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_core_p50_ms={warm_core_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_overhead_p50_ms={warm_overhead_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_daemon_total_p50_ms={daemon_total_p50_ms:.3}");
    println!(
        "ee_search_daemon_warm_10k_embedder_preparation_p50_ms={embedder_preparation_p50_ms:.3}"
    );
    println!("ee_search_daemon_warm_10k_index_open_p50_ms={index_open_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_query_p50_ms={query_p50_ms:.3}");
    println!(
        "ee_search_daemon_10k_initial_embedder_preparation_ms={:.3}",
        daemon_initialization.embedder_preparation_ms
    );
    println!(
        "ee_search_daemon_10k_fixture_seed_ms={:.3}",
        fixture.seed_ms
    );
    println!(
        "ee_search_daemon_10k_fixture_index_ms={:.3}",
        fixture.index_ms
    );
    println!("ee_search_daemon_10k_daemon_start_ms={daemon_start_ms:.3}");
    println!("ee_search_daemon_10k_samples={SEARCH_MEASURE_SAMPLES}");
    println!("ee_search_daemon_10k_backend=neural_local");
    println!("ee_search_daemon_10k_result_parity=exact_results_array");
    println!(
        "ee_search_daemon_10k_model_id={}",
        fixture.fingerprint.model_id
    );
    println!(
        "ee_search_daemon_10k_model_revision={}",
        fixture.fingerprint.model_revision
    );
    println!(
        "ee_search_daemon_10k_model_hash={}",
        fixture.fingerprint.model_hash
    );
    println!(
        "ee_search_daemon_10k_model_dimension={}",
        fixture.fingerprint.dimension
    );
    println!(
        "ee_search_daemon_10k_model_distance_metric={}",
        fixture.fingerprint.distance_metric
    );
    println!(
        "ee_search_daemon_10k_model_vector_dtype={}",
        fixture.fingerprint.vector_dtype
    );
    println!(
        "ee_search_daemon_10k_result_json={}",
        json!({
            "schema": "ee.perf.daemon_search_slo.v1",
            "fixture": {
                "documents": SEARCH_DOCUMENT_COUNT,
                "query": SEARCH_QUERY,
                "seedMs": fixture.seed_ms,
                "indexMs": fixture.index_ms,
            },
            "backend": {
                "embedBackend": "neural_local",
                "sourceMode": "hybrid",
                "downloads": "disabled",
                "fingerprint": {
                    "modelId": fixture.fingerprint.model_id,
                    "modelRevision": fixture.fingerprint.model_revision,
                    "modelHash": fixture.fingerprint.model_hash,
                    "dimension": fixture.fingerprint.dimension,
                    "distanceMetric": fixture.fingerprint.distance_metric,
                    "vectorDtype": fixture.fingerprint.vector_dtype,
                },
            },
            "samples": {
                "daemonInitializationProbes": 1,
                "warmupPerPath": SEARCH_WARMUP_SAMPLES,
                "measuredPerPath": SEARCH_MEASURE_SAMPLES,
                "deepTimingProbes": SEARCH_MEASURE_SAMPLES,
                "executionOrder": "cold_then_warm_then_deep_timing_probe_interleaved",
            },
            "cold": {
                "process": "fresh_per_sample",
                "p50Ms": cold_p50_ms,
                "budgetMsExclusive": SEARCH_COLD_P50_BUDGET_MS,
                "coreSearchP50Ms": cold_core_p50_ms,
                "processAndRenderOverheadP50Ms": cold_overhead_p50_ms,
            },
            "warm": {
                "clientProcess": "fresh_per_sample",
                "daemonProcess": "one_pre_warmed_process",
                "p50Ms": warm_p50_ms,
                "budgetMsExclusive": SEARCH_WARM_P50_BUDGET_MS,
                "daemonCoreSearchP50Ms": warm_core_p50_ms,
                "clientRpcAndRenderOverheadP50Ms": warm_overhead_p50_ms,
                "daemonTimingProbeP50Ms": {
                    "daemonTotal": daemon_total_p50_ms,
                    "embedderPreparation": embedder_preparation_p50_ms,
                    "indexOpen": index_open_p50_ms,
                    "query": query_p50_ms,
                },
                "initialEmbedderPreparationMs": daemon_initialization.embedder_preparation_ms,
            },
            "resultParity": "exact_results_array",
            "passed": passed,
        })
    );

    assert!(
        cold_p50_ms < SEARCH_COLD_P50_BUDGET_MS,
        "fresh-process cold ee search p50 {cold_p50_ms:.3}ms must be below the exclusive {SEARCH_COLD_P50_BUDGET_MS:.3}ms budget"
    );
    assert!(
        warm_p50_ms < SEARCH_WARM_P50_BUDGET_MS,
        "warm-daemon ee search p50 {warm_p50_ms:.3}ms must be below the exclusive {SEARCH_WARM_P50_BUDGET_MS:.3}ms budget"
    );
}

#[cfg(unix)]
fn main() {
    if std::env::args().any(|argument| argument == "--warm-search-gate") {
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        run_warm_search_gate();
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        panic!("warm-search gate requires safe same-EUID peer credentials on Linux or Apple");
        return;
    }

    let mut criterion = Criterion::default().configure_from_args();
    bench_daemon_round_trip(&mut criterion);
    criterion.final_summary();
}

#[cfg(not(unix))]
fn main() {}
