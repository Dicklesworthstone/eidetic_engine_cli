//! Daemon dispatch microbenchmarks and public cold/warm retrieval acceptance gates.
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
//! `--warm-search-observation` runs the identical workload without asserting
//! the ceilings, so variable GitHub-hosted hardware cannot masquerade as the
//! stable, exclusive-runner closure gate.
//!
//! `--warm-pack-gate` and `--warm-pack-observation` exercise real `ee pack`
//! processes on the same fixture. They retain the first daemon call separately,
//! compare repeated warm calls against fresh in-process calls, require matching
//! selected content/provenance, and report the serving path for every sample.
//! The gate enforces ADR 0052's >= 2x p50 improvement at 10,000 memories.
//! Observations may select 10/100/1,000/10,000 memories with
//! `--fixture-memories N` and mixed admitted/quarantined evidence with
//! `--fixture-evidence N`; a smaller fixture cannot satisfy the 10k gate.
//! Pack fixtures default to repeated neutral/helpful/harmful feedback recorded
//! through the supplied CLI before indexing. `--fixture-feedback empty|neutral|mixed`
//! selects the ledger scenario, whose actual counts are checked and reported.
//! A local fallback must fail the warm serving-path assertion in every scenario;
//! its latency cannot be counted as verified daemon acceleration.
#![allow(clippy::expect_used, reason = "bench assertions fail closed")]
// This file is compiled TWICE and only one of the two compilations has a test
// harness. `Cargo.toml` declares this bench `harness = false`, so when the
// bench target itself is checked (`cargo check --all-targets` enables
// `cfg(test)` for it) the `#[cfg(all(test, ...))] mod pack_benchmark_contracts`
// block compiles, its 23 `#[test]` functions are never collected because there
// is no libtest harness to collect them, and the private fixtures those tests
// call therefore look unreachable. The same module IS harnessed and DOES run
// through `tests/bench_contracts.rs`, which `#[path]`-includes this file along
// with 16 sibling benches, so the fixtures are genuinely used.
//
// Measured 2026-11-05 at 59aa07a13: without this allow, `cargo check --locked
// --all-targets` reports `persistence_fixture`, `insert_fixture_pack`,
// `pack_performance_fixture`, `neutral_feedback_event` and `response` as never
// used — 5 of the 6 warnings that regressed the `-D warnings` lint gate from
// its 2026-10-30 zero-warning baseline. Deleting them would have deleted live
// fixtures for 23 executing tests. `benches/context.rs` and
// `benches/remember.rs` already carry this same file-level allow for the same
// structural reason.
#![allow(
    dead_code,
    reason = "harness = false means the bench compilation never collects this file's #[test] fns, so their fixtures look dead; tests/bench_contracts.rs harnesses them for real"
)]

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::collections::BTreeSet;
#[cfg(unix)]
use std::hint::black_box;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::io::{BufRead, BufReader, Read};
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
use ee::daemon::server::{METHOD_CAPABILITIES, METHOD_SHUTDOWN, client_round_trip};
#[cfg(unix)]
use ee::daemon::server::{METHOD_CONTEXT, METHOD_ECHO, dispatch};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreateModelRegistryInput, CreateSessionInput,
    CreateWorkspaceInput, DbConnection, EvidenceProducerKind, StoredFeedbackEvent,
};
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
const SEARCH_TARGET_MEMORY_ID: &str = "mem_00000000000000000000004242";
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const PACK_MIN_P50_SPEEDUP: f64 = 2.0;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const BENCH_OPERATING_PROFILE: &str = "portable";
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
const BENCH_PACK_PROFILE: &str = "balanced";

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
#[derive(Clone, Copy)]
struct FixtureSize {
    memories: usize,
    evidence: usize,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl Default for FixtureSize {
    fn default() -> Self {
        Self {
            memories: SEARCH_DOCUMENT_COUNT,
            evidence: 0,
        }
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FeedbackScenario {
    Empty,
    Neutral,
    Mixed,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl FeedbackScenario {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Neutral => "neutral",
            Self::Mixed => "mixed",
        }
    }

    const fn signals(self) -> &'static [&'static str] {
        match self {
            Self::Empty => &[],
            Self::Neutral => &["neutral"],
            Self::Mixed => &[
                "neutral", "helpful", "helpful", "helpful", "harmful", "harmful",
            ],
        }
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct RunningDaemon {
    _socket_dir: Option<TempDir>,
    socket_path: PathBuf,
    handle: ee::daemon::server::DaemonServerHandle,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl RunningDaemon {
    fn start() -> Self {
        let socket_dir = tempfile::Builder::new()
            .prefix("ee-b-")
            .tempdir_in("/tmp")
            .unwrap_or_else(|error| panic!("daemon benchmark tempdir: {error}"));
        std::fs::set_permissions(socket_dir.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|error| panic!("secure daemon benchmark tempdir: {error}"));
        let socket_path = socket_dir.path().join("rpc.sock");
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
fn context_refusal_request() -> DaemonRequest {
    let mut request = DaemonRequest::new(
        "bench-context-0001",
        BENCH_AGENT_ID,
        METHOD_CONTEXT,
        json!({
            "task": "measure daemon context validation overhead",
            "maxTokens": 4000,
        }),
    );
    request.workspace_id = Some(BENCH_WORKSPACE_ID.to_owned());
    request
}

#[cfg(unix)]
fn bench_daemon_round_trip(criterion: &mut Criterion) {
    let echo = echo_request();
    let context_refusal = context_refusal_request();
    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("dispatch_echo_disabled_default", |bench| {
        bench.iter(|| {
            let response = dispatch(black_box(&echo));
            black_box(response);
        });
    });

    group.bench_function("dispatch_context_refusal", |bench| {
        bench.iter(|| {
            let response = dispatch(black_box(&context_refusal));
            black_box(response);
        });
    });

    bench_live_socket_round_trip(&mut group, &echo, &context_refusal);
    group.finish();
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn bench_live_socket_round_trip(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    echo: &DaemonRequest,
    context_refusal: &DaemonRequest,
) {
    use ee::daemon::server::client_round_trip;

    let daemon = RunningDaemon::start();
    for (label, request) in [
        ("echo_disabled_default", echo),
        ("context_refusal", context_refusal),
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
    _context_refusal: &DaemonRequest,
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
    size: FixtureSize,
    feedback: JsonValue,
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
    _socket_dir: TempDir,
    socket_path: PathBuf,
    workspace_id: String,
    child: Child,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl RunningPublicDaemon {
    fn start(ee_binary: &Path, fixture: &PrebuiltSearchFixture) -> Self {
        // The server appends a 37-byte atomic-publication suffix. RCH TMPDIR
        // paths can consume sun_path before a measurement starts.
        let socket_dir = tempfile::Builder::new()
            .prefix("ee-b-")
            .tempdir_in("/tmp")
            .unwrap_or_else(|error| panic!("create public daemon socket parent: {error}"));
        std::fs::set_permissions(socket_dir.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|error| panic!("secure public daemon socket parent: {error}"));
        let socket_path = socket_dir.path().join("rpc.sock");
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
            _socket_dir: socket_dir,
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
        // Socket publication is transport readiness, not model/index readiness.
        // Never count a cold-model race or a successful fallback as a warm run.
        let warm_deadline = Instant::now() + Duration::from_secs(120);
        let mut readiness = DaemonRequest::new(
            "bench-warm-readiness",
            BENCH_AGENT_ID,
            METHOD_CAPABILITIES,
            json!({}),
        );
        readiness.workspace_id = Some(running.workspace_id.clone());
        loop {
            let response = client_round_trip(&running.socket_path, &readiness)
                .unwrap_or_else(|error| panic!("probe daemon warm readiness: {error}"));
            let result = response
                .result
                .unwrap_or_else(|| panic!("daemon readiness failed: {:?}", response.error));
            let posture = result.pointer("/warm/posture").and_then(JsonValue::as_str);
            if posture == Some("ready") {
                break;
            }
            assert_eq!(posture, Some("warming"), "daemon cannot warm: {result}");
            assert!(
                Instant::now() < warm_deadline,
                "daemon did not become warm-ready within 120s: {result}"
            );
            assert!(
                running
                    .child
                    .try_wait()
                    .expect("poll warming daemon")
                    .is_none(),
                "daemon exited while warming"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
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
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        // EE_PROFILE selects the pack profile, not the operating profile.
        // The latter is fixed in the fixture's [profile].selected config.
        .env("EE_PROFILE", BENCH_PACK_PROFILE)
        .env("EE_EMBED_BACKEND", "local")
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_DAEMON_WARM", "on")
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
fn assert_fixed_operating_profile(payload: &JsonValue) {
    assert_eq!(
        payload
            .pointer("/data/profileRuntime/activeProfile")
            .and_then(JsonValue::as_str),
        Some(BENCH_OPERATING_PROFILE),
        "benchmark operating profile drifted: {payload}"
    );
    assert_eq!(
        payload
            .pointer("/data/profileRuntime/source")
            .and_then(JsonValue::as_str),
        Some("workspace_config"),
        "benchmark must not select budgets from a changing host probe: {payload}"
    );
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
fn build_prebuilt_search_fixture(
    ee_binary: &Path,
    size: FixtureSize,
    feedback_scenario: FeedbackScenario,
) -> PrebuiltSearchFixture {
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
    std::fs::write(
        ee_dir.join("config.toml"),
        format!("[profile]\nselected = \"{BENCH_OPERATING_PROFILE}\"\n"),
    )
    .unwrap_or_else(|error| panic!("pin fixture operating profile: {error}"));
    let workspace = std::fs::canonicalize(&workspace)
        .unwrap_or_else(|error| panic!("canonicalize warm-search workspace: {error}"));
    let database = ee_dir.join("ee.db");
    let index = ee_dir.join("index");
    let home = root.path().join("home");
    let xdg_data = root.path().join("xdg-data");
    std::fs::create_dir_all(&home)
        .unwrap_or_else(|error| panic!("create warm-search HOME: {error}"));
    for directory in [home.join(".config"), home.join(".cache")] {
        std::fs::create_dir_all(directory)
            .unwrap_or_else(|error| panic!("create private fixture XDG directory: {error}"));
    }
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
                name: Some(format!("daemon-retrieval-{}", size.memories)),
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
    assert!(
        size.memories > 0,
        "the retrieval fixture must contain memories"
    );
    let target_index = 4_242.min(size.memories - 1);
    for document_index in 0..size.memories {
        let topic = topics[document_index % topics.len()];
        let content = if document_index == target_index {
            format!(
                "{SEARCH_QUERY}. Unique target document {document_index} proves cold and warm result parity."
            )
        } else {
            format!(
                "Daemon search fixture document {document_index}: deterministic {topic} evidence for indexed retrieval latency."
            )
        };
        let memory_id = if document_index == target_index {
            SEARCH_TARGET_MEMORY_ID.to_owned()
        } else {
            format!("mem_{document_index:026}")
        };
        connection
            .insert_memory(
                &memory_id,
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
    seed_evidence_fixture(&connection, &workspace_id, size.evidence);
    assert_eq!(
        connection
            .feedback_events_fingerprint(&workspace_id)
            .expect("inspect fixture feedback ledger")
            .count,
        0,
        "feedback fixture must start from an empty ledger"
    );
    drop(connection);
    let feedback = seed_feedback_fixture(
        ee_binary,
        &workspace,
        &database,
        &home,
        &xdg_data,
        size.memories,
        feedback_scenario,
    );
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
    assert_fixed_operating_profile(&rebuild_json);
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
        Some(u64::try_from(size.memories).unwrap_or(u64::MAX))
    );
    if size.evidence == 0 {
        assert_eq!(
            rebuild_json
                .pointer("/data/documents_total")
                .and_then(JsonValue::as_u64),
            Some(u64::try_from(size.memories).unwrap_or(u64::MAX))
        );
    } else {
        assert_eq!(
            rebuild_json
                .pointer("/data/evidence_indexed")
                .and_then(JsonValue::as_u64),
            Some(u64::try_from(size.evidence.div_ceil(5)).unwrap_or(u64::MAX)),
            "evidence fixture must retain the intended admitted/quarantined split"
        );
    }
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
        size,
        feedback,
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn feedback_target(signal: &str, memories: usize) -> String {
    if signal == "harmful" && memories > 1 {
        // Keep the planted positive result useful while penalizing a distractor.
        format!("mem_{:026}", 0)
    } else {
        SEARCH_TARGET_MEMORY_ID.to_owned()
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn assert_feedback_fixture(
    events: &[StoredFeedbackEvent],
    workspace_id: &str,
    memories: usize,
    scenario: FeedbackScenario,
) {
    assert_eq!(
        events.len(),
        scenario.signals().len(),
        "feedback fixture event count drift"
    );
    for (index, signal) in scenario.signals().iter().enumerate() {
        let id = format!("fb_{index:026}");
        let event = events
            .iter()
            .find(|event| event.id == id)
            .unwrap_or_else(|| panic!("feedback fixture omitted canonical event {id}"));
        assert_eq!(event.workspace_id, workspace_id);
        assert_eq!(event.target_type, "memory");
        assert_eq!(event.target_id, feedback_target(signal, memories));
        assert_eq!(event.signal, *signal, "feedback fixture signal drift");
        assert_eq!(event.source_type, "outcome_observed");
        assert_eq!(
            event.source_id,
            Some(format!("daemon-bench-feedback-{index}"))
        );
        assert!(
            event.applied_at.is_some(),
            "feedback fixture event was not applied"
        );
        assert!(event.weight.is_finite() && event.weight >= 0.0);
        if *signal != "neutral" {
            assert!(
                event.weight > 0.0,
                "scoring feedback must have positive weight"
            );
        }
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn seed_feedback_fixture(
    ee_binary: &Path,
    workspace: &Path,
    database: &Path,
    home: &Path,
    xdg_data: &Path,
    memories: usize,
    scenario: FeedbackScenario,
) -> JsonValue {
    let started = Instant::now();
    let mut confidence_changes = 0;
    for (index, signal) in scenario.signals().iter().enumerate() {
        let mut command = Command::new(ee_binary);
        command
            .arg("--workspace")
            .arg(workspace)
            .arg("--json")
            .arg("outcome")
            .arg(feedback_target(signal, memories))
            .arg("--database")
            .arg(database)
            .arg("--signal")
            .arg(signal)
            .arg("--event-id")
            .arg(format!("fb_{index:026}"))
            .arg("--source-id")
            .arg(format!("daemon-bench-feedback-{index}"))
            .arg("--reason")
            .arg("Daemon pack feedback benchmark fixture");
        configure_hermetic_process(&mut command, home, xdg_data, None);
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("seed canonical feedback: {error}"));
        assert!(
            output.status.success(),
            "feedback seed failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let payload: JsonValue = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("feedback seed JSON: {error}"));
        assert_eq!(payload["success"], true);
        assert_eq!(
            payload.pointer("/data/status").and_then(JsonValue::as_str),
            Some("recorded"),
            "quarantined, duplicate or dry-run feedback cannot seed the benchmark: {payload}"
        );
        assert_eq!(payload.pointer("/data/target/verified"), Some(&json!(true)));
        let before = payload
            .pointer("/data/confidence/before")
            .and_then(JsonValue::as_f64)
            .unwrap_or_else(|| panic!("feedback omitted prior confidence: {payload}"));
        let after = payload
            .pointer("/data/confidence/after")
            .and_then(JsonValue::as_f64)
            .unwrap_or_else(|| panic!("feedback omitted updated confidence: {payload}"));
        let confidence_changed = before.to_bits() != after.to_bits();
        match *signal {
            "neutral" => assert!(
                !confidence_changed,
                "neutral fixture feedback changed confidence"
            ),
            "helpful" => assert!(
                after > before,
                "helpful fixture feedback did not improve confidence"
            ),
            "harmful" => assert!(
                after < before,
                "harmful fixture feedback did not reduce confidence"
            ),
            _ => unreachable!("fixture signals are closed"),
        }
        confidence_changes += usize::from(confidence_changed);
    }
    // Validate actual applied rows, including identities and signal payloads;
    // the reported count is fixture metadata, never a freshness witness.
    let connection = DbConnection::open_file_read_only(database)
        .unwrap_or_else(|error| panic!("inspect feedback fixture: {error}"));
    let workspace_id = stable_workspace_id(workspace);
    let events = connection
        .list_feedback_events(&workspace_id)
        .unwrap_or_else(|error| panic!("read seeded feedback ledger: {error}"));
    assert_feedback_fixture(&events, &workspace_id, memories, scenario);
    let count_signal = |signal: &str| events.iter().filter(|event| event.signal == signal).count();
    json!({
        "scenario": scenario.as_str(), "events": events.len(),
        "neutral": count_signal("neutral"), "helpful": count_signal("helpful"),
        "harmful": count_signal("harmful"), "confidenceChanges": confidence_changes,
        "seedMs": started.elapsed().as_secs_f64() * 1_000.0,
        "recordedBy": "public_cli_outcome_before_indexing",
    })
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn seed_evidence_fixture(connection: &DbConnection, workspace_id: &str, count: usize) {
    if count == 0 {
        return;
    }
    let session_id = "sess_00000000000000000000000001";
    connection
        .insert_session(
            session_id,
            &CreateSessionInput {
                workspace_id: workspace_id.to_owned(),
                cass_session_id: session_id.to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: u32::try_from(count).expect("evidence fixture count fits u32"),
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(session_id.as_bytes()).to_hex()),
                metadata_json: None,
            },
        )
        .unwrap_or_else(|error| panic!("seed evidence fixture session: {error}"));
    for index in 0..count {
        let admitted = index % 5 == 0;
        let line = u32::try_from(index)
            .expect("evidence line fits u32")
            .checked_add(1)
            .expect("evidence line is one-based and fits u32");
        let excerpt = format!(
            "Daemon fixture source {index}: release verification retains complete provenance."
        );
        connection
            .insert_evidence_span(
                &ee::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(index as u128 + 1))
                    .to_string(),
                &CreateEvidenceSpanInput {
                    workspace_id: workspace_id.to_owned(),
                    session_id: session_id.to_owned(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("daemon-bench-span-{index}"),
                    span_kind: "message".to_owned(),
                    start_line: line,
                    end_line: line,
                    start_byte: None,
                    end_byte: None,
                    role: Some(if admitted { "assistant" } else { "tool" }.to_owned()),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                    excerpt,
                    metadata_json: Some(
                        r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.to_owned(),
                    ),
                    inherited_redaction_classes: Vec::new(),
                },
            )
            .unwrap_or_else(|error| panic!("seed evidence fixture span {index}: {error}"));
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
fn required_public_daemon_timing_ms(payload: &JsonValue, field: &str) -> f64 {
    payload
        .pointer(&format!("/data/daemonTiming/{field}/elapsedMs"))
        .and_then(JsonValue::as_f64)
        .filter(|elapsed| elapsed.is_finite() && *elapsed >= 0.0)
        .unwrap_or_else(|| {
            panic!("public daemon timing field {field} missing or invalid: {payload}")
        })
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_daemon_deep_timing_probe(
    ee_binary: &Path,
    fixture: &PrebuiltSearchFixture,
    daemon_socket: &Path,
    sequence: usize,
) -> DaemonDeepTimingObservation {
    let output = command_for_search(ee_binary, fixture, Some(daemon_socket))
        .arg("--explain-performance")
        .output()
        .unwrap_or_else(|error| {
            panic!("spawn public daemon deep-timing search {sequence}: {error}")
        });
    assert!(
        output.status.success(),
        "public daemon deep-timing search {sequence} failed with {:?}: stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "public daemon deep-timing search {sequence} emitted invalid JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        payload.pointer("/schema").and_then(JsonValue::as_str),
        Some("ee.explain.performance.v1")
    );
    assert_fixed_operating_profile(&payload);
    assert_eq!(
        payload.pointer("/success").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(
        payload
            .pointer("/data/executionPath")
            .and_then(JsonValue::as_str),
        Some("daemon")
    );
    assert_eq!(
        payload
            .pointer("/data/embedBackend")
            .and_then(JsonValue::as_str),
        Some("neural_local")
    );
    let fallbacks = payload
        .pointer("/data/fallbacks")
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("public daemon deep-timing response missing fallbacks"));
    assert!(
        !fallbacks
            .iter()
            .any(|fallback| fallback.get("code").and_then(JsonValue::as_str)
                == Some("daemon_search_fallback")),
        "public daemon deep-timing search fell back in-process: {payload}"
    );
    let returned_hits = payload
        .pointer("/data/search/returnedHits")
        .and_then(JsonValue::as_u64)
        .unwrap_or_else(|| panic!("public daemon deep-timing response omitted returnedHits"));
    assert!(
        returned_hits > 0,
        "public daemon timing probe returned no hits"
    );
    // The performance envelope is deliberately content-redacted. Exercise the
    // same public CLI path once more without the diagnostic projection so the
    // benchmark keeps its exact result-array parity assertion.
    let rendered = run_search_process(ee_binary, fixture, Some(daemon_socket));
    assert_eq!(
        returned_hits,
        rendered
            .results
            .as_array()
            .map_or(0, |results| results.len() as u64),
        "public daemon timing hit count drifted from its canonical rendering"
    );
    DaemonDeepTimingObservation {
        daemon_total_ms: required_public_daemon_timing_ms(&payload, "daemonTotal"),
        embedder_preparation_ms: required_public_daemon_timing_ms(&payload, "embedderPreparation"),
        index_open_ms: required_public_daemon_timing_ms(&payload, "indexOpen"),
        query_ms: required_public_daemon_timing_ms(&payload, "query"),
        results: rendered.results,
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
    ["/degraded", "/data/degraded", "/data/fallbacks"]
        .iter()
        .any(|pointer| {
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
    assert_fixed_operating_profile(&payload);
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
    assert_eq!(
        payload
            .pointer("/data/servingPath")
            .and_then(JsonValue::as_str),
        Some(if daemon_socket.is_some() {
            "daemon"
        } else {
            "in_process"
        }),
        "search measurement must prove its serving path"
    );
    assert_eq!(
        payload.pointer("/data/fallbackReason"),
        Some(&JsonValue::Null)
    );

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
fn measured_cli_artifact(ee_binary: &Path) -> JsonValue {
    let path = std::fs::canonicalize(ee_binary)
        .unwrap_or_else(|error| panic!("resolve measured CLI {}: {error}", ee_binary.display()));
    let mut file = std::fs::File::open(&path)
        .unwrap_or_else(|error| panic!("open measured CLI {}: {error}", path.display()));
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size_bytes = 0_u64;
    loop {
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("hash measured CLI {}: {error}", path.display()),
        };
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size_bytes += count as u64;
    }
    json!({
        "path": path.to_string_lossy(),
        "contentHash": format!("blake3:{}", hasher.finalize().to_hex()),
        "sizeBytes": size_bytes,
    })
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_warm_search_measurement(enforce_budgets: bool) {
    let ee_binary = std::env::var_os("EE_BENCH_EE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("EE_BENCH_EE_BIN must name the release ee binary"));
    assert!(
        ee_binary.is_file(),
        "EE_BENCH_EE_BIN does not name a file: {}",
        ee_binary.display()
    );
    // The external CLI can differ from the benchmark's source revision. Hash
    // its actual bytes once, before any fixture setup or timed samples.
    let cli_artifact = measured_cli_artifact(&ee_binary);

    let fixture =
        build_prebuilt_search_fixture(&ee_binary, FixtureSize::default(), FeedbackScenario::Empty);
    let daemon_started = Instant::now();
    let daemon = RunningPublicDaemon::start(&ee_binary, &fixture);
    let daemon_start_ms = daemon_started.elapsed().as_secs_f64() * 1_000.0;
    let daemon_initialization =
        run_daemon_deep_timing_probe(&ee_binary, &fixture, daemon.socket_path(), usize::MAX);

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

        let timing =
            run_daemon_deep_timing_probe(&ee_binary, &fixture, daemon.socket_path(), sequence);
        assert_eq!(
            timing.results, expected_results,
            "public daemon timing probe results drifted from the shared fixture baseline"
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
            "cliArtifact": cli_artifact,
            "operatingProfile": BENCH_OPERATING_PROFILE,
            "operatingProfileSource": "workspace_config",
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
            "budgetEnforcement": if enforce_budgets {
                "hard_gate"
            } else {
                "observation_only"
            },
            "passed": passed,
        })
    );

    if enforce_budgets {
        assert!(
            cold_p50_ms < SEARCH_COLD_P50_BUDGET_MS,
            "fresh-process cold ee search p50 {cold_p50_ms:.3}ms must be below the exclusive {SEARCH_COLD_P50_BUDGET_MS:.3}ms budget"
        );
        assert!(
            warm_p50_ms < SEARCH_WARM_P50_BUDGET_MS,
            "warm-daemon ee search p50 {warm_p50_ms:.3}ms must be below the exclusive {SEARCH_WARM_P50_BUDGET_MS:.3}ms budget"
        );
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct PackObservation {
    wall_ms: f64,
    serving_path: String,
    signature: JsonValue,
    hash: JsonValue,
    record_id: Option<String>,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
struct PackPersistenceVerifier {
    database: PathBuf,
    workspace_id: String,
    record_ids: BTreeSet<String>,
    calls: usize,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn fixture_pack_record_ids(connection: &DbConnection, workspace_id: &str) -> BTreeSet<String> {
    let count = connection
        .count_table_rows("pack_records")
        .expect("count benchmark pack records");
    let limit = u32::try_from(count).expect("fixture pack count fits u32");
    let ids: BTreeSet<_> = connection
        .list_recent_pack_record_ids_for_workspace(workspace_id, limit)
        .expect("inspect benchmark pack record identities")
        .into_iter()
        .collect();
    assert_eq!(
        ids.len(),
        usize::try_from(count).expect("fixture pack count fits usize"),
        "benchmark pack records must all belong to the fixture workspace"
    );
    ids
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn verified_pack_record_delta(
    previous: &BTreeSet<String>,
    current: &BTreeSet<String>,
    read_only: bool,
) -> Option<String> {
    assert!(
        previous.is_subset(current),
        "a pack call removed or replaced an existing persisted record"
    );
    assert_eq!(
        current.len() - previous.len(),
        usize::from(!read_only),
        "each persisted pack call must add exactly one record; read-only calls must add none"
    );
    current.difference(previous).next().cloned()
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn assert_pack_persistence_not_degraded(payload: &JsonValue) {
    assert!(
        !degradation_present(payload, "context_pack_persist_failed"),
        "failed pack persistence cannot satisfy a persisted benchmark: {payload}"
    );
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn assert_persisted_selection_matches_response(ledger: &JsonValue, signature: &JsonValue) {
    let mut expected: Vec<_> = signature["items"]
        .as_array()
        .expect("signature items")
        .iter()
        .map(|item| {
            let kind = item.get("entityKind").and_then(JsonValue::as_str).unwrap_or("memory");
            let id_field = match kind {
                "memory" => "memoryId",
                "rule" => "ruleId",
                "evidence_span" => "evidenceSpanId",
                _ => panic!("unknown pack entity kind: {kind}"),
            };
            // Persistence hashes precisely the canonical uri/note entries.
            // Render-only labels and locators are not stored ledger inputs.
            // Keep the redacted output bytes instead of reparsing URIs.
            let entries: Vec<_> = item["provenance"]
                .as_array()
                .expect("response provenance entries")
                .iter()
                .map(|source| json!({
                    "uri": source["uri"].as_str().expect("provenance URI"),
                    "note": source["note"].as_str().expect("provenance note"),
                }))
                .collect();
            let provenance = json!({
                "schema": ee::pack::PACK_ITEM_PROVENANCE_SCHEMA_V1,
                "entries": entries,
            })
            .to_string();
            json!({
                "entityKind": kind,
                "entityId": item[id_field].as_str().expect("typed pack entity identity"),
                "entityRevision": item.get("entityRevision").unwrap_or(&JsonValue::Null),
                "rank": item["rank"], "section": item["section"],
                "estimatedTokens": item["estimatedTokens"],
                "trustClass": item["trust"]["class"],
                "trustSubclass": item["trust"].get("subclass").unwrap_or(&JsonValue::Null),
                "provenanceHash": format!("blake3:{}", blake3::hash(provenance.as_bytes()).to_hex()),
            })
        })
        .collect();
    // The public output groups native entity kinds; the durable ledger orders
    // them by the shared selection rank. Compare that semantic order without
    // weakening the separate cold/warm response-array order assertion.
    expected.sort_by_key(|item| item["rank"].as_u64().expect("response selection rank"));
    let actual: Vec<_> = ee::db::pack_ledger_core_array(ledger, "selectedItems")
        .expect("validated ledger selected items")
        .iter()
        .map(|item| {
            json!({
                "entityKind": item["entityKind"], "entityId": item["entityId"],
                "entityRevision": item.get("entityRevision").unwrap_or(&JsonValue::Null),
                "rank": item["rank"], "section": item["section"],
                "estimatedTokens": item["estimatedTokens"],
                "trustClass": item["trustClass"], "trustSubclass": item["trustSubclass"],
                "provenanceHash": item["provenance"]["hash"],
            })
        })
        .collect();
    assert_eq!(
        actual, expected,
        "persisted selection must match the response's typed entities, ranks, trust and provenance"
    );
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
impl PackPersistenceVerifier {
    fn new(database: &Path, workspace_id: String) -> Self {
        let connection = DbConnection::open_file_read_only(database)
            .expect("inspect initial benchmark pack ledger");
        let record_ids = fixture_pack_record_ids(&connection, &workspace_id);
        assert!(
            record_ids.is_empty(),
            "fixture must start without pack records"
        );
        Self {
            database: database.to_path_buf(),
            workspace_id,
            record_ids,
            calls: 0,
        }
    }

    // Reopen after the child exits, outside the measured wall interval. A
    // successful response is insufficient: canonical pack deliberately returns
    // usable content when persistence fails. Checking every transition also
    // catches a missing write followed by a duplicate with the same pack hash.
    fn verify(
        &mut self,
        payload: &JsonValue,
        signature: &JsonValue,
        read_only: bool,
    ) -> Option<String> {
        assert_pack_persistence_not_degraded(payload);
        let response_hash = if read_only {
            None
        } else {
            Some(
                payload
                    .pointer("/data/pack/hash")
                    .and_then(JsonValue::as_str)
                    .expect("persisted pack response must expose its hash"),
            )
        };
        self.verify_record(signature, read_only, response_hash)
    }

    // Performance output deliberately omits content and the pack hash. Its
    // new, integrity-checked ledger must still match the ordinary-call
    // baseline; never substitute a fabricated response for that evidence.
    fn verify_diagnostic(
        &mut self,
        payload: &JsonValue,
        signature: &JsonValue,
        read_only: bool,
    ) -> Option<String> {
        assert_pack_persistence_not_degraded(payload);
        self.verify_record(signature, read_only, None)
    }

    fn verify_record(
        &mut self,
        signature: &JsonValue,
        read_only: bool,
        response_hash: Option<&str>,
    ) -> Option<String> {
        let connection = DbConnection::open_file_read_only(&self.database)
            .expect("inspect completed benchmark pack ledger");
        let current = fixture_pack_record_ids(&connection, &self.workspace_id);
        let new_id = verified_pack_record_delta(&self.record_ids, &current, read_only);
        if let Some(id) = &new_id {
            let record = connection
                .get_pack_record(id)
                .expect("read benchmark persisted pack")
                .expect("new benchmark pack record must exist");
            assert_eq!(record.workspace_id, self.workspace_id);
            assert_eq!(record.query, SEARCH_QUERY);
            assert_eq!(record.profile, BENCH_PACK_PROFILE);
            if let Some(response_hash) = response_hash {
                assert_eq!(
                    record.pack_hash, response_hash,
                    "persisted pack hash must identify this invocation's response"
                );
            }
            assert_eq!(json!(record.max_tokens), signature["maxTokens"]);
            assert_eq!(json!(record.used_tokens), signature["usedTokens"]);
            assert_eq!(
                record.item_count as usize,
                signature["items"]
                    .as_array()
                    .expect("signature items")
                    .len(),
                "persisted pack must retain every selected entity"
            );
            let ledger = ee::db::parse_stored_pack_ledger(&record);
            assert!(
                ledger.available_ledger().is_some(),
                "persisted pack must have an integrity-checked, record-bound selection ledger: {:?}",
                ledger.degraded
            );
            assert_persisted_selection_matches_response(
                ledger
                    .available_ledger()
                    .expect("validated benchmark selection ledger"),
                signature,
            );
        }
        self.record_ids = current;
        self.calls += 1;
        new_id
    }
}

/// The pack record ID, timestamps, timing diagnostics and time-derived scores
/// can vary across ordinary persisted calls. Compare the actual selected
/// entities, order, content, trust and provenance, plus the enforced budget.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn pack_content_signature(payload: &JsonValue) -> JsonValue {
    let items = payload
        .pointer("/data/pack/items")
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("pack response omitted its items: {payload}"));
    assert!(
        !items.is_empty(),
        "empty packs cannot prove retrieval parity"
    );
    let selected: Vec<_> = items
        .iter()
        .map(|item| {
            for field in [
                "rank",
                "section",
                "content",
                "estimatedTokens",
                "trust",
                "provenance",
                "selectedIn",
            ] {
                assert!(
                    item.get(field).is_some(),
                    "pack item omitted canonical field {field}: {item}"
                );
            }
            let mut selected = serde_json::Map::new();
            for field in [
                "rank",
                "memoryId",
                "entityKind",
                "ruleId",
                "evidenceSpanId",
                "entityRevision",
                "sessionId",
                "startLine",
                "endLine",
                "section",
                "content",
                "estimatedTokens",
                "trust",
                "provenance",
                "selectedIn",
                "origin",
                "redactions",
            ] {
                if let Some(value) = item.get(field) {
                    selected.insert(field.to_owned(), value.clone());
                }
            }
            assert!(selected.contains_key("content") && selected.contains_key("rank"));
            JsonValue::Object(selected)
        })
        .collect();
    json!({
        "items": selected,
        "maxTokens": payload.pointer("/data/pack/budget/maxTokens").expect("pack maxTokens"),
        "usedTokens": payload.pointer("/data/pack/budget/usedTokens").expect("pack usedTokens"),
    })
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn verified_pack_serving_path(payload: &JsonValue, daemon_expected: bool) -> &str {
    let serving_path = payload
        .pointer("/data/servingPath")
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("pack benchmark cannot establish serving path: {payload}"));
    if daemon_expected {
        assert!(
            matches!(serving_path, "daemon" | "hybrid"),
            "warm pack used {serving_path}: {payload}"
        );
        assert_eq!(
            payload
                .pointer("/data/retrievalPath")
                .and_then(JsonValue::as_str),
            Some("daemon")
        );
    } else {
        assert_eq!(serving_path, "in_process");
        assert_eq!(
            payload
                .pointer("/data/retrievalPath")
                .and_then(JsonValue::as_str),
            Some("in_process")
        );
    }
    assert_eq!(
        payload.pointer("/data/fallbackReason"),
        Some(&JsonValue::Null)
    );
    serving_path
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn command_for_pack(
    ee_binary: &Path,
    fixture: &PrebuiltSearchFixture,
    daemon_socket: Option<&Path>,
    read_only: bool,
) -> Command {
    let mut command = Command::new(ee_binary);
    command
        .arg("--workspace")
        .arg(&fixture.workspace)
        .arg("--json")
        .arg("pack")
        .arg(SEARCH_QUERY)
        .arg("--database")
        .arg(&fixture.database)
        .arg("--index-dir")
        .arg(&fixture.index)
        .arg("--max-tokens")
        .arg("400")
        .arg("--candidate-pool")
        .arg("100")
        .arg("--source-mode")
        .arg("hybrid")
        .arg("--strict-source-mode")
        .arg("--relevance-floor")
        .arg("0");
    configure_hermetic_process(&mut command, &fixture.home, &fixture.xdg_data, None);
    if let Some(socket) = daemon_socket {
        command
            .arg("--use-daemon")
            .arg("--daemon-socket")
            .arg(socket);
    }
    if read_only {
        command.arg("--read-only");
    }
    command
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_pack_process(
    ee_binary: &Path,
    fixture: &PrebuiltSearchFixture,
    daemon_socket: Option<&Path>,
    read_only: bool,
    persistence: &mut PackPersistenceVerifier,
) -> PackObservation {
    let mut command = command_for_pack(ee_binary, fixture, daemon_socket, read_only);
    let started = Instant::now();
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn ee pack benchmark: {error}"));
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert!(
        output.status.success(),
        "ee pack benchmark failed with {:?}: stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid pack JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(payload["schema"], "ee.response.v2");
    assert_eq!(payload["success"], true);
    assert_eq!(
        payload
            .pointer("/data/request/profile")
            .and_then(JsonValue::as_str),
        Some(BENCH_PACK_PROFILE),
        "pack benchmark profile drifted: {payload}"
    );
    assert_eq!(
        payload
            .pointer("/data/embed_backend")
            .and_then(JsonValue::as_str),
        Some("neural_local")
    );
    assert!(!degradation_present(&payload, "embed_model_unavailable"));
    assert!(!degradation_present(&payload, "daemon_search_fallback"));
    let serving_path = verified_pack_serving_path(&payload, daemon_socket.is_some());
    let signature = pack_content_signature(&payload);
    assert!(
        signature["items"]
            .as_array()
            .expect("signature items")
            .iter()
            .any(|item| item["memoryId"].as_str() == Some(SEARCH_TARGET_MEMORY_ID)),
        "pack benchmark must select the planted relevant memory: {signature}"
    );
    let record_id = persistence.verify(&payload, &signature, read_only);
    PackObservation {
        wall_ms,
        serving_path: serving_path.to_owned(),
        signature,
        hash: payload
            .pointer("/data/pack/hash")
            .cloned()
            .unwrap_or(JsonValue::Null),
        record_id,
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn required_pack_phase_ms(payload: &JsonValue, name: &str) -> f64 {
    let matches: Vec<_> = payload
        .pointer("/data/timings")
        .and_then(JsonValue::as_array)
        .expect("pack performance report must expose phase timings")
        .iter()
        .filter(|timing| timing["name"].as_str() == Some(name))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "pack performance report must expose one accumulated {name} timing"
    );
    let elapsed_ms = matches[0]["elapsedMs"]
        .as_f64()
        .unwrap_or_else(|| panic!("pack performance phase {name} must expose elapsedMs"));
    assert!(
        elapsed_ms.is_finite() && elapsed_ms >= 0.0,
        "pack performance phase {name} has invalid elapsedMs"
    );
    elapsed_ms
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn assert_pack_performance_report(
    payload: &JsonValue,
    signature: &JsonValue,
    daemon_expected: bool,
    read_only: bool,
) {
    assert_eq!(payload["schema"], "ee.explain.performance.v1");
    assert_eq!(payload["success"], true);
    assert_fixed_operating_profile(payload);
    let _ = verified_pack_serving_path(payload, daemon_expected);
    assert_eq!(payload.pointer("/data/command"), Some(&json!("pack")));
    assert_eq!(
        payload.pointer("/data/queryPlan/profile"),
        Some(&json!(BENCH_PACK_PROFILE))
    );
    assert_eq!(
        payload.pointer("/data/queryPlan/requestedCandidatePool"),
        Some(&json!(100))
    );
    assert_eq!(
        payload.pointer("/data/queryPlan/maxTokens"),
        Some(&signature["maxTokens"])
    );
    for field in ["sourceModeRequested", "sourceModeApplied"] {
        assert_eq!(
            payload.pointer(&format!("/data/queryPlan/{field}")),
            Some(&json!("hybrid")),
            "pack performance probe must retain hybrid retrieval"
        );
    }
    assert_eq!(
        payload.pointer("/data/queryPlan/strictSourceMode"),
        Some(&json!(true))
    );
    assert_eq!(
        payload.pointer("/data/queryPlan/fallbackApplied"),
        Some(&json!(false))
    );
    // Verify the backend that actually produced this probe's search report,
    // independently of the preceding ordinary calls and requested speed.
    assert_eq!(
        payload.pointer("/data/search/embedBackend"),
        Some(&json!("neural_local"))
    );
    assert_eq!(
        payload.pointer("/data/search/usesEmbeddings"),
        Some(&json!(true))
    );
    assert!(
        payload
            .pointer("/data/fallbacks")
            .is_some_and(JsonValue::is_array),
        "pack performance probe must report degradations"
    );
    assert!(!degradation_present(payload, "embed_model_unavailable"));
    assert!(!degradation_present(payload, "daemon_search_fallback"));
    assert_pack_persistence_not_degraded(payload);
    assert_eq!(
        payload.pointer("/data/redaction/memoryContentIncluded"),
        Some(&json!(false))
    );
    assert_eq!(
        payload.pointer("/data/redaction/queryTextIncluded"),
        Some(&json!(false))
    );
    for name in [
        "requestValidate",
        "dbOpen",
        "search",
        "candidateResolution",
        "packAssembly",
        "packPersistence",
        "total",
    ] {
        let _ = required_pack_phase_ms(payload, name);
    }
    if daemon_expected {
        let _ = required_pack_phase_ms(payload, "daemonRetrieval");
        let _ = required_pack_phase_ms(payload, "daemonHandoffValidation");
    } else {
        let _ = required_pack_phase_ms(payload, "embedderPrepare");
    }
    let selected = signature["items"]
        .as_array()
        .expect("baseline pack items")
        .len();
    assert_eq!(
        payload.pointer("/data/pack/hashPresent"),
        Some(&json!(true))
    );
    assert_eq!(
        payload.pointer("/data/pack/selectedCount"),
        Some(&json!(selected)),
        "pack performance probe must retain the baseline selection count"
    );
    assert_eq!(
        payload.pointer("/data/pack/tokenBudget/limit"),
        Some(&signature["maxTokens"])
    );
    assert_eq!(
        payload.pointer("/data/pack/tokenBudget/used"),
        Some(&signature["usedTokens"])
    );
    let writes = usize::from(!read_only);
    assert_eq!(
        payload.pointer("/data/pack/persistence/itemCount"),
        Some(&json!(if read_only { 0 } else { selected })),
        "pack performance probe must report every persisted selected item"
    );
    for pointer in [
        "/data/pack/persistence/attempted",
        "/data/pack/persistence/succeeded",
    ] {
        assert_eq!(
            payload.pointer(pointer),
            Some(&json!(!read_only)),
            "pack performance probe has incorrect persistence state at {pointer}"
        );
    }
    for pointer in [
        "/data/pack/persistence/packRecordWrites",
        "/data/dbReads/packRecordWrites",
    ] {
        assert_eq!(
            payload.pointer(pointer),
            Some(&json!(writes)),
            "pack performance probe has incorrect persistence count at {pointer}"
        );
    }
    // Keep the complete public report, including nested spans that separate
    // writer initialization from writes. The parent packPersistence duration
    // includes that early open; overlapping spans must not be summed.
    for name in [
        "connectionOpen",
        "workspaceLookup",
        "packHash",
        "ledgerSerialization",
        "recordWrite",
        "itemWrites",
        "omissionWrites",
        "transaction",
        "transactionOverhead",
        "audit",
    ] {
        let elapsed_ms = payload
            .pointer(&format!("/data/pack/persistence/subspans/{name}/elapsedMs"))
            .and_then(JsonValue::as_f64)
            .unwrap_or_else(|| panic!("pack performance report lacks persistence {name} timing"));
        assert!(
            elapsed_ms.is_finite() && elapsed_ms >= 0.0,
            "pack persistence {name} has invalid elapsedMs"
        );
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_pack_performance_probe(
    ee_binary: &Path,
    fixture: &PrebuiltSearchFixture,
    daemon_socket: Option<&Path>,
    read_only: bool,
    signature: &JsonValue,
    persistence: &mut PackPersistenceVerifier,
) -> JsonValue {
    let mut command = command_for_pack(ee_binary, fixture, daemon_socket, read_only);
    command.arg("--explain-performance");
    let started = Instant::now();
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn ee pack performance probe: {error}"));
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    assert!(
        output.status.success(),
        "ee pack performance probe failed with {:?}: stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: JsonValue = serde_json::from_slice(&output.stdout)
        .expect("pack performance probe must return public JSON");
    assert_pack_performance_report(&payload, signature, daemon_socket.is_some(), read_only);
    let record_id = persistence.verify_diagnostic(&payload, signature, read_only);
    json!({
        "wallMs": wall_ms,
        "packRecordId": record_id,
        "performance": payload,
    })
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn fixture_size_from_args() -> FixtureSize {
    let args: Vec<_> = std::env::args().collect();
    let count = |flag: &str, default: usize| {
        args.iter()
            .position(|arg| arg == flag)
            .map_or(default, |position| {
                args.get(position + 1)
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|value| *value <= 100_000)
                    .unwrap_or_else(|| panic!("{flag} requires an integer from 0 through 100000"))
            })
    };
    let size = FixtureSize {
        memories: count("--fixture-memories", SEARCH_DOCUMENT_COUNT),
        evidence: count("--fixture-evidence", 0),
    };
    assert!(size.memories > 0, "--fixture-memories must be positive");
    size
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn feedback_scenario_from_args(args: &[String]) -> FeedbackScenario {
    let value = args
        .iter()
        .position(|arg| arg == "--fixture-feedback")
        .map_or("mixed", |position| {
            args.get(position + 1)
                .map(String::as_str)
                .unwrap_or_else(|| panic!("--fixture-feedback requires empty, neutral, or mixed"))
        });
    match value {
        "empty" => FeedbackScenario::Empty,
        "neutral" => FeedbackScenario::Neutral,
        "mixed" => FeedbackScenario::Mixed,
        _ => panic!("--fixture-feedback requires empty, neutral, or mixed; got {value}"),
    }
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn run_warm_pack_measurement(enforce_budgets: bool) {
    let ee_binary = std::env::var_os("EE_BENCH_EE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("EE_BENCH_EE_BIN must name the release ee binary"));
    assert!(ee_binary.is_file(), "EE_BENCH_EE_BIN does not name a file");
    let cli_artifact = measured_cli_artifact(&ee_binary);
    let size = fixture_size_from_args();
    let read_only = std::env::args().any(|argument| argument == "--pack-read-only");
    if enforce_budgets {
        assert_eq!(
            size.memories, SEARCH_DOCUMENT_COUNT,
            "ADR 0052 gate requires 10000 memories; use observation mode at other sizes"
        );
        assert!(
            !read_only,
            "ADR 0052 gate measures ordinary persisted pack; use observation for read-only"
        );
    }
    let feedback_scenario = feedback_scenario_from_args(&std::env::args().collect::<Vec<_>>());
    let fixture = build_prebuilt_search_fixture(&ee_binary, size, feedback_scenario);
    let mut persistence =
        PackPersistenceVerifier::new(&fixture.database, stable_workspace_id(&fixture.workspace));
    let cold_first = run_pack_process(&ee_binary, &fixture, None, read_only, &mut persistence);
    let daemon_started = Instant::now();
    let daemon = RunningPublicDaemon::start(&ee_binary, &fixture);
    let daemon_ready_ms = daemon_started.elapsed().as_secs_f64() * 1_000.0;
    let daemon_first = run_pack_process(
        &ee_binary,
        &fixture,
        Some(daemon.socket_path()),
        read_only,
        &mut persistence,
    );
    assert_eq!(
        cold_first.signature, daemon_first.signature,
        "first daemon pack differs from cold selection"
    );
    let expected = cold_first.signature.clone();
    for _ in 0..SEARCH_WARMUP_SAMPLES {
        for socket in [None, Some(daemon.socket_path())] {
            let observation =
                run_pack_process(&ee_binary, &fixture, socket, read_only, &mut persistence);
            assert_eq!(
                observation.signature, expected,
                "pack warmup selection drift"
            );
        }
    }
    let mut cold = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut warm = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    let mut raw = Vec::with_capacity(SEARCH_MEASURE_SAMPLES);
    for sample in 0..SEARCH_MEASURE_SAMPLES {
        // Alternate path order so a monotonic host-load/thermal trend does not
        // systematically favour the daemon path.
        let (direct, resident) = if sample % 2 == 0 {
            let direct = run_pack_process(&ee_binary, &fixture, None, read_only, &mut persistence);
            let resident = run_pack_process(
                &ee_binary,
                &fixture,
                Some(daemon.socket_path()),
                read_only,
                &mut persistence,
            );
            (direct, resident)
        } else {
            let resident = run_pack_process(
                &ee_binary,
                &fixture,
                Some(daemon.socket_path()),
                read_only,
                &mut persistence,
            );
            let direct = run_pack_process(&ee_binary, &fixture, None, read_only, &mut persistence);
            (direct, resident)
        };
        assert_eq!(
            direct.signature, expected,
            "cold pack sample {sample} selection drift"
        );
        assert_eq!(
            resident.signature, expected,
            "warm pack sample {sample} selection drift"
        );
        cold.push(direct.wall_ms);
        warm.push(resident.wall_ms);
        raw.push(json!({
            "sample": sample, "coldMs": direct.wall_ms, "warmMs": resident.wall_ms,
            "coldServingPath": direct.serving_path, "warmServingPath": resident.serving_path,
            "coldHash": direct.hash, "warmHash": resident.hash,
            "coldPackRecordId": direct.record_id, "warmPackRecordId": resident.record_id,
        }));
    }
    assert_eq!(
        persistence.calls,
        2 * (1 + SEARCH_WARMUP_SAMPLES + SEARCH_MEASURE_SAMPLES),
        "persistence qualification must cover first, warmup and measured calls"
    );
    let cold_p50_ms = p50(cold);
    let warm_p50_ms = p50(warm);
    let speedup = cold_p50_ms / warm_p50_ms;
    let passed = speedup >= PACK_MIN_P50_SPEEDUP;
    let verified_normal_calls = persistence.calls;
    let normal_persisted_records = persistence.record_ids.len();
    // Diagnostic rendering changes the measured workload. Collect it only
    // after all ordinary calls, never as a warmup or percentile sample.
    let cold_performance = run_pack_performance_probe(
        &ee_binary,
        &fixture,
        None,
        read_only,
        &expected,
        &mut persistence,
    );
    let daemon_performance = run_pack_performance_probe(
        &ee_binary,
        &fixture,
        Some(daemon.socket_path()),
        read_only,
        &expected,
        &mut persistence,
    );
    assert_eq!(persistence.calls, verified_normal_calls + 2);
    println!("ee_pack_cli_cold_p50_ms={cold_p50_ms:.3}");
    println!("ee_pack_daemon_warm_p50_ms={warm_p50_ms:.3}");
    println!("ee_pack_daemon_p50_speedup={speedup:.3}");
    println!(
        "ee_pack_daemon_result_json={}",
        json!({
            "schema": "ee.perf.daemon_pack_round_trip.v1",
            "cliArtifact": cli_artifact,
            "operatingProfile": BENCH_OPERATING_PROFILE,
            "operatingProfileSource": "workspace_config",
            "packProfile": BENCH_PACK_PROFILE,
            "fixture": { "memories": fixture.size.memories, "evidenceSpans": fixture.size.evidence,
                "feedbackEvents": fixture.feedback["events"], "feedback": fixture.feedback,
                "seedMs": fixture.seed_ms, "indexMs": fixture.index_ms,
                "query": SEARCH_QUERY },
            "backend": { "embedBackend": "neural_local", "downloads": "disabled",
                "modelId": fixture.fingerprint.model_id, "modelRevision": fixture.fingerprint.model_revision,
                "modelHash": fixture.fingerprint.model_hash, "dimension": fixture.fingerprint.dimension },
            "readOnly": read_only,
            "daemonReadyMs": daemon_ready_ms,
            "firstColdMs": cold_first.wall_ms,
            "firstDaemonMs": daemon_first.wall_ms,
            "firstDaemonServingPath": daemon_first.serving_path,
            "persistence": {
                "verifiedCalls": verified_normal_calls,
                "persistedRecords": normal_persisted_records,
                "checksOutsideWallTime": true,
                "firstColdPackRecordId": cold_first.record_id,
                "firstDaemonPackRecordId": daemon_first.record_id,
            },
            "performanceProbes": {
                "afterMeasuredCalls": true,
                "includedInP50": false,
                "verifiedCalls": persistence.calls - verified_normal_calls,
                "persistedRecords": persistence.record_ids.len() - normal_persisted_records,
                "checksOutsideWallTime": true,
                "selectionParity": if read_only {
                    "reported_count_and_budget_only_no_persisted_ledger"
                } else {
                    "integrity_checked_persisted_selection_matches_ordinary_baseline"
                },
                "backendVerification": "each_probe_requires_search_embed_backend_neural_local",
                "cold": cold_performance,
                "daemon": daemon_performance,
            },
            "coldP50Ms": cold_p50_ms, "warmP50Ms": warm_p50_ms, "p50Speedup": speedup,
            "minimumP50Speedup": PACK_MIN_P50_SPEEDUP,
            "warmupPerPath": SEARCH_WARMUP_SAMPLES, "measuredPerPath": SEARCH_MEASURE_SAMPLES,
            "resultParity": "selected_entities_order_content_trust_provenance_budget",
            "packHashesCompared": false,
            "executionOrder": "alternating_cold_warm_pairs",
            "filesystemCache": "shared_warm_no_eviction",
            "budgetEnforcement": if enforce_budgets { "hard_gate" } else { "observation_only" },
            "passed": passed, "samples": raw,
        })
    );
    if enforce_budgets {
        assert!(
            passed,
            "ADR 0052 requires >= {PACK_MIN_P50_SPEEDUP}x p50 improvement at 10k memories; measured {speedup:.3}x (cold {cold_p50_ms:.3}ms, warm {warm_p50_ms:.3}ms)"
        );
    }
}

#[cfg(all(test, any(target_os = "linux", target_vendor = "apple")))]
mod pack_benchmark_contracts {
    use super::*;

    fn persistence_fixture() -> (TempDir, DbConnection, PackPersistenceVerifier, JsonValue) {
        let root = TempDir::new().expect("persistence fixture directory");
        let database = root.path().join("ee.db");
        let connection = DbConnection::open_file(&database).expect("persistence fixture database");
        connection.migrate().expect("migrate persistence fixture");
        let workspace_id = "wsp_00000000000000000000000001";
        connection
            .insert_workspace(
                workspace_id,
                &CreateWorkspaceInput {
                    path: root.path().display().to_string(),
                    name: None,
                },
            )
            .expect("insert persistence fixture workspace");
        connection
            .insert_memory(
                SEARCH_TARGET_MEMORY_ID,
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    content: "persisted benchmark fact".to_owned(),
                    workflow_id: None,
                    confidence: 0.75,
                    utility: 0.75,
                    importance: 0.75,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert persistence fixture memory");
        let verifier = PackPersistenceVerifier::new(&database, workspace_id.to_owned());
        let payload = json!({"data": {"pack": {
            "hash": format!("blake3:{}", blake3::hash(b"persistence fixture").to_hex()),
            "budget": {"maxTokens": 400, "usedTokens": 10},
            "items": [{"rank": 1, "memoryId": SEARCH_TARGET_MEMORY_ID,
                "section": "facts", "content": "persisted benchmark fact",
                "estimatedTokens": 10, "trust": {"class": "human_explicit"},
                "provenance": [], "selectedIn": "mmr"}]
        }}});
        (root, connection, verifier, payload)
    }

    fn insert_fixture_pack(connection: &DbConnection, id: &str, payload: &JsonValue) {
        connection
            .insert_pack_record(
                id,
                &ee::db::CreatePackRecordInput {
                    task_paths: Vec::new(),
                    workspace_id: "wsp_00000000000000000000000001".to_owned(),
                    query: SEARCH_QUERY.to_owned(),
                    profile: BENCH_PACK_PROFILE.to_owned(),
                    max_tokens: 400,
                    used_tokens: 10,
                    item_count: 1,
                    omitted_count: 0,
                    pack_hash: payload["data"]["pack"]["hash"]
                        .as_str()
                        .expect("fixture hash")
                        .to_owned(),
                    degraded_json: None,
                    created_by: Some("ee context".to_owned()),
                },
                &[ee::db::CreatePackItemInput {
                    pack_id: id.to_owned(),
                    memory_id: SEARCH_TARGET_MEMORY_ID.to_owned(),
                    rank: 1,
                    section: "facts".to_owned(),
                    estimated_tokens: 10,
                    relevance: 0.75,
                    utility: 0.75,
                    combined_score: None,
                    attempt_family_multiplicity: None,
                    why: "persistence qualification".to_owned(),
                    diversity_key: None,
                    provenance_json: ee::pack::pack_item_provenance_json(&[]),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                }],
                &[],
            )
            .expect("persist fixture pack and selection ledger");
    }

    fn pack_performance_fixture(daemon: bool, read_only: bool) -> JsonValue {
        let mut phases = vec![
            "requestValidate",
            "dbOpen",
            "search",
            "candidateResolution",
            "packAssembly",
            "packPersistence",
            "total",
        ];
        if daemon {
            phases.extend(["daemonRetrieval", "daemonHandoffValidation"]);
        } else {
            phases.push("embedderPrepare");
        }
        let timings: Vec<_> = phases
            .into_iter()
            .map(|name| json!({"name": name, "elapsedMs": 0.25}))
            .collect();
        let subspans: serde_json::Map<_, _> = [
            "connectionOpen",
            "workspaceLookup",
            "packHash",
            "ledgerSerialization",
            "recordWrite",
            "itemWrites",
            "omissionWrites",
            "transaction",
            "transactionOverhead",
            "audit",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), json!({"elapsedMs": 0.0})))
        .collect();
        json!({
            "schema": "ee.explain.performance.v1", "success": true,
            "data": {
                "command": "pack",
                "profileRuntime": {
                    "activeProfile": BENCH_OPERATING_PROFILE, "source": "workspace_config",
                },
                "queryPlan": {
                    "profile": BENCH_PACK_PROFILE, "requestedCandidatePool": 100,
                    "maxTokens": 400, "sourceModeRequested": "hybrid",
                    "sourceModeApplied": "hybrid", "strictSourceMode": true,
                    "fallbackApplied": false,
                },
                "servingPath": if daemon { "hybrid" } else { "in_process" },
                "retrievalPath": if daemon { "daemon" } else { "in_process" },
                "fallbackReason": null,
                "search": {"embedBackend": "neural_local", "usesEmbeddings": true},
                "dbReads": {"packRecordWrites": usize::from(!read_only)},
                "pack": {
                    "hashPresent": true, "selectedCount": 1,
                    "tokenBudget": {"limit": 400, "used": 10},
                    "persistence": {
                        "attempted": !read_only, "succeeded": !read_only,
                        "packRecordWrites": usize::from(!read_only),
                        "itemCount": usize::from(!read_only), "subspans": subspans,
                    },
                },
                "timings": timings, "fallbacks": [],
                "redaction": {"memoryContentIncluded": false, "queryTextIncluded": false},
            },
        })
    }

    #[test]
    fn pack_performance_probes_check_the_same_workload_and_persistence_mode() {
        let (_root, _connection, _verifier, response) = persistence_fixture();
        let signature = pack_content_signature(&response);
        for daemon in [false, true] {
            for read_only in [false, true] {
                assert_pack_performance_report(
                    &pack_performance_fixture(daemon, read_only),
                    &signature,
                    daemon,
                    read_only,
                );
            }
        }
    }

    #[test]
    fn pack_performance_probes_reject_missing_or_misleading_evidence() {
        let (_root, _connection, _verifier, response) = persistence_fixture();
        let signature = pack_content_signature(&response);
        for (pointer, changed) in [
            ("/data/timings", json!([])),
            ("/data/timings/0/elapsedMs", json!(-1.0)),
            (
                "/data/pack/persistence/subspans/connectionOpen",
                JsonValue::Null,
            ),
            ("/data/profileRuntime/source", json!("host_probe")),
            ("/data/queryPlan/sourceModeApplied", json!("lexical_only")),
            ("/data/search/embedBackend", JsonValue::Null),
            ("/data/search/embedBackend", json!("hash_fallback")),
            ("/data/search/usesEmbeddings", json!(false)),
            ("/data/retrievalPath", json!("in_process")),
            ("/data/servingPath", json!("in_process_fallback")),
            ("/data/fallbackReason", json!("daemon_handoff_rejected")),
            (
                "/data/fallbacks",
                json!([{"code": "daemon_search_fallback"}]),
            ),
            (
                "/data/fallbacks",
                json!([{"code": "embed_model_unavailable"}]),
            ),
            (
                "/data/fallbacks",
                json!([{"code": "context_pack_persist_failed"}]),
            ),
            ("/data/pack/persistence/succeeded", json!(false)),
            ("/data/pack/persistence/packRecordWrites", json!(0)),
            ("/data/dbReads/packRecordWrites", json!(2)),
            ("/data/pack/selectedCount", json!(0)),
            ("/data/pack/tokenBudget/used", json!(11)),
        ] {
            let mut changed_report = pack_performance_fixture(true, false);
            *changed_report
                .pointer_mut(pointer)
                .expect("diagnostic field") = changed;
            assert!(
                std::panic::catch_unwind(|| assert_pack_performance_report(
                    &changed_report,
                    &signature,
                    true,
                    false
                ))
                .is_err(),
                "pack performance probe accepted changed evidence at {pointer}"
            );
        }
    }

    #[test]
    fn redacted_diagnostic_calls_verify_new_records_and_read_only_zero_writes() {
        let (_root, connection, mut verifier, response) = persistence_fixture();
        let signature = pack_content_signature(&response);
        let diagnostic = pack_performance_fixture(true, false);
        assert!(diagnostic.pointer("/data/pack/hash").is_none());
        assert!(diagnostic.pointer("/data/pack/items").is_none());
        let id = "pack_00000000000000000000000001";
        insert_fixture_pack(&connection, id, &response);
        assert_eq!(
            verifier
                .verify_diagnostic(&diagnostic, &signature, false)
                .as_deref(),
            Some(id)
        );
        assert!(
            verifier
                .verify_diagnostic(&pack_performance_fixture(true, true), &signature, true)
                .is_none()
        );
        assert_eq!(verifier.calls, 2);
        assert_eq!(verifier.record_ids.len(), 1);
    }

    #[test]
    #[should_panic(expected = "each persisted pack call must add exactly one record")]
    fn redacted_diagnostic_cannot_reuse_the_previous_normal_calls_record() {
        let (_root, connection, mut verifier, response) = persistence_fixture();
        let signature = pack_content_signature(&response);
        insert_fixture_pack(&connection, "pack_00000000000000000000000001", &response);
        let _ = verifier.verify(&response, &signature, false);
        let _ =
            verifier.verify_diagnostic(&pack_performance_fixture(true, false), &signature, false);
    }

    #[test]
    #[should_panic(expected = "persisted selection must match")]
    fn redacted_diagnostic_rejects_a_valid_ledger_with_a_different_selection() {
        let (_root, connection, mut verifier, response) = persistence_fixture();
        insert_fixture_pack(&connection, "pack_00000000000000000000000001", &response);
        let mut baseline = pack_content_signature(&response);
        baseline["items"][0]["memoryId"] = json!("mem_another_baseline_selection");
        let _ =
            verifier.verify_diagnostic(&pack_performance_fixture(true, false), &baseline, false);
    }

    #[test]
    fn persisted_gate_verifies_each_new_record_even_when_hashes_repeat() {
        let (_root, connection, mut verifier, payload) = persistence_fixture();
        let signature = pack_content_signature(&payload);
        for id in [
            "pack_00000000000000000000000001",
            "pack_00000000000000000000000002",
        ] {
            insert_fixture_pack(&connection, id, &payload);
            assert_eq!(
                verifier.verify(&payload, &signature, false).as_deref(),
                Some(id)
            );
        }
        assert_eq!(verifier.calls, 2);
        assert_eq!(verifier.record_ids.len(), 2);
        assert!(verifier.verify(&payload, &signature, true).is_none());
        assert_eq!(verifier.calls, 3);
    }

    #[test]
    #[should_panic(expected = "each persisted pack call must add exactly one record")]
    fn persisted_gate_rejects_a_successful_response_without_a_write() {
        let (_root, _connection, mut verifier, payload) = persistence_fixture();
        let _ = verifier.verify(&payload, &pack_content_signature(&payload), false);
    }

    #[test]
    #[should_panic(expected = "each persisted pack call must add exactly one record")]
    fn persisted_gate_rejects_duplicate_writes_for_one_response() {
        let (_root, connection, mut verifier, payload) = persistence_fixture();
        for id in [
            "pack_00000000000000000000000001",
            "pack_00000000000000000000000002",
        ] {
            insert_fixture_pack(&connection, id, &payload);
        }
        let _ = verifier.verify(&payload, &pack_content_signature(&payload), false);
    }

    #[test]
    #[should_panic(expected = "persisted pack hash must identify this invocation's response")]
    fn persisted_gate_rejects_a_record_from_another_response() {
        let (_root, connection, mut verifier, mut payload) = persistence_fixture();
        insert_fixture_pack(&connection, "pack_00000000000000000000000001", &payload);
        payload["data"]["pack"]["hash"] = json!("a-different-response");
        let _ = verifier.verify(&payload, &pack_content_signature(&payload), false);
    }

    #[test]
    #[should_panic(expected = "persisted selection must match the response's typed entities")]
    fn persisted_gate_rejects_a_valid_ledger_with_a_different_selection() {
        let (_root, connection, mut verifier, mut payload) = persistence_fixture();
        let id = "pack_00000000000000000000000001";
        insert_fixture_pack(&connection, id, &payload);
        let stored = connection
            .get_pack_record(id)
            .expect("read valid record")
            .expect("stored record");
        assert!(
            ee::db::parse_stored_pack_ledger(&stored)
                .available_ledger()
                .is_some()
        );
        // Hash, count and budget still agree, and the stored ledger is valid.
        // Only the selected entity differs from the public response.
        payload["data"]["pack"]["items"][0]["memoryId"] = json!("mem_00000000000000000000000002");
        let _ = verifier.verify(&payload, &pack_content_signature(&payload), false);
    }

    #[test]
    fn persisted_selection_detects_rank_budget_trust_and_provenance_changes() {
        let (_root, connection, _verifier, payload) = persistence_fixture();
        let id = "pack_00000000000000000000000001";
        insert_fixture_pack(&connection, id, &payload);
        let record = connection
            .get_pack_record(id)
            .expect("read record")
            .expect("stored record");
        let parsed = ee::db::parse_stored_pack_ledger(&record);
        let ledger = parsed.available_ledger().expect("valid selection ledger");
        let baseline = pack_content_signature(&payload);
        assert_persisted_selection_matches_response(ledger, &baseline);
        for (pointer, changed) in [
            ("/items/0/rank", json!(2)),
            ("/items/0/section", json!("warnings")),
            ("/items/0/estimatedTokens", json!(11)),
            ("/items/0/trust/class", json!("cass_evidence")),
            (
                "/items/0/provenance",
                json!([{"uri": "file://wrong.rs", "note": "wrong source"}]),
            ),
        ] {
            let mut changed_signature = baseline.clone();
            *changed_signature
                .pointer_mut(pointer)
                .expect("signature mutation") = changed;
            assert!(
                std::panic::catch_unwind(|| assert_persisted_selection_matches_response(
                    ledger,
                    &changed_signature
                ))
                .is_err(),
                "missed persisted selection change at {pointer}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "integrity-checked, record-bound selection ledger")]
    fn persisted_gate_rejects_a_corrupt_selection_ledger() {
        let (_root, connection, mut verifier, payload) = persistence_fixture();
        insert_fixture_pack(&connection, "pack_00000000000000000000000001", &payload);
        connection
            .execute_raw(
                "UPDATE pack_records SET ledger_hash = 'not-the-ledger-hash' \
                 WHERE id = 'pack_00000000000000000000000001'",
            )
            .expect("corrupt fixture ledger hash");
        let _ = verifier.verify(&payload, &pack_content_signature(&payload), false);
    }

    #[test]
    #[should_panic(expected = "read-only calls must add none")]
    fn read_only_observation_rejects_persisted_writes() {
        let (_root, connection, mut verifier, payload) = persistence_fixture();
        insert_fixture_pack(&connection, "pack_00000000000000000000000001", &payload);
        let _ = verifier.verify(&payload, &pack_content_signature(&payload), true);
    }

    #[test]
    #[should_panic(expected = "removed or replaced an existing persisted record")]
    fn persisted_gate_rejects_replaced_record_identities() {
        let previous = BTreeSet::from(["original".to_owned()]);
        let current = BTreeSet::from(["replacement".to_owned(), "new".to_owned()]);
        let _ = verified_pack_record_delta(&previous, &current, false);
    }

    #[test]
    fn persisted_gate_rejects_reported_persistence_failure_at_either_envelope_depth() {
        for pointer in ["degraded", "data"] {
            let degradation = json!([{"code": "context_pack_persist_failed"}]);
            let payload = if pointer == "data" {
                json!({"success": true, "data": {"degraded": degradation}})
            } else {
                json!({"success": true, "degraded": degradation})
            };
            assert!(
                std::panic::catch_unwind(|| assert_pack_persistence_not_degraded(&payload))
                    .is_err()
            );
        }
    }

    fn neutral_feedback_event() -> StoredFeedbackEvent {
        StoredFeedbackEvent {
            id: format!("fb_{:026}", 0),
            workspace_id: "workspace-fixture".to_owned(),
            target_type: "memory".to_owned(),
            target_id: SEARCH_TARGET_MEMORY_ID.to_owned(),
            signal: "neutral".to_owned(),
            weight: 0.0,
            source_type: "outcome_observed".to_owned(),
            source_id: Some("daemon-bench-feedback-0".to_owned()),
            reason: Some("Daemon pack feedback benchmark fixture".to_owned()),
            evidence_json: None,
            session_id: None,
            applied_at: Some("2026-11-01T00:00:00Z".to_owned()),
            created_at: "2026-11-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn pack_fixture_defaults_to_mixed_feedback_with_explicit_empty_control() {
        assert_eq!(feedback_scenario_from_args(&[]), FeedbackScenario::Mixed);
        for (value, expected) in [
            ("empty", FeedbackScenario::Empty),
            ("neutral", FeedbackScenario::Neutral),
            ("mixed", FeedbackScenario::Mixed),
        ] {
            assert_eq!(
                feedback_scenario_from_args(&["--fixture-feedback".to_owned(), value.to_owned()]),
                expected
            );
        }
    }

    #[test]
    #[should_panic(expected = "--fixture-feedback requires")]
    fn invalid_feedback_scenario_cannot_silently_measure_empty_history() {
        let _ = feedback_scenario_from_args(&["--fixture-feedback".to_owned(), "mixd".to_owned()]);
    }

    #[test]
    fn feedback_fixture_requires_canonical_applied_rows() {
        assert_feedback_fixture(
            &[neutral_feedback_event()],
            "workspace-fixture",
            10,
            FeedbackScenario::Neutral,
        );
    }

    #[test]
    #[should_panic(expected = "feedback fixture event was not applied")]
    fn unapplied_feedback_cannot_satisfy_the_fixture() {
        let mut event = neutral_feedback_event();
        event.applied_at = None;
        assert_feedback_fixture(&[event], "workspace-fixture", 10, FeedbackScenario::Neutral);
    }

    #[test]
    #[should_panic(expected = "feedback fixture signal drift")]
    fn same_count_feedback_replacement_cannot_satisfy_the_fixture() {
        let mut event = neutral_feedback_event();
        event.signal = "harmful".to_owned();
        assert_feedback_fixture(&[event], "workspace-fixture", 10, FeedbackScenario::Neutral);
    }

    fn response() -> JsonValue {
        json!({"data": {
            "servingPath": "in_process",
            "pack": {
                "id": "one-record", "hash": "one-hash",
                "budget": {"maxTokens": 400, "usedTokens": 20},
                "items": [
                    {"rank": 1, "memoryId": "mem_a", "section": "facts", "content": "first fact",
                     "estimatedTokens": 10, "trust": {"class": "agent_validated"},
                     "provenance": [{"uri": "test://first"}], "selectedIn": "mmr"},
                    {"rank": 2, "memoryId": "mem_b", "section": "facts", "content": "second fact",
                     "estimatedTokens": 10, "trust": {"class": "agent_validated"},
                     "provenance": [{"uri": "test://second"}], "selectedIn": "mmr"}
                ]
            }
        }})
    }

    #[test]
    fn pack_parity_detects_wrong_content_identity_order_trust_and_provenance() {
        let baseline = response();
        let expected = pack_content_signature(&baseline);
        for (pointer, changed) in [
            ("/data/pack/items/0/content", json!("wrong fact")),
            ("/data/pack/items/0/memoryId", json!("mem_wrong")),
            ("/data/pack/items/0/rank", json!(2)),
            ("/data/pack/items/0/trust/class", json!("untrusted")),
            ("/data/pack/items/0/provenance/0/uri", json!("test://wrong")),
            ("/data/pack/budget/usedTokens", json!(401)),
        ] {
            let mut wrong = baseline.clone();
            *wrong.pointer_mut(pointer).expect("fixture mutation path") = changed;
            assert_ne!(pack_content_signature(&wrong), expected, "missed {pointer}");
        }
        let mut reordered = baseline.clone();
        reordered
            .pointer_mut("/data/pack/items")
            .and_then(JsonValue::as_array_mut)
            .expect("fixture items")
            .reverse();
        assert_ne!(pack_content_signature(&reordered), expected);
        let mut delivered = baseline;
        delivered["data"]["servingPath"] = json!("hybrid");
        delivered["data"]["pack"]["id"] = json!("new-record");
        assert_eq!(pack_content_signature(&delivered), expected);
    }

    #[test]
    #[should_panic(expected = "empty packs cannot prove retrieval parity")]
    fn pack_parity_refuses_vacuous_empty_results() {
        let mut empty = response();
        empty["data"]["pack"]["items"] = json!([]);
        let _ = pack_content_signature(&empty);
    }

    #[test]
    #[should_panic(expected = "warm pack used in_process_fallback")]
    fn stale_feedback_fallback_cannot_satisfy_daemon_acceleration_gate() {
        let fallback = json!({"data": {
            "servingPath": "in_process_fallback",
            "retrievalPath": "in_process",
            "fallbackReason": "daemon_handoff_rejected"
        }});
        let _ = verified_pack_serving_path(&fallback, true);
    }
}

#[cfg(unix)]
fn main() {
    if std::env::args()
        .any(|argument| argument == "--warm-pack-gate" || argument == "--warm-pack-observation")
    {
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        run_warm_pack_measurement(std::env::args().any(|argument| argument == "--warm-pack-gate"));
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        panic!("warm-pack measurement requires safe same-EUID peer credentials on Linux or Apple");
        return;
    }
    if std::env::args().any(|argument| argument == "--warm-search-gate") {
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        run_warm_search_measurement(true);
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        panic!("warm-search gate requires safe same-EUID peer credentials on Linux or Apple");
        return;
    }
    if std::env::args().any(|argument| argument == "--warm-search-observation") {
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        run_warm_search_measurement(false);
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        panic!(
            "warm-search observation requires safe same-EUID peer credentials on Linux or Apple"
        );
        return;
    }

    let mut criterion = Criterion::default().configure_from_args();
    bench_daemon_round_trip(&mut criterion);
    criterion.final_summary();
}

#[cfg(not(unix))]
fn main() {}
