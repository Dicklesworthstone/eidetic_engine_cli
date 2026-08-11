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
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::process::Command;
#[cfg(unix)]
use std::time::Duration;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use std::time::Instant;

#[cfg(unix)]
use criterion::Criterion;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
#[cfg(unix)]
use ee::daemon::protocol::DaemonRequest;
#[cfg(unix)]
use ee::daemon::server::{METHOD_CONTEXT, METHOD_ECHO, dispatch};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
use ee::models::WorkspaceId;
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

    fn start_for_workspace(socket_path: PathBuf, workspace: &Path) -> Self {
        let workspace_id = workspace.display().to_string();
        let handle = ee::daemon::server::start_server_for_workspace(&socket_path, workspace_id)
            .unwrap_or_else(|error| panic!("start warm-search daemon: {error}"));
        Self {
            _socket_dir: None,
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
    seed_ms: f64,
    index_ms: f64,
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn stable_workspace_id(workspace: &Path) -> String {
    let hash = blake3::hash(format!("daemon-search-bench:{}", workspace.display()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn build_prebuilt_search_fixture() -> PrebuiltSearchFixture {
    assert!(
        matches!(std::env::var("EE_EMBED_DOWNLOAD").as_deref(), Ok("off")),
        "warm-search gate requires EE_EMBED_DOWNLOAD=off so measurement never performs network I/O"
    );
    let root =
        TempDir::new().unwrap_or_else(|error| panic!("warm-search fixture tempdir: {error}"));
    let workspace = root.path().join("workspace");
    let ee_dir = workspace.join(".ee");
    std::fs::create_dir_all(&ee_dir)
        .unwrap_or_else(|error| panic!("create warm-search fixture layout: {error}"));
    let database = ee_dir.join("ee.db");
    let index = ee_dir.join("index");

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
    let report = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(index.clone()),
        dry_run: false,
    })
    .unwrap_or_else(|error| panic!("build warm-search fixture index: {error}"));
    let index_ms = index_started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(report.status, IndexRebuildStatus::Success);
    assert_eq!(report.memories_indexed as usize, SEARCH_DOCUMENT_COUNT);
    assert_eq!(report.documents_total as usize, SEARCH_DOCUMENT_COUNT);
    assert!(
        report.errors.is_empty(),
        "index rebuild errors: {:?}",
        report.errors
    );

    PrebuiltSearchFixture {
        _root: root,
        workspace,
        database,
        index,
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
        .arg("--strict-source-mode")
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_NO_COLOR", "1")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("FRANKENSEARCH_API_PROVIDER")
        .env_remove("FRANKENSEARCH_API_MODEL")
        .env_remove("FRANKENSEARCH_API_DIMENSION")
        .env_remove("FRANKENSEARCH_API_IDENTITY_JSON");
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

    let fixture = build_prebuilt_search_fixture();
    let socket_path = fixture._root.path().join("ee-daemon-search-slo.sock");
    let daemon_started = Instant::now();
    let daemon = RunningDaemon::start_for_workspace(socket_path, &fixture.workspace);
    let daemon_start_ms = daemon_started.elapsed().as_secs_f64() * 1_000.0;

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
    for _ in 0..SEARCH_MEASURE_SAMPLES {
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
    }

    let cold_p50_ms = p50(cold_wall);
    let cold_core_p50_ms = p50(cold_core);
    let cold_overhead_p50_ms = p50(cold_overhead);
    let warm_p50_ms = p50(warm_wall);
    let warm_core_p50_ms = p50(warm_core);
    let warm_overhead_p50_ms = p50(warm_overhead);
    let passed = cold_p50_ms < SEARCH_COLD_P50_BUDGET_MS && warm_p50_ms < SEARCH_WARM_P50_BUDGET_MS;

    println!("ee_search_cli_cold_10k_p50_ms={cold_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_p50_ms={warm_p50_ms:.3}");
    println!("ee_search_cli_cold_10k_core_p50_ms={cold_core_p50_ms:.3}");
    println!("ee_search_cli_cold_10k_overhead_p50_ms={cold_overhead_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_core_p50_ms={warm_core_p50_ms:.3}");
    println!("ee_search_daemon_warm_10k_overhead_p50_ms={warm_overhead_p50_ms:.3}");
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
            },
            "samples": {
                "warmupPerPath": SEARCH_WARMUP_SAMPLES,
                "measuredPerPath": SEARCH_MEASURE_SAMPLES,
                "executionOrder": "cold_then_warm_interleaved",
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
