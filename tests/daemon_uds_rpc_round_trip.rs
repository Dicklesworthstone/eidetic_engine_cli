//! Integration test for the bd-oja31 daemon UDS RPC skeleton.
//!
//! Pins the wire-framing contract end-to-end: spin up the daemon
//! server on a tempdir UDS, send an `ee.daemon.echo` request, assert
//! default production servers refuse the diagnostic reflector, send an
//! `ee.daemon.context` request, assert the result carries the canonical
//! `ee.response.v2` / `ee.pack.v2` context-pack payload, exercise repeated
//! strict `ee.daemon.search` calls against one long-lived process, and shut the
//! server down cleanly so the socket file is unlinked.
//!
//! Cfg-gated to Unix because the UDS server is Unix-only; non-Unix
//! builds get a no-op stub so the test binary compiles cleanly under
//! the Windows `cargo test --workspace` smoke job.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use ee::core::context::{
    ContextPackError, ContextPackOptions, ContextPackOutputOptions,
    run_context_pack_with_performance_controlled,
};
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::core::model::{ModelFetchOptions, fetch_rerank_model};
use ee::core::outcome::cancel_message;
use ee::core::search::{
    SEARCH_ADVISORY_SCOPE_PROCESS, SEARCH_INDEX_LARGE_GAP_THRESHOLD, SearchSourceMode,
};
use ee::daemon::{
    DAEMON_METHOD_UNAUTHORIZED_CODE, DAEMON_REQUEST_MAX_BYTES, DAEMON_REQUEST_SCHEMA_V1,
    DAEMON_RESPONSE_MAX_BYTES, DAEMON_RESPONSE_SCHEMA_V1, DAEMON_SHUTTING_DOWN_CODE,
    protocol::{DaemonRequest, DaemonResponse},
    server::{
        ClientError, DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE, DAEMON_CONTEXT_PARAMS_INVALID_CODE,
        DAEMON_ECHO_DISABLED_CODE, DAEMON_REQUEST_DECODE_FAILED_CODE,
        DAEMON_REQUEST_SCHEMA_MISMATCH_CODE, DAEMON_SEARCH_REQUEST_SCHEMA_V2,
        DAEMON_SEARCH_RESPONSE_SCHEMA_V3, DAEMON_UNKNOWN_METHOD_CODE, DaemonSearchResult,
        METHOD_CAPABILITIES, METHOD_CONTEXT, METHOD_ECHO, METHOD_SEARCH, METHOD_SHUTDOWN,
        METHOD_TELEMETRY, METHOD_WRITE, METHOD_WRITE_JOURNAL, client_round_trip, start_server,
        start_server_for_workspace,
    },
};
use ee::db::{CreateMemoryInput, CreateModelRegistryInput, CreateWorkspaceInput, DbConnection};
use ee::models::model_registry::{ModelPurpose, ModelRegistryStatus};
use ee::models::{MemoryScope, QueryFilters, RedactionLevel, WorkspaceId};
use ee::pack::{ContextPackProfile, DEFAULT_COORDINATION_STALE_AFTER_MS, PackResourceProfile};
use ee::search::SpeedMode;
use ee::steward::{
    JobPriority, JobType, ManualRunner, RunnerOptions, ScoreDecayJobOptions, run_score_decay_job,
};

mod tempfile {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    pub fn tempdir() -> std::io::Result<::tempfile::TempDir> {
        let temp = ::tempfile::tempdir()?;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))?;
        Ok(temp)
    }
}

type TestResult = Result<(), String>;
const TEST_AGENT_ID: &str = "agent-daemon-uds-test";
const TEST_WORKSPACE_ID: &str = "workspace-daemon-uds-test";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn connect_client(socket_path: &Path) -> Result<UnixStream, String> {
    let stream = UnixStream::connect(socket_path).map_err(|error| format!("connect: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("set_read_timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("set_write_timeout: {error}"))?;
    Ok(stream)
}

fn secure_socket_path(root: &Path, file_name: &str) -> Result<PathBuf, String> {
    let socket_dir = root.join("daemon-sockets");
    fs::create_dir_all(&socket_dir).map_err(|error| format!("create socket dir: {error}"))?;
    fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("secure socket dir permissions: {error}"))?;
    Ok(socket_dir.join(file_name))
}

fn stable_test_workspace_id(workspace: &Path) -> Result<String, String> {
    let canonical = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace {}: {error}", workspace.display()))?;
    let hash = blake3::hash(format!("workspace:{}", canonical.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    Ok(WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string())
}

fn context_request(
    request_id: &'static str,
    agent_id: &'static str,
    params: serde_json::Value,
) -> DaemonRequest {
    let mut request = DaemonRequest::new(request_id, agent_id, METHOD_CONTEXT, params);
    request.workspace_id = Some(TEST_WORKSPACE_ID.to_owned());
    request
}

fn search_request(
    request_id: &str,
    workspace: &Path,
    database: &Path,
    index_dir: &Path,
) -> DaemonRequest {
    search_request_with_query(
        request_id,
        workspace,
        database,
        index_dir,
        "release provenance",
        false,
    )
}

fn search_request_with_query(
    request_id: &str,
    workspace: &Path,
    database: &Path,
    index_dir: &Path,
    query: &str,
    explain_performance: bool,
) -> DaemonRequest {
    let workspace_id = workspace.display().to_string();
    let mut params = serde_json::json!({
        "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
        "query": query,
        "workspacePath": workspace_id,
        "databasePath": database.display().to_string(),
        "indexDir": index_dir.display().to_string(),
        "limit": 5,
        "speed": "instant",
        "sourceMode": "hybrid",
        "memoryScope": "swarm",
        "explainPerformance": explain_performance
    });
    if explain_performance {
        // Force the planted-query request through the historical
        // no_relevant_results degradation that used to echo query text.
        params["relevanceFloor"] = serde_json::json!(1.0);
    }
    let mut request = DaemonRequest::new(request_id, TEST_AGENT_ID, METHOD_SEARCH, params);
    request.workspace_id = Some(workspace.display().to_string());
    request
}

fn seed_context_workspace(root: &Path) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let workspace = root.join("workspace");
    let ee_dir = workspace.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee dir: {error}"))?;
    let database = ee_dir.join("ee.db");
    let workspace_id = stable_test_workspace_id(&workspace)?;
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("daemon-uds-context".to_string()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory(
            "mem_00000000000000000000005001",
            &CreateMemoryInput {
                workspace_id,
                level: "procedural".to_string(),
                kind: "rule".to_string(),
                content: "Daemon context canonical pack must preserve release provenance."
                    .to_string(),
                workflow_id: None,
                confidence: 0.95,
                utility: 0.9,
                importance: 0.8,
                provenance_uri: Some("file://AGENTS.md#daemon-context".to_string()),
                trust_class: "agent_validated".to_string(),
                trust_subclass: Some("daemon-uds-test".to_string()),
                tags: vec!["daemon".to_string(), "release".to_string()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok((workspace, database))
}

fn rebuild_test_index(workspace: &Path, database: &Path, index_dir: &Path) -> TestResult {
    let rebuild = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace.to_path_buf(),
        database_path: Some(database.to_path_buf()),
        index_dir: Some(index_dir.to_path_buf()),
        dry_run: false,
    })
    .map_err(|error| format!("rebuild search index: {error}"))?;
    ensure(
        rebuild.status == IndexRebuildStatus::Success,
        format!(
            "search index rebuild must succeed; got {:?}",
            rebuild.status
        ),
    )
}

fn plant_large_index_gap(database: &Path, id_base: u64, label: &str) -> TestResult {
    let workspace = database
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("database has no workspace parent: {}", database.display()))?;
    let workspace_id = stable_test_workspace_id(workspace)?;
    let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
    for offset in 0..=SEARCH_INDEX_LARGE_GAP_THRESHOLD {
        let memory_id = format!("mem_{:026}", id_base.saturating_add(offset));
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "working".to_owned(),
                    kind: "fact".to_owned(),
                    content: format!("Unindexed {label} generation {offset}."),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some(format!("test://daemon-advisory/{label}/{offset}")),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: Some("daemon-uds-advisory".to_owned()),
                    tags: vec!["daemon".to_owned(), "advisory".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
    }
    connection.close().map_err(|error| error.to_string())
}

fn seed_rerank_candidates(workspace: &Path, database: &Path) -> TestResult {
    let workspace_id = stable_test_workspace_id(workspace)?;
    let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
    for offset in 0..6_u64 {
        connection
            .insert_memory(
                &format!("mem_{:026}", 70_000_u64 + offset),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    content: format!(
                        "Release provenance daemon search evidence context rule {offset}."
                    ),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some(format!("test://daemon-reranker/{offset}")),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: Some("daemon-uds-rerank-candidate".to_owned()),
                    tags: vec!["daemon".to_owned(), "reranker".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
    }
    connection.close().map_err(|error| error.to_string())
}

fn context_request_for_workspace(
    request_id: &'static str,
    workspace: &Path,
    database: &Path,
    task: &str,
) -> DaemonRequest {
    let mut params = context_pack_params(workspace, database, task);
    params["includeNonAffectingDegradations"] = serde_json::json!(true);
    let mut request = DaemonRequest::new(request_id, TEST_AGENT_ID, METHOD_CONTEXT, params);
    request.workspace_id = Some(workspace.display().to_string());
    request
}

fn hybrid_context_request_for_workspace(
    request_id: &'static str,
    params_workspace: &Path,
    authorized_workspace: &Path,
    database: &Path,
    index_dir: &Path,
    task: &str,
) -> DaemonRequest {
    let mut params = context_pack_params(params_workspace, database, task);
    params["sourceMode"] = serde_json::json!("hybrid");
    params["indexDir"] = serde_json::json!(index_dir.display().to_string());
    params["includeNonAffectingDegradations"] = serde_json::json!(true);
    let mut request = DaemonRequest::new(request_id, TEST_AGENT_ID, METHOD_CONTEXT, params);
    request.workspace_id = Some(authorized_workspace.display().to_string());
    request
}

fn successful_result(response: DaemonResponse, label: &str) -> Result<serde_json::Value, String> {
    ensure(
        response.error.is_none(),
        format!("{label} returned a daemon error: {response:?}"),
    )?;
    response
        .result
        .ok_or_else(|| format!("{label} omitted its result"))
}

fn degraded_codes_at<'a>(
    result: &'a serde_json::Value,
    pointer: &str,
    label: &str,
) -> Result<Vec<&'a str>, String> {
    result
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{label} omitted {pointer}: {result}"))
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("code").and_then(serde_json::Value::as_str))
                .collect()
        })
}

/// The structured per-response freshness truth must stay authoritative on
/// every stale search response, including episode-suppressed repeats
/// (bd-index-auto-freshness-m5kwf).
fn assert_search_index_freshness(result: &serde_json::Value, label: &str) -> TestResult {
    let freshness = result
        .pointer("/response/data/indexFreshness")
        .ok_or_else(|| format!("{label}: indexFreshness missing: {result}"))?;
    ensure(
        freshness
            .pointer("/stale")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && freshness
                .pointer("/dbGeneration")
                .and_then(serde_json::Value::as_u64)
                .is_some()
            && freshness
                .pointer("/indexGeneration")
                .and_then(serde_json::Value::as_u64)
                .is_some()
            && freshness
                .pointer("/generationGap")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|gap| gap > 0)
            && freshness
                .pointer("/largeGap")
                .and_then(serde_json::Value::as_bool)
                == Some(true),
        format!("{label}: structured freshness truth drifted: {freshness}"),
    )
}

/// Send one framed daemon request over a real socket and abort the
/// connection before reading the response, so the daemon's socket write
/// fails and any deferred advisory delivery settles as undelivered.
fn client_send_frame_then_abort(socket_path: &Path, request: &DaemonRequest) -> TestResult {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|error| format!("abort-client connect: {error}"))?;
    let body =
        serde_json::to_vec(request).map_err(|error| format!("abort-client encode: {error}"))?;
    let length =
        u32::try_from(body.len()).map_err(|error| format!("abort-client length: {error}"))?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|error| format!("abort-client length write: {error}"))?;
    stream
        .write_all(&body)
        .map_err(|error| format!("abort-client body write: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("abort-client flush: {error}"))?;
    stream
        .shutdown(Shutdown::Both)
        .map_err(|error| format!("abort-client shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_failed_delivery_does_not_consume_stale_warning_episode() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-advisory-abort.sock")?;
    let (workspace, database) = seed_context_workspace(&temp.path().join("workspace-abort"))?;
    let index_dir = workspace.join(".ee").join("index");
    rebuild_test_index(&workspace, &database, &index_dir)?;
    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    plant_large_index_gap(&database, 400_000, "workspace-abort-search")?;

    // Planted negative: the first affected response is sent to a client that
    // aborts before reading. The daemon's socket write fails, so the
    // reservation must settle as undelivered and the episode must NOT be
    // consumed.
    client_send_frame_then_abort(
        handle.socket_path(),
        &search_request("req-abort-first", &workspace, &database, &index_dir),
    )?;
    // Synchronize on the exact production state transition rather than a
    // scheduling delay: while the aborted response still owns the provisional
    // reservation, probes carry structured freshness without warning prose.
    // The first probe after failed-write settlement must receive the full
    // warning pair and becomes the successfully delivered episode winner.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut attempt = 0_u64;
    let replay = loop {
        attempt = attempt.saturating_add(1);
        let candidate = successful_result(
            client_round_trip(
                handle.socket_path(),
                &search_request(
                    &format!("req-abort-replay-{attempt}"),
                    &workspace,
                    &database,
                    &index_dir,
                ),
            )
            .map_err(|error| format!("replay round-trip {attempt}: {error}"))?,
            "replay after aborted delivery",
        )?;
        assert_search_index_freshness(&candidate, "failed-delivery-settlement-probe")?;
        let codes = degraded_codes_at(&candidate, "/response/data/degraded")
            .ok_or_else(|| format!("settlement probe omitted degraded[]: {candidate}"))?;
        if codes.contains(&"search_index_stale") && codes.contains(&"search_index_large_gap") {
            break candidate;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "aborted socket delivery did not release its advisory reservation after {attempt} production round trips; last={candidate}"
            ));
        }
        thread::yield_now();
    };
    assert_stale_episode(
        &replay,
        "/response/data/degraded",
        true,
        true,
        "replay_full_warning_after_failed_delivery",
    )?;
    assert_search_index_freshness(&replay, "replay_freshness")?;

    // Positive observable: the successfully delivered warning consumes the
    // episode, so the next response in the same episode emits neither
    // warning while structured truth stays.
    let suppressed = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request("req-abort-suppressed", &workspace, &database, &index_dir),
        )
        .map_err(|error| format!("suppressed round-trip: {error}"))?,
        "suppressed response after delivered warning",
    )?;
    assert_stale_episode(
        &suppressed,
        "/response/data/degraded",
        false,
        false,
        "suppressed_after_delivered_warning",
    )?;
    assert_search_index_freshness(&suppressed, "suppressed_freshness")?;

    handle.shutdown()
}

fn assert_stale_episode(
    result: &serde_json::Value,
    degraded_pointer: &str,
    expect_stale: bool,
    expect_large_gap: bool,
    label: &str,
) -> TestResult {
    let codes = degraded_codes_at(result, degraded_pointer, label)?;
    ensure(
        codes.contains(&"search_index_stale") == expect_stale,
        format!("{label} stale truth drifted; codes={codes:?}; result={result}"),
    )?;
    ensure(
        codes.contains(&"search_index_large_gap") == expect_large_gap,
        format!("{label} large-gap episode drifted; codes={codes:?}; result={result}"),
    )?;
    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "test": "daemon_advisory_active_episode_lifecycle",
            "phase": label,
            "event": "assertion",
            "data": {
                "degradedCodes": codes,
                "expectedStale": expect_stale,
                "expectedLargeGap": expect_large_gap,
            }
        })
    );
    Ok(())
}

fn context_pack_params(workspace: &Path, database: &Path, task: &str) -> serde_json::Value {
    let workspace_path = workspace.to_string_lossy().into_owned();
    let database_path = database.to_string_lossy().into_owned();
    serde_json::json!({
        "task": task,
        "workspacePath": workspace_path,
        "databasePath": database_path,
        "speed": "instant",
        "sourceMode": "lexical_only",
        "candidatePool": 20,
        "maxTokens": 600,
        "readOnly": true
    })
}

fn context_pack_options(workspace: &Path, database: &Path, task: &str) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace.to_path_buf(),
        database_path: Some(database.to_path_buf()),
        index_dir: None,
        query: task.to_owned(),
        speed: SpeedMode::Instant,
        source_mode: SearchSourceMode::LexicalOnly,
        strict_source_mode: false,
        filters: QueryFilters::default(),
        profile: Some(ContextPackProfile::Balanced),
        max_tokens: Some(600),
        candidate_pool: Some(20),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        require_fresh_sentinels: false,
        relevance_floor: None,
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: DEFAULT_COORDINATION_STALE_AFTER_MS,
        task_lens: None,
        output_options: ContextPackOutputOptions::default()
            .with_resource_profile(PackResourceProfile::Standard),
        persist_pack: false,
        baseline_write: None,
        no_lod: false,
    }
}

fn write_raw_frame(stream: &mut UnixStream, body: &[u8]) -> TestResult {
    let length = u32::try_from(body.len()).map_err(|error| format!("frame too large: {error}"))?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(|error| format!("write length: {error}"))?;
    stream
        .write_all(body)
        .map_err(|error| format!("write body: {error}"))?;
    stream.flush().map_err(|error| format!("flush: {error}"))
}

fn read_response_frame(stream: &mut UnixStream) -> Result<DaemonResponse, String> {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .map_err(|error| format!("read response length: {error}"))?;
    let announced = u32::from_be_bytes(prefix);
    let announced_usize = usize::try_from(announced)
        .map_err(|error| format!("response length conversion failed: {error}"))?;
    ensure(
        announced_usize <= DAEMON_RESPONSE_MAX_BYTES,
        format!("response announced {announced_usize} bytes above cap {DAEMON_RESPONSE_MAX_BYTES}"),
    )?;
    let mut body = vec![0_u8; announced_usize];
    stream
        .read_exact(&mut body)
        .map_err(|error| format!("read response body: {error}"))?;
    serde_json::from_slice(&body).map_err(|error| format!("decode response: {error}"))
}

fn spawn_one_response_daemon(
    socket_path: &Path,
    response: serde_json::Value,
) -> Result<thread::JoinHandle<TestResult>, String> {
    let listener = UnixListener::bind(socket_path).map_err(|error| format!("bind: {error}"))?;
    let response_body =
        serde_json::to_vec(&response).map_err(|error| format!("encode response: {error}"))?;

    Ok(thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .map_err(|error| format!("accept: {error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| format!("set_read_timeout: {error}"))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| format!("set_write_timeout: {error}"))?;

        let mut request_prefix = [0_u8; 4];
        stream
            .read_exact(&mut request_prefix)
            .map_err(|error| format!("read request length: {error}"))?;
        let announced = u32::from_be_bytes(request_prefix);
        let announced_usize = usize::try_from(announced)
            .map_err(|error| format!("request length conversion failed: {error}"))?;
        let mut request_body = vec![0_u8; announced_usize];
        stream
            .read_exact(&mut request_body)
            .map_err(|error| format!("read request body: {error}"))?;

        let response_length = u32::try_from(response_body.len())
            .map_err(|error| format!("response frame too large: {error}"))?;
        stream
            .write_all(&response_length.to_be_bytes())
            .map_err(|error| format!("write response length: {error}"))?;
        stream
            .write_all(&response_body)
            .map_err(|error| format!("write response body: {error}"))?;
        stream.flush().map_err(|error| format!("flush: {error}"))
    }))
}

fn ensure_peer_closed_without_response(stream: &mut UnixStream) -> TestResult {
    let mut prefix = [0_u8; 4];
    match stream.read_exact(&mut prefix) {
        Ok(()) => {
            let announced = u32::from_be_bytes(prefix);
            Err(format!(
                "expected daemon to close without a response frame; got response length prefix {announced}"
            ))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(format!(
            "expected daemon to close connection without a response frame; read response length failed with {error}"
        )),
    }
}

fn ensure_error_code(response: &DaemonResponse, expected: &str) -> TestResult {
    ensure(
        response.schema == DAEMON_RESPONSE_SCHEMA_V1,
        format!(
            "error response schema must be {DAEMON_RESPONSE_SCHEMA_V1}; got {}",
            response.schema
        ),
    )?;
    ensure(
        response.result.is_none(),
        format!("error response must not contain result; got {response:?}"),
    )?;
    let error = response
        .error
        .as_ref()
        .ok_or_else(|| format!("response must contain error; got {response:?}"))?;
    ensure(
        error.code == expected,
        format!("error code must be {expected}; got {}", error.code),
    )
}

#[test]
fn client_round_trip_rejects_response_schema_mismatch() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-client-schema-drift.sock");

    let server = spawn_one_response_daemon(
        &socket_path,
        serde_json::json!({
            "schema": "ee.daemon.response.v2",
            "request_id": "req-client-schema-drift",
            "agent_id": TEST_AGENT_ID,
            "result": {"ok": true}
        }),
    )?;
    let request = context_request(
        "req-client-schema-drift",
        TEST_AGENT_ID,
        serde_json::json!({"task": "schema drift"}),
    );
    let error = client_round_trip(&socket_path, &request)
        .expect_err("client must reject daemon response schema drift");
    server
        .join()
        .map_err(|_| "fake daemon thread panicked".to_owned())??;

    match error {
        ClientError::ResponseSchemaMismatch { expected, actual } => {
            ensure(
                expected == DAEMON_RESPONSE_SCHEMA_V1,
                format!("expected schema must be {DAEMON_RESPONSE_SCHEMA_V1}; got {expected}"),
            )?;
            ensure(
                actual == "ee.daemon.response.v2",
                format!("actual schema must report the daemon value; got {actual}"),
            )
        }
        other => Err(format!(
            "schema drift must return ResponseSchemaMismatch; got {other:?}"
        )),
    }
}

#[test]
fn client_round_trip_rejects_response_request_id_mismatch() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-client-request-id-drift.sock");

    let server = spawn_one_response_daemon(
        &socket_path,
        serde_json::json!({
            "schema": DAEMON_RESPONSE_SCHEMA_V1,
            "request_id": "req-attacker-chosen",
            "agent_id": TEST_AGENT_ID,
            "result": {"ok": true}
        }),
    )?;
    let request = context_request(
        "req-client-request-id",
        TEST_AGENT_ID,
        serde_json::json!({"task": "request id drift"}),
    );
    let error = client_round_trip(&socket_path, &request)
        .expect_err("client must reject daemon response request_id drift");
    server
        .join()
        .map_err(|_| "fake daemon thread panicked".to_owned())??;

    match error {
        ClientError::ResponseRequestIdMismatch { expected, actual } => {
            ensure(
                expected == "req-client-request-id",
                format!("expected request_id must be the sent value; got {expected}"),
            )?;
            ensure(
                actual == "req-attacker-chosen",
                format!("actual request_id must report the daemon value; got {actual}"),
            )
        }
        other => Err(format!(
            "request_id drift must return ResponseRequestIdMismatch; got {other:?}"
        )),
    }
}

#[test]
fn client_round_trip_rejects_response_agent_id_mismatch() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-client-agent-id-drift.sock");

    let server = spawn_one_response_daemon(
        &socket_path,
        serde_json::json!({
            "schema": DAEMON_RESPONSE_SCHEMA_V1,
            "request_id": "req-client-agent-id",
            "agent_id": "agent-attacker-chosen",
            "workspace_id": TEST_WORKSPACE_ID,
            "result": {"ok": true}
        }),
    )?;
    let request = context_request(
        "req-client-agent-id",
        TEST_AGENT_ID,
        serde_json::json!({"task": "agent id drift"}),
    );
    let error = client_round_trip(&socket_path, &request)
        .expect_err("client must reject daemon response agent_id drift");
    server
        .join()
        .map_err(|_| "fake daemon thread panicked".to_owned())??;

    match error {
        ClientError::ResponseAgentIdMismatch { expected, actual } => {
            ensure(
                expected == TEST_AGENT_ID,
                format!("expected agent_id must be the sent value; got {expected}"),
            )?;
            ensure(
                actual == "agent-attacker-chosen",
                format!("actual agent_id must report the daemon value; got {actual}"),
            )
        }
        other => Err(format!(
            "agent_id drift must return ResponseAgentIdMismatch; got {other:?}"
        )),
    }
}

#[test]
fn client_round_trip_rejects_response_workspace_id_mismatch() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-client-workspace-id-drift.sock");

    let server = spawn_one_response_daemon(
        &socket_path,
        serde_json::json!({
            "schema": DAEMON_RESPONSE_SCHEMA_V1,
            "request_id": "req-client-workspace-id",
            "agent_id": TEST_AGENT_ID,
            "workspace_id": "workspace-attacker-chosen",
            "result": {"ok": true}
        }),
    )?;
    let request = context_request(
        "req-client-workspace-id",
        TEST_AGENT_ID,
        serde_json::json!({"task": "workspace id drift"}),
    );
    let error = client_round_trip(&socket_path, &request)
        .expect_err("client must reject daemon response workspace_id drift");
    server
        .join()
        .map_err(|_| "fake daemon thread panicked".to_owned())??;

    match error {
        ClientError::ResponseWorkspaceIdMismatch { expected, actual } => {
            ensure(
                expected.as_deref() == Some(TEST_WORKSPACE_ID),
                format!("expected workspace_id must be the sent value; got {expected:?}"),
            )?;
            ensure(
                actual.as_deref() == Some("workspace-attacker-chosen"),
                format!("actual workspace_id must report the daemon value; got {actual:?}"),
            )
        }
        other => Err(format!(
            "workspace_id drift must return ResponseWorkspaceIdMismatch; got {other:?}"
        )),
    }
}

#[test]
fn daemon_echo_disabled_by_default_returns_error_envelope() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-rt.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let request = DaemonRequest::new(
        "req-echo-roundtrip-001",
        TEST_AGENT_ID,
        METHOD_ECHO,
        serde_json::json!({
            "hello": "world",
            "n": 42,
            "nested": {"k": "v"},
            "token": "sk_live_abcdefghijklmnopqrstuvwxyz0123456789"
        }),
    );

    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;

    ensure(
        response.schema == DAEMON_RESPONSE_SCHEMA_V1,
        format!(
            "response schema must be {DAEMON_RESPONSE_SCHEMA_V1}; got {}",
            response.schema
        ),
    )?;
    ensure(
        response.request_id == "req-echo-roundtrip-001",
        format!(
            "request_id must echo unchanged; got {}",
            response.request_id
        ),
    )?;
    ensure(
        response.agent_id == TEST_AGENT_ID,
        format!("agent_id must echo unchanged; got {}", response.agent_id),
    )?;
    ensure_error_code(&response, DAEMON_ECHO_DISABLED_CODE)?;
    let error = response
        .error
        .as_ref()
        .ok_or_else(|| format!("echo-disabled response must contain error; got {response:?}"))?;
    ensure(
        !error.message.contains("sk_live_"),
        format!(
            "echo disabled message must not reflect params; got {}",
            error.message
        ),
    )?;
    ensure(
        response.degraded_codes.is_empty(),
        format!(
            "echo disabled is a structured method error, not a response degradation; got {:?}",
            response.degraded_codes
        ),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    ensure(
        !socket_path.exists(),
        "socket file must be unlinked after shutdown".to_owned(),
    )?;
    Ok(())
}

#[test]
fn daemon_capabilities_advertises_schema_and_method_contract_over_wire() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-capabilities.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let request = DaemonRequest::new(
        "req-capabilities-roundtrip-001",
        TEST_AGENT_ID,
        METHOD_CAPABILITIES,
        serde_json::json!({}),
    );
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;

    ensure(
        response.schema == DAEMON_RESPONSE_SCHEMA_V1,
        format!(
            "response schema must be {DAEMON_RESPONSE_SCHEMA_V1}; got {}",
            response.schema
        ),
    )?;
    ensure(
        response.request_id == "req-capabilities-roundtrip-001",
        format!(
            "request_id must echo unchanged; got {}",
            response.request_id
        ),
    )?;
    ensure(
        response.agent_id == TEST_AGENT_ID,
        format!("agent_id must echo unchanged; got {}", response.agent_id),
    )?;
    ensure(
        response.error.is_none(),
        format!("capabilities must succeed; got {:?}", response.error),
    )?;
    ensure(
        response.degraded_codes.is_empty(),
        format!(
            "capabilities is discovery, not a degraded response; got {:?}",
            response.degraded_codes
        ),
    )?;

    let result = response
        .result
        .as_ref()
        .ok_or_else(|| format!("capabilities response missing result; got {response:?}"))?;
    ensure(
        result
            .pointer("/protocol")
            .and_then(serde_json::Value::as_str)
            == Some("ee.daemon"),
        format!("capabilities protocol missing or wrong; got {result}"),
    )?;
    ensure(
        result.get("request_schemas") == Some(&serde_json::json!([DAEMON_REQUEST_SCHEMA_V1])),
        format!("capabilities request_schemas wrong; got {result}"),
    )?;
    ensure(
        result.get("response_schemas") == Some(&serde_json::json!([DAEMON_RESPONSE_SCHEMA_V1])),
        format!("capabilities response_schemas wrong; got {result}"),
    )?;
    ensure(
        result.get("methods")
            == Some(&serde_json::json!([
                METHOD_CAPABILITIES,
                METHOD_CONTEXT,
                METHOD_ECHO,
                METHOD_SEARCH,
                METHOD_SHUTDOWN,
                METHOD_TELEMETRY,
                METHOD_WRITE,
                METHOD_WRITE_JOURNAL
            ])),
        format!("capabilities methods wrong; got {result}"),
    )?;
    ensure(
        result
            .pointer("/method_schemas/ee.daemon.search/request")
            .and_then(serde_json::Value::as_str)
            == Some(DAEMON_SEARCH_REQUEST_SCHEMA_V2),
        format!("search request schema capability wrong; got {result}"),
    )?;
    ensure(
        result
            .pointer("/method_schemas/ee.daemon.search/response")
            .and_then(serde_json::Value::as_str)
            == Some(DAEMON_SEARCH_RESPONSE_SCHEMA_V3),
        format!("search response schema capability wrong; got {result}"),
    )?;
    ensure(
        result
            .pointer("/authorization/ee.daemon.context")
            .and_then(serde_json::Value::as_str)
            == Some("same_uid_workspace"),
        format!("capabilities authorization wrong; got {result}"),
    )?;
    ensure(
        result
            .pointer("/forward_compat/v1_unknown_fields")
            .and_then(serde_json::Value::as_str)
            == Some("rejected"),
        format!("strict v1 unknown-field policy missing; got {result}"),
    )?;
    ensure(
        result
            .pointer("/forward_compat/v1_unknown_methods")
            .and_then(serde_json::Value::as_str)
            == Some(DAEMON_UNKNOWN_METHOD_CODE),
        format!("strict v1 unknown-method policy missing; got {result}"),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_search_reuses_one_process_and_returns_stable_results() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-search.sock")?;
    let (workspace, database) = seed_context_workspace(temp.path())?;
    let index_dir = workspace.join(".ee").join("index");
    let rebuild = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .map_err(|error| format!("rebuild search index: {error}"))?;
    ensure(
        rebuild.status == IndexRebuildStatus::Success,
        format!(
            "search index rebuild must succeed; got {:?}",
            rebuild.status
        ),
    )?;

    let workspace_id = workspace.display().to_string();
    let mut handle = start_server_for_workspace(&socket_path, workspace_id)
        .map_err(|error| format!("start_server_for_workspace: {error}"))?;

    let first = client_round_trip(
        handle.socket_path(),
        &search_request("req-search-warm-001", &workspace, &database, &index_dir),
    )
    .map_err(|error| format!("first search round-trip: {error}"))?;
    let second = client_round_trip(
        handle.socket_path(),
        &search_request("req-search-warm-002", &workspace, &database, &index_dir),
    )
    .map_err(|error| format!("second search round-trip: {error}"))?;
    let third = client_round_trip(
        handle.socket_path(),
        &search_request("req-search-warm-003", &workspace, &database, &index_dir),
    )
    .map_err(|error| format!("third search round-trip: {error}"))?;
    let mut context_params = context_pack_params(
        &workspace,
        &database,
        "release provenance daemon context advisory",
    );
    context_params["sourceMode"] = serde_json::json!("hybrid");
    context_params["indexDir"] = serde_json::json!(index_dir.display().to_string());
    let mut first_context_request = DaemonRequest::new(
        "req-context-warm-001",
        TEST_AGENT_ID,
        METHOD_CONTEXT,
        context_params.clone(),
    );
    first_context_request.workspace_id = Some(workspace.display().to_string());
    let first_context = client_round_trip(handle.socket_path(), &first_context_request)
        .map_err(|error| format!("first context round-trip: {error}"))?;
    let mut second_context_request = DaemonRequest::new(
        "req-context-warm-002",
        TEST_AGENT_ID,
        METHOD_CONTEXT,
        context_params,
    );
    second_context_request.workspace_id = Some(workspace.display().to_string());
    let second_context = client_round_trip(handle.socket_path(), &second_context_request)
        .map_err(|error| format!("second context round-trip: {error}"))?;
    const SECRET_QUERY: &str = "release provenance sk_live_uds_query_must_not_escape_performance";
    let privacy = client_round_trip(
        handle.socket_path(),
        &search_request_with_query(
            "req-search-warm-privacy",
            &workspace,
            &database,
            &index_dir,
            SECRET_QUERY,
            true,
        ),
    )
    .map_err(|error| format!("privacy search round-trip: {error}"))?;
    ensure(
        first.error.is_none(),
        format!("first search failed: {first:?}"),
    )?;
    ensure(
        second.error.is_none(),
        format!("second search failed: {second:?}"),
    )?;
    ensure(
        third.error.is_none(),
        format!("third search failed: {third:?}"),
    )?;
    ensure(
        privacy.error.is_none(),
        format!("privacy search failed: {privacy:?}"),
    )?;
    ensure(
        first_context.error.is_none() && second_context.error.is_none(),
        format!(
            "mixed long-lived context requests failed: first={first_context:?}; second={second_context:?}"
        ),
    )?;
    let privacy_result = privacy
        .result
        .as_ref()
        .ok_or_else(|| "privacy search result missing".to_owned())?;
    let performance = privacy_result
        .get("performance")
        .ok_or_else(|| format!("privacy search performance missing: {privacy_result}"))?;
    let rendered_performance = serde_json::to_string(performance)
        .map_err(|error| format!("serialize privacy performance: {error}"))?;
    let fallbacks = performance
        .pointer("/data/fallbacks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("privacy search fallbacks missing: {performance}"))?;
    ensure(
        performance
            .pointer("/data/redaction/queryTextIncluded")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
            && fallbacks
                .iter()
                .any(|fallback| fallback["code"] == "no_relevant_results")
            && !rendered_performance.contains(SECRET_QUERY)
            && !rendered_performance.contains("sk_live_uds_query"),
        format!("daemon performance leaked planted query: {rendered_performance}"),
    )?;
    DaemonSearchResult::from_value(privacy_result.clone())
        .map_err(|error| format!("privacy method response validation: {error}"))?;
    for (ordinal, response) in [("first", &first), ("second", &second), ("third", &third)] {
        ensure(
            !response
                .degraded_codes
                .iter()
                .any(|code| code == "rerank_model_unavailable"),
            format!(
                "{ordinal} daemon response must not repeat permanent reranker posture in outer degraded_codes: {:?}",
                response.degraded_codes
            ),
        )?;
    }
    let first_result = first
        .result
        .ok_or_else(|| "first search result missing".to_owned())?;
    let second_result = second
        .result
        .ok_or_else(|| "second search result missing".to_owned())?;
    let third_result = third
        .result
        .ok_or_else(|| "third search result missing".to_owned())?;
    for result in [&first_result, &second_result, &third_result] {
        ensure(
            result
                .pointer("/schema")
                .and_then(serde_json::Value::as_str)
                == Some(DAEMON_SEARCH_RESPONSE_SCHEMA_V3),
            format!("method response schema drifted: {result}"),
        )?;
        ensure(
            result
                .pointer("/response/schema")
                .and_then(serde_json::Value::as_str)
                == Some("ee.response.v2"),
            format!("canonical response schema drifted: {result}"),
        )?;
        ensure(
            result
                .pointer("/reuseContract/daemonProcess")
                .and_then(serde_json::Value::as_str)
                == Some("long_lived"),
            format!("daemon process reuse contract drifted: {result}"),
        )?;
        ensure(
            result
                .pointer("/reuseContract/defaultSearchEmbedder")
                .and_then(serde_json::Value::as_str)
                == Some("process_scoped"),
            format!("embedder reuse contract drifted: {result}"),
        )?;
        ensure(
            result
                .pointer("/reuseContract/searchIndex")
                .and_then(serde_json::Value::as_str)
                == Some("per_request"),
            format!("search index reuse contract drifted: {result}"),
        )?;
        let timing = result
            .pointer("/timing")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("strict timing object missing: {result}"))?;
        ensure(
            timing
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == ["daemonTotal", "embedderPreparation", "indexOpen", "query"]
                    .into_iter()
                    .collect(),
            format!("strict timing field set drifted: {result}"),
        )?;
        for field in ["daemonTotal", "indexOpen", "query"] {
            let measurement = timing
                .get(field)
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| format!("timing.{field} missing for indexed search: {result}"))?;
            ensure(
                measurement
                    .keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    == ["elapsedMs", "elapsedMsBucket", "nondeterministic"]
                        .into_iter()
                        .collect()
                    && measurement
                        .get("elapsedMs")
                        .and_then(serde_json::Value::as_f64)
                        .is_some_and(|elapsed| elapsed >= 0.0)
                    && measurement
                        .get("nondeterministic")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true),
                format!("timing.{field} contract drifted: {result}"),
            )?;
        }
        DaemonSearchResult::from_value(result.clone())
            .map_err(|error| format!("strict method response validation: {error}"))?;
    }
    ensure(
        first_result.pointer("/response/data/results")
            == second_result.pointer("/response/data/results")
            && second_result.pointer("/response/data/results")
                == third_result.pointer("/response/data/results"),
        format!(
            "repeated warm-daemon calls must preserve ranked results; first={first_result}; second={second_result}; third={third_result}"
        ),
    )?;
    ensure(
        first_result
            .pointer("/response/data/rerank/advisory/permanent")
            .and_then(serde_json::Value::as_bool)
            == Some(true),
        format!("first daemon query must emit permanent advisory once: {first_result}"),
    )?;
    ensure(
        first_result
            .pointer("/response/data/rerank/advisorySummary/scope")
            .and_then(serde_json::Value::as_str)
            == Some(SEARCH_ADVISORY_SCOPE_PROCESS),
        format!("daemon advisory scope must describe bounded workspace episodes: {first_result}"),
    )?;
    ensure(
        first_result
            .pointer("/response/data/rerank/advisorySummary/emittedCount")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
            && first_result
                .pointer("/response/data/rerank/advisorySummary/suppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0),
        format!("first daemon query advisory summary drifted: {first_result}"),
    )?;
    ensure(
        second_result
            .pointer("/response/data/rerank/advisory")
            .is_some_and(serde_json::Value::is_null),
        format!("second daemon query must suppress repeated advisory: {second_result}"),
    )?;
    ensure(
        second_result
            .pointer("/response/data/rerank/advisorySummary/permanent")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && second_result
                .pointer("/response/data/rerank/advisorySummary/emittedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0)
            && second_result
                .pointer("/response/data/rerank/advisorySummary/suppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1)
            && second_result
                .pointer("/response/data/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(serde_json::Value::as_u64)
                == Some(2)
            && second_result
                .pointer("/response/data/rerank/advisorySummary/sessionSuppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
        format!("second daemon query suppression summary drifted: {second_result}"),
    )?;
    ensure(
        third_result
            .pointer("/response/data/rerank/advisory")
            .is_some_and(serde_json::Value::is_null)
            && third_result
                .pointer("/response/data/rerank/advisorySummary/emittedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0)
            && third_result
                .pointer("/response/data/rerank/advisorySummary/suppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1)
            && third_result
                .pointer("/response/data/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(serde_json::Value::as_u64)
                == Some(3)
            && third_result
                .pointer("/response/data/rerank/advisorySummary/sessionSuppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(2),
        format!("third daemon query suppression summary drifted: {third_result}"),
    )?;
    let first_context_result = first_context
        .result
        .as_ref()
        .ok_or_else(|| "first mixed context result missing".to_owned())?;
    let second_context_result = second_context
        .result
        .as_ref()
        .ok_or_else(|| "second mixed context result missing".to_owned())?;
    for (ordinal, result, occurrence_count, suppressed_count) in [
        ("first", first_context_result, 4_u64, 3_u64),
        ("second", second_context_result, 5_u64, 4_u64),
    ] {
        ensure(
            result
                .pointer("/data/rerank/advisory")
                .is_some_and(serde_json::Value::is_null)
                && result
                    .pointer("/data/rerank/advisorySummary/permanent")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                && result
                    .pointer("/data/rerank/advisorySummary/scope")
                    .and_then(serde_json::Value::as_str)
                    == Some(SEARCH_ADVISORY_SCOPE_PROCESS)
                && result
                    .pointer("/data/rerank/advisorySummary/sessionOccurrenceCount")
                    .and_then(serde_json::Value::as_u64)
                    == Some(occurrence_count)
                && result
                    .pointer("/data/rerank/advisorySummary/sessionSuppressedCount")
                    .and_then(serde_json::Value::as_u64)
                    == Some(suppressed_count),
            format!(
                "{ordinal} mixed context request did not share cumulative process advisory state: {result}"
            ),
        )?;
        for pointer in ["/degraded", "/data/degraded"] {
            ensure(
                degraded_codes_at(result, pointer, "mixed context")?
                    .into_iter()
                    .all(|code| code != "rerank_model_unavailable"),
                format!(
                    "{ordinal} mixed context request repeated permanent posture as degradation: {result}"
                ),
            )?;
        }
    }
    ensure(
        first_result
            .pointer("/response/data/results/0/docId")
            .and_then(serde_json::Value::as_str)
            == Some("mem_00000000000000000000005001"),
        format!("seeded memory missing from daemon search: {first_result}"),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_context_binds_workspace_and_shares_canonical_advisory_partition() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-context-binding.sock")?;
    let (workspace_a, database_a) = seed_context_workspace(&temp.path().join("workspace-a"))?;
    let (workspace_b, database_b) = seed_context_workspace(&temp.path().join("workspace-b"))?;
    let index_a = workspace_a.join(".ee").join("index");
    let index_b = workspace_b.join(".ee").join("index");
    rebuild_test_index(&workspace_a, &database_a, &index_a)?;
    rebuild_test_index(&workspace_b, &database_b, &index_b)?;
    let workspace_a_alias = temp.path().join("workspace-a-alias");
    std::os::unix::fs::symlink(&workspace_a, &workspace_a_alias)
        .map_err(|error| format!("create workspace alias: {error}"))?;

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let mismatched = client_round_trip(
        handle.socket_path(),
        &hybrid_context_request_for_workspace(
            "req-context-workspace-mismatch",
            &workspace_b,
            &workspace_a,
            &database_b,
            &index_b,
            "mismatched context workspace must not execute",
        ),
    )
    .map_err(|error| format!("mismatched context round-trip: {error}"))?;
    ensure(
        mismatched.result.is_none()
            && mismatched.error.as_ref().is_some_and(|error| {
                error.code == DAEMON_CONTEXT_PARAMS_INVALID_CODE
                    && error.message.contains(
                        "workspacePath` must identify the authorized envelope `workspace_id",
                    )
            }),
        format!("valid-envelope/mismatched context params were not rejected: {mismatched:?}"),
    )?;

    plant_large_index_gap(&database_a, 600_000, "workspace-a-context-alias-first")?;
    let context_first = successful_result(
        client_round_trip(
            handle.socket_path(),
            &hybrid_context_request_for_workspace(
                "req-context-alias-first",
                &workspace_a_alias,
                &workspace_a_alias,
                &database_a,
                &index_a,
                "context alias emits the first process advisory",
            ),
        )
        .map_err(|error| format!("context-first alias round-trip: {error}"))?,
        "context-first alias request",
    )?;
    assert_stale_episode(
        &context_first,
        "/data/degraded",
        true,
        true,
        "context_alias_first",
    )?;
    ensure(
        context_first
            .pointer("/data/rerank/advisory/code")
            .and_then(serde_json::Value::as_str)
            == Some("rerank_model_unavailable")
            && context_first
                .pointer("/data/rerank/advisory/permanent")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            && context_first
                .pointer("/data/rerank/advisorySummary/scope")
                .and_then(serde_json::Value::as_str)
                == Some(SEARCH_ADVISORY_SCOPE_PROCESS)
            && context_first
                .pointer("/data/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
        format!("context-first request did not emit the process advisory: {context_first}"),
    )?;

    let search_repeated = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-search-canonical-repeat",
                &workspace_a,
                &database_a,
                &index_a,
            ),
        )
        .map_err(|error| format!("canonical search repeat round-trip: {error}"))?,
        "canonical search repeat",
    )?;
    assert_stale_episode(
        &search_repeated,
        "/response/data/degraded",
        false,
        false,
        "search_canonical_repeat",
    )?;
    ensure(
        search_repeated
            .pointer("/response/data/rerank/advisory")
            .is_some_and(serde_json::Value::is_null)
            && search_repeated
                .pointer("/response/data/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(serde_json::Value::as_u64)
                == Some(2)
            && search_repeated
                .pointer("/response/data/rerank/advisorySummary/sessionSuppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
        format!("canonical search did not share context advisory state: {search_repeated}"),
    )?;

    let context_repeated = successful_result(
        client_round_trip(
            handle.socket_path(),
            &hybrid_context_request_for_workspace(
                "req-context-canonical-repeat",
                &workspace_a,
                &workspace_a,
                &database_a,
                &index_a,
                "canonical context repeats the process advisory",
            ),
        )
        .map_err(|error| format!("canonical context repeat round-trip: {error}"))?,
        "canonical context repeat",
    )?;
    assert_stale_episode(
        &context_repeated,
        "/data/degraded",
        true,
        false,
        "context_canonical_repeat",
    )?;
    ensure(
        context_repeated
            .pointer("/data/rerank/advisory")
            .is_some_and(serde_json::Value::is_null)
            && context_repeated
                .pointer("/data/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(serde_json::Value::as_u64)
                == Some(3)
            && context_repeated
                .pointer("/data/rerank/advisorySummary/sessionSuppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(2),
        format!("canonical context did not suppress the repeated advisory: {context_repeated}"),
    )?;

    plant_large_index_gap(&database_b, 700_000, "workspace-b-independent")?;
    let workspace_b_first = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-search-workspace-b-independent",
                &workspace_b,
                &database_b,
                &index_b,
            ),
        )
        .map_err(|error| format!("workspace B first round-trip: {error}"))?,
        "workspace B first advisory episode",
    )?;
    assert_stale_episode(
        &workspace_b_first,
        "/response/data/degraded",
        true,
        true,
        "workspace_b_independent",
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_advisory_active_episode_lifecycle_is_real_and_workspace_partitioned() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-advisory-episodes.sock")?;
    let (workspace_a, database_a) = seed_context_workspace(&temp.path().join("workspace-a"))?;
    let (workspace_b, database_b) = seed_context_workspace(&temp.path().join("workspace-b"))?;
    let index_a = workspace_a.join(".ee").join("index");
    let index_b = workspace_b.join(".ee").join("index");
    rebuild_test_index(&workspace_a, &database_a, &index_a)?;
    rebuild_test_index(&workspace_b, &database_b, &index_b)?;

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    plant_large_index_gap(&database_a, 100_000, "workspace-a-search-first")?;
    let search_first = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-advisory-search-first",
                &workspace_a,
                &database_a,
                &index_a,
            ),
        )
        .map_err(|error| format!("search first round-trip: {error}"))?,
        "workspace A first stale search",
    )?;
    let search_repeated = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-advisory-search-repeated",
                &workspace_a,
                &database_a,
                &index_a,
            ),
        )
        .map_err(|error| format!("search repeated round-trip: {error}"))?,
        "workspace A repeated stale search",
    )?;
    assert_stale_episode(
        &search_first,
        "/response/data/degraded",
        true,
        true,
        "search_first_stale",
    )?;
    assert_stale_episode(
        &search_repeated,
        "/response/data/degraded",
        false,
        false,
        "search_repeated_stale",
    )?;
    assert_search_index_freshness(&search_first, "search_first_freshness")?;
    assert_search_index_freshness(&search_repeated, "search_repeated_freshness")?;
    ensure(
        search_first
            .pointer("/response/data/rerank/advisory/code")
            .and_then(serde_json::Value::as_str)
            == Some("rerank_model_unavailable")
            && search_first
                .pointer("/response/data/rerank/advisorySummary/scope")
                .and_then(serde_json::Value::as_str)
                == Some(SEARCH_ADVISORY_SCOPE_PROCESS)
            && search_first
                .pointer("/response/data/rerank/advisorySummary/emittedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
        format!("first permanent advisory contract drifted: {search_first}"),
    )?;
    ensure(
        search_repeated
            .pointer("/response/data/rerank/advisory")
            .is_some_and(serde_json::Value::is_null)
            && search_repeated
                .pointer("/response/data/rerank/advisorySummary/emittedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0)
            && search_repeated
                .pointer("/response/data/rerank/advisorySummary/suppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
        format!("repeated permanent advisory was not suppressed: {search_repeated}"),
    )?;

    rebuild_test_index(&workspace_a, &database_a, &index_a)?;
    let search_ready = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-advisory-search-ready",
                &workspace_a,
                &database_a,
                &index_a,
            ),
        )
        .map_err(|error| format!("search ready round-trip: {error}"))?,
        "workspace A ready search",
    )?;
    assert_stale_episode(
        &search_ready,
        "/response/data/degraded",
        false,
        false,
        "search_ready",
    )?;
    plant_large_index_gap(&database_a, 200_000, "workspace-a-search-new")?;
    let search_new_stale = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-advisory-search-new-stale",
                &workspace_a,
                &database_a,
                &index_a,
            ),
        )
        .map_err(|error| format!("search new-stale round-trip: {error}"))?,
        "workspace A new stale search",
    )?;
    assert_stale_episode(
        &search_new_stale,
        "/response/data/degraded",
        true,
        true,
        "search_new_stale",
    )?;

    rebuild_test_index(&workspace_a, &database_a, &index_a)?;
    let context_ready_before_episode = successful_result(
        client_round_trip(
            handle.socket_path(),
            &context_request_for_workspace(
                "req-advisory-context-ready-before",
                &workspace_a,
                &database_a,
                "release provenance context ready before stale episode",
            ),
        )
        .map_err(|error| format!("context ready-before round-trip: {error}"))?,
        "workspace A context ready before stale episode",
    )?;
    assert_stale_episode(
        &context_ready_before_episode,
        "/data/degraded",
        false,
        false,
        "context_ready_before_episode",
    )?;
    plant_large_index_gap(&database_a, 300_000, "workspace-a-context-first")?;
    let context_first = successful_result(
        client_round_trip(
            handle.socket_path(),
            &context_request_for_workspace(
                "req-advisory-context-first",
                &workspace_a,
                &database_a,
                "release provenance context first stale episode",
            ),
        )
        .map_err(|error| format!("context first round-trip: {error}"))?,
        "workspace A first stale context",
    )?;
    let context_repeated = successful_result(
        client_round_trip(
            handle.socket_path(),
            &context_request_for_workspace(
                "req-advisory-context-repeated",
                &workspace_a,
                &database_a,
                "release provenance context repeated stale episode",
            ),
        )
        .map_err(|error| format!("context repeated round-trip: {error}"))?,
        "workspace A repeated stale context",
    )?;
    assert_stale_episode(
        &context_first,
        "/data/degraded",
        true,
        true,
        "context_first_stale",
    )?;
    assert_stale_episode(
        &context_repeated,
        "/data/degraded",
        true,
        false,
        "context_repeated_stale",
    )?;

    rebuild_test_index(&workspace_a, &database_a, &index_a)?;
    let context_ready = successful_result(
        client_round_trip(
            handle.socket_path(),
            &context_request_for_workspace(
                "req-advisory-context-ready",
                &workspace_a,
                &database_a,
                "release provenance context authoritative ready",
            ),
        )
        .map_err(|error| format!("context ready round-trip: {error}"))?,
        "workspace A ready context",
    )?;
    assert_stale_episode(
        &context_ready,
        "/data/degraded",
        false,
        false,
        "context_ready",
    )?;
    plant_large_index_gap(&database_a, 400_000, "workspace-a-context-new")?;
    let context_new_stale = successful_result(
        client_round_trip(
            handle.socket_path(),
            &context_request_for_workspace(
                "req-advisory-context-new-stale",
                &workspace_a,
                &database_a,
                "release provenance context later stale episode",
            ),
        )
        .map_err(|error| format!("context new-stale round-trip: {error}"))?,
        "workspace A new stale context",
    )?;
    assert_stale_episode(
        &context_new_stale,
        "/data/degraded",
        true,
        true,
        "context_new_stale",
    )?;

    plant_large_index_gap(&database_b, 500_000, "workspace-b-first")?;
    let workspace_b_first = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-advisory-workspace-b-first",
                &workspace_b,
                &database_b,
                &index_b,
            ),
        )
        .map_err(|error| format!("workspace B first round-trip: {error}"))?,
        "workspace B first stale search",
    )?;
    assert_stale_episode(
        &workspace_b_first,
        "/response/data/degraded",
        true,
        true,
        "workspace_b_first_stale",
    )?;
    ensure(
        workspace_b_first
            .pointer("/response/data/rerank/advisory")
            .is_some_and(serde_json::Value::is_null)
            && workspace_b_first
                .pointer("/response/data/rerank/advisorySummary/emittedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0),
        format!(
            "process-lifetime permanent advisory re-emitted in workspace B: {workspace_b_first}"
        ),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
#[ignore = "requires EE_E2E_RERANK_MODEL_ARCHIVE accepted by production manifest verification"]
fn daemon_manifest_verified_reranker_archive_keeps_permanent_advisory_consumed_over_uds()
-> TestResult {
    let archive = std::env::var_os("EE_E2E_RERANK_MODEL_ARCHIVE")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "EE_E2E_RERANK_MODEL_ARCHIVE must point to an archive accepted by production manifest verification"
                .to_owned()
        })?;
    ensure(
        archive.is_file(),
        format!("reranker archive does not exist: {}", archive.display()),
    )?;
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-reranker-episode.sock")?;
    let (workspace, database) = seed_context_workspace(temp.path())?;
    let workspace_id = stable_test_workspace_id(&workspace)?;
    let index_dir = workspace.join(".ee").join("index");
    let model_store = temp.path().join("model-store");
    seed_rerank_candidates(&workspace, &database)?;
    rebuild_test_index(&workspace, &database, &index_dir)?;
    let mut handle = start_server_for_workspace(&socket_path, workspace.display().to_string())
        .map_err(|error| format!("start_server_for_workspace: {error}"))?;

    let first_absent = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-reranker-episode-absent-first",
                &workspace,
                &database,
                &index_dir,
            ),
        )
        .map_err(|error| format!("first absent round-trip: {error}"))?,
        "first absent reranker search",
    )?;
    let repeated_absent = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-reranker-episode-absent-repeated",
                &workspace,
                &database,
                &index_dir,
            ),
        )
        .map_err(|error| format!("repeated absent round-trip: {error}"))?,
        "repeated absent reranker search",
    )?;
    ensure(
        first_absent
            .pointer("/response/data/rerank/advisory/code")
            .and_then(serde_json::Value::as_str)
            == Some("rerank_model_unavailable")
            && first_absent
                .pointer("/response/data/rerank/advisorySummary/scope")
                .and_then(serde_json::Value::as_str)
                == Some(SEARCH_ADVISORY_SCOPE_PROCESS)
            && first_absent
                .pointer("/response/data/rerank/advisorySummary/emittedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
        format!("first permanent episode did not emit: {first_absent}"),
    )?;
    ensure(
        repeated_absent
            .pointer("/response/data/rerank/advisory")
            .is_some_and(serde_json::Value::is_null)
            && repeated_absent
                .pointer("/response/data/rerank/advisorySummary/suppressedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
        format!("repeated permanent episode did not suppress: {repeated_absent}"),
    )?;
    ensure(
        first_absent
            .pointer("/response/data/results")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|results| {
                !results.is_empty()
                    && results
                        .iter()
                        .all(|result| result.get("rerankScore").is_none())
            }),
        format!("absent runtime planted negative unexpectedly carried scores: {first_absent}"),
    )?;

    fetch_rerank_model(&ModelFetchOptions {
        workspace_path: &workspace,
        database_path: Some(&database),
        model_id: "rerank-default",
        from_file: Some(&archive),
        model_store_root: Some(&model_store),
    })
    .map_err(|error| format!("manifest-verified reranker import: {error}"))?;

    let available = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-reranker-episode-real-available",
                &workspace,
                &database,
                &index_dir,
            ),
        )
        .map_err(|error| format!("real available round-trip: {error}"))?,
        "real native reranker search",
    )?;
    let available_results = available
        .pointer("/response/data/results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("real native reranker result array missing: {available}"))?;
    ensure(
        available
            .pointer("/response/data/rerank/available")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && available
                .pointer("/response/data/rerank/mode")
                .and_then(serde_json::Value::as_str)
                == Some("reranked")
            && available
                .pointer("/response/data/rerank/rerankScoreCount")
                .and_then(serde_json::Value::as_u64)
                == u64::try_from(available_results.len()).ok()
            && available_results.len() >= 5
            && available_results.iter().all(|result| {
                result
                    .get("rerankScore")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(f64::is_finite)
            })
            && available
                .pointer("/response/data/rerank/advisory")
                .is_some_and(serde_json::Value::is_null),
        format!("native loader/inference did not produce real UDS rerank scores: {available}"),
    )?;

    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let reranker_entry = connection
        .list_model_registry_entries(&workspace_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|entry| entry.purpose == ModelPurpose::Reranker)
        .ok_or_else(|| "manifest-verified import did not register a reranker row".to_owned())?;
    let disabled_input = CreateModelRegistryInput {
        workspace_id: reranker_entry.workspace_id.clone(),
        provider: reranker_entry.provider,
        model_name: reranker_entry.model_name.clone(),
        purpose: reranker_entry.purpose,
        dimension: reranker_entry.dimension,
        distance_metric: reranker_entry.distance_metric,
        status: ModelRegistryStatus::Unavailable,
        version: reranker_entry.version.clone(),
        source_uri: reranker_entry.source_uri.clone(),
        content_hash: reranker_entry.content_hash.clone(),
        metadata_json: reranker_entry.metadata_json.clone(),
        last_checked_at: reranker_entry.last_checked_at.clone(),
    };
    ensure(
        connection
            .update_model_registry_entry(&reranker_entry.id, &disabled_input)
            .map_err(|error| format!("disable manifest-verified reranker row: {error}"))?,
        "reranker transition back to unavailable affected no row",
    )?;
    connection.close().map_err(|error| error.to_string())?;

    let later_absent = successful_result(
        client_round_trip(
            handle.socket_path(),
            &search_request(
                "req-reranker-episode-absent-later",
                &workspace,
                &database,
                &index_dir,
            ),
        )
        .map_err(|error| format!("later absent round-trip: {error}"))?,
        "later absent reranker search",
    )?;
    ensure(
        later_absent
            .pointer("/response/data/rerank/available")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
            && later_absent
                .pointer("/response/data/rerank/advisory")
                .is_some_and(serde_json::Value::is_null)
            && later_absent
                .pointer("/response/data/rerank/advisorySummary/emittedCount")
                .and_then(serde_json::Value::as_u64)
                == Some(0)
            && later_absent
                .pointer("/response/data/rerank/advisorySummary/sessionOccurrenceCount")
                .and_then(serde_json::Value::as_u64)
                == Some(3)
            && later_absent
                .pointer("/response/data/results")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|results| {
                    !results.is_empty()
                        && results
                            .iter()
                            .all(|result| result.get("rerankScore").is_none())
                }),
        format!("permanent advisory was not kept consumed after real recovery: {later_absent}"),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_context_returns_canonical_pack_response_with_provenance() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-ctx.sock")?;
    let (workspace, database) = seed_context_workspace(temp.path())?;

    let mut handle = start_server_for_workspace(&socket_path, TEST_WORKSPACE_ID)
        .map_err(|error| format!("start_server_for_workspace: {error}"))?;

    let request = context_request(
        "req-ctx-pack-001",
        TEST_AGENT_ID,
        context_pack_params(
            &workspace,
            &database,
            "release provenance daemon context canonical pack",
        ),
    );
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;
    ensure(
        response.agent_id == TEST_AGENT_ID,
        format!("agent_id must echo unchanged; got {}", response.agent_id),
    )?;
    ensure(
        response.workspace_id.as_deref() == Some("workspace-daemon-uds-test"),
        format!(
            "workspace_id must echo unchanged; got {:?}",
            response.workspace_id
        ),
    )?;
    ensure(
        response.error.is_none(),
        format!("context pack request must not return daemon error; got {response:?}"),
    )?;
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| format!("context pack request must return result; got {response:?}"))?;
    ensure(
        result.get("schema").and_then(serde_json::Value::as_str) == Some("ee.response.v2"),
        format!("daemon context result must be canonical response envelope; got {result}"),
    )?;
    ensure(
        result.get("success").and_then(serde_json::Value::as_bool) == Some(true),
        format!("daemon context canonical envelope must be successful; got {result}"),
    )?;
    ensure(
        result
            .pointer("/data/command")
            .and_then(serde_json::Value::as_str)
            == Some("pack"),
        format!("daemon context must execute canonical pack command; got {result}"),
    )?;
    ensure(
        result
            .pointer("/data/pack/schema")
            .and_then(serde_json::Value::as_str)
            == Some("ee.pack.v2"),
        format!("daemon context result must carry ee.pack.v2; got {result}"),
    )?;
    let memory_count = result
        .pointer("/data/pack/provenanceFooter/memoryCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    ensure(
        memory_count >= 1,
        format!("daemon context pack must expose provenance memory count; got {result}"),
    )?;
    let rendered = result.to_string();
    ensure(
        rendered.contains("mem_00000000000000000000005001"),
        format!("daemon context pack must include seeded memory id; got {result}"),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_context_zero_timeout_refuses_before_pack_execution() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-ctx-timeout.sock")?;
    let (workspace, database) = seed_context_workspace(temp.path())?;

    let mut handle = start_server_for_workspace(&socket_path, TEST_WORKSPACE_ID)
        .map_err(|error| format!("start_server_for_workspace: {error}"))?;

    let mut params = context_pack_params(&workspace, &database, "deadline should not run pack");
    params["timeoutMs"] = serde_json::json!(0);
    let request = context_request("req-ctx-timeout-001", TEST_AGENT_ID, params);
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;
    ensure_error_code(&response, DAEMON_CONTEXT_DEADLINE_EXCEEDED_CODE)?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_context_controlled_runner_honors_deadline_and_cancellation_before_db_work() -> TestResult
{
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path().join("workspace");
    let database = workspace.join(".ee").join("missing.db");
    let options = context_pack_options(
        &workspace,
        &database,
        "deadline and cancellation should win before storage",
    );

    let deadline_error =
        run_context_pack_with_performance_controlled(&options, "pack", Some(Duration::ZERO), None)
            .expect_err("pre-expired deadline must stop before storage access");
    match deadline_error {
        ContextPackError::DeadlineExceeded(reason) => ensure(
            cancel_message(&reason).contains("deadline"),
            format!("deadline error should name deadline, got: {reason:?}"),
        )?,
        other => {
            return Err(format!(
                "pre-expired deadline must produce DeadlineExceeded, got {other:?}"
            ));
        }
    }

    let shutdown = AtomicBool::new(true);
    let cancellation_error =
        run_context_pack_with_performance_controlled(&options, "pack", None, Some(&shutdown))
            .expect_err("pre-set cancellation flag must stop before storage access");
    match cancellation_error {
        ContextPackError::Cancelled(reason) => ensure(
            cancel_message(&reason).contains("shutdown"),
            format!("cancellation error should name shutdown, got: {reason:?}"),
        )?,
        other => {
            return Err(format!(
                "pre-set cancellation must produce Cancelled, got {other:?}"
            ));
        }
    }
    ensure(
        shutdown.load(Ordering::SeqCst),
        "test cancellation flag should remain set",
    )
}

#[test]
fn daemon_background_runner_cancellation_flag_cancels_pending_job_promptly() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let shutdown = Arc::new(AtomicBool::new(true));
    let mut runner = ManualRunner::new(
        RunnerOptions::new()
            .with_workspace_path(temp.path().join("workspace"))
            .with_cancellation_flag(Arc::clone(&shutdown)),
    );
    runner.schedule(
        JobType::HealthCheck,
        JobPriority::Normal,
        Some("daemon shutdown cancellation test".to_owned()),
    );

    let started = Instant::now();
    let report = runner.run_pending();

    ensure(report.was_cancelled, "runner must report cancellation")?;
    ensure(
        report.failed == 1,
        format!(
            "cancelled job should count as failed; got {}",
            report.failed
        ),
    )?;
    ensure(
        report.results.len() == 1,
        format!(
            "one pending job should produce one result; got {:?}",
            report.results
        ),
    )?;
    let result = &report.results[0];
    ensure(
        result.outcome.as_str() == "cancelled",
        format!("pending job should be cancelled; got {}", result.outcome),
    )?;
    ensure(
        result.items_processed == Some(0),
        format!(
            "cancelled job must not process items; got {:?}",
            result.items_processed
        ),
    )?;
    ensure(
        started.elapsed() < Duration::from_millis(250),
        format!(
            "cooperative daemon cancellation should be prompt; elapsed {:?}",
            started.elapsed()
        ),
    )?;
    ensure(
        shutdown.load(Ordering::SeqCst),
        "test cancellation flag should remain set",
    )
}

#[test]
fn daemon_score_decay_job_honors_shutdown_before_background_work() -> TestResult {
    let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    let shutdown = Arc::new(AtomicBool::new(true));
    let options = ScoreDecayJobOptions::new("wsp-daemon-score-decay-cancel")
        .with_cancellation_flag(Arc::clone(&shutdown));

    let started = Instant::now();
    let error = run_score_decay_job(&connection, &options)
        .expect_err("pre-set daemon shutdown flag must cancel score decay");

    ensure(
        error.contains("shutdown"),
        format!("score decay cancellation should name shutdown, got: {error}"),
    )?;
    ensure(
        started.elapsed() < Duration::from_millis(250),
        format!(
            "score decay cancellation should be prompt; elapsed {:?}",
            started.elapsed()
        ),
    )?;
    ensure(
        shutdown.load(Ordering::SeqCst),
        "test cancellation flag should remain set",
    )
}

#[test]
fn daemon_context_wrong_workspace_returns_method_unauthorized_over_wire() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = secure_socket_path(temp.path(), "ee-daemon-ctx-auth.sock")?;

    let mut handle = start_server_for_workspace(&socket_path, TEST_WORKSPACE_ID)
        .map_err(|error| format!("start_server_for_workspace: {error}"))?;

    let mut request = context_request(
        "req-ctx-auth-001",
        TEST_AGENT_ID,
        serde_json::json!({"task": "ship daemon skeleton"}),
    );
    request.workspace_id = Some("workspace-other".to_owned());
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;

    ensure(
        response.request_id == "req-ctx-auth-001",
        format!(
            "request_id must echo unchanged; got {}",
            response.request_id
        ),
    )?;
    ensure(
        response.workspace_id.as_deref() == Some("workspace-other"),
        format!(
            "workspace_id must echo caller value; got {:?}",
            response.workspace_id
        ),
    )?;
    ensure_error_code(&response, DAEMON_METHOD_UNAUTHORIZED_CODE)?;
    ensure(
        response
            .degraded_codes
            .contains(&DAEMON_METHOD_UNAUTHORIZED_CODE.to_owned()),
        format!(
            "method auth failure must attach degraded code; got {:?}",
            response.degraded_codes
        ),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_schema_mismatch_returns_error_envelope_over_wire() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-schema-mismatch.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let mut request = DaemonRequest::new(
        "req-schema-mismatch-001",
        TEST_AGENT_ID,
        METHOD_ECHO,
        serde_json::json!({"hello": "world"}),
    );
    request.schema = "ee.daemon.request.v0_wrong".to_owned();
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;

    ensure(
        response.request_id == "req-schema-mismatch-001",
        format!("request_id must round-trip; got {}", response.request_id),
    )?;
    ensure(
        response.agent_id == TEST_AGENT_ID,
        format!("agent_id must round-trip; got {}", response.agent_id),
    )?;
    ensure_error_code(&response, DAEMON_REQUEST_SCHEMA_MISMATCH_CODE)?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_unknown_method_returns_error_envelope_over_wire() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-unknown-method.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let request = DaemonRequest::new(
        "req-unknown-method-001",
        TEST_AGENT_ID,
        "ee.daemon.nope",
        serde_json::json!({"hello": "world"}),
    );
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;

    ensure(
        response.request_id == "req-unknown-method-001",
        format!("request_id must round-trip; got {}", response.request_id),
    )?;
    ensure(
        response.agent_id == TEST_AGENT_ID,
        format!("agent_id must round-trip; got {}", response.agent_id),
    )?;
    ensure_error_code(&response, DAEMON_UNKNOWN_METHOD_CODE)?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_malformed_json_returns_decode_failed_envelope_over_wire() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-malformed-json.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let mut stream = connect_client(handle.socket_path())?;
    write_raw_frame(
        &mut stream,
        br#"{"token":"sk_live_abcdefghijklmnopqrstuvwxyz0123456789","oops":"#,
    )?;
    let response = read_response_frame(&mut stream)?;

    ensure(
        response.request_id == "<unknown>",
        format!(
            "malformed request must use <unknown> request_id; got {}",
            response.request_id
        ),
    )?;
    ensure(
        response.agent_id == "<unknown>",
        format!(
            "malformed request must use <unknown> agent_id; got {}",
            response.agent_id
        ),
    )?;
    ensure_error_code(&response, DAEMON_REQUEST_DECODE_FAILED_CODE)?;
    let error = response
        .error
        .as_ref()
        .ok_or_else(|| format!("decode response must contain error; got {response:?}"))?;
    ensure(
        error.message == "request body failed to decode",
        format!("decode error must use fixed message; got {}", error.message),
    )?;
    ensure(
        !error.message.contains("sk_live_"),
        format!(
            "decode error must not reflect input bytes; got {}",
            error.message
        ),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_mid_frame_disconnect_closes_without_decode_failed_envelope() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-truncated-frame.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let announced = 64_u32;
    let mut stream = connect_client(handle.socket_path())?;
    stream
        .write_all(&announced.to_be_bytes())
        .map_err(|error| format!("write announced length: {error}"))?;
    stream
        .write_all(br#"{"partial":"#)
        .map_err(|error| format!("write partial body: {error}"))?;
    stream.flush().map_err(|error| format!("flush: {error}"))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| format!("shutdown write half: {error}"))?;

    ensure_peer_closed_without_response(&mut stream)?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_oversize_request_prefix_returns_decode_failed_without_body_allocation() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-oversize-prefix.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let oversized = u32::try_from(DAEMON_REQUEST_MAX_BYTES + 1)
        .map_err(|error| format!("request cap must fit u32 for test: {error}"))?;
    let mut stream = connect_client(handle.socket_path())?;
    stream
        .write_all(&oversized.to_be_bytes())
        .map_err(|error| format!("write oversize prefix: {error}"))?;
    stream.flush().map_err(|error| format!("flush: {error}"))?;
    let response = read_response_frame(&mut stream)?;

    ensure(
        response.request_id == "<unknown>",
        format!(
            "oversize request must use <unknown> request_id; got {}",
            response.request_id
        ),
    )?;
    ensure(
        response.agent_id == "<unknown>",
        format!(
            "oversize request must use <unknown> agent_id; got {}",
            response.agent_id
        ),
    )?;
    ensure_error_code(&response, DAEMON_REQUEST_DECODE_FAILED_CODE)?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_serves_two_clients_concurrently() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-concurrent-clients.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let socket_a = handle.socket_path().to_path_buf();
    let socket_b = handle.socket_path().to_path_buf();
    let client_a = thread::spawn(move || {
        let request = context_request(
            "req-concurrent-a",
            "agent-daemon-uds-a",
            serde_json::json!({"client": "a"}),
        );
        client_round_trip(&socket_a, &request).map_err(|error| format!("client a: {error}"))
    });
    let client_b = thread::spawn(move || {
        let request = context_request(
            "req-concurrent-b",
            "agent-daemon-uds-b",
            serde_json::json!({"client": "b"}),
        );
        client_round_trip(&socket_b, &request).map_err(|error| format!("client b: {error}"))
    });

    let response_a = client_a
        .join()
        .map_err(|_| "client a thread panicked".to_owned())??;
    let response_b = client_b
        .join()
        .map_err(|_| "client b thread panicked".to_owned())??;

    ensure(
        response_a.request_id == "req-concurrent-a",
        format!("client a request_id drifted: {}", response_a.request_id),
    )?;
    ensure(
        response_a.agent_id == "agent-daemon-uds-a",
        format!("client a agent_id drifted: {}", response_a.agent_id),
    )?;
    ensure(
        response_b.request_id == "req-concurrent-b",
        format!("client b request_id drifted: {}", response_b.request_id),
    )?;
    ensure(
        response_b.agent_id == "agent-daemon-uds-b",
        format!("client b agent_id drifted: {}", response_b.agent_id),
    )?;
    ensure_error_code(&response_a, DAEMON_CONTEXT_PARAMS_INVALID_CODE)?;
    ensure_error_code(&response_b, DAEMON_CONTEXT_PARAMS_INVALID_CODE)?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
    Ok(())
}

#[test]
fn daemon_shutdown_is_idempotent_across_repeated_calls_over_uds() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-idempotent-uds.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    handle
        .shutdown()
        .map_err(|error| format!("first shutdown: {error}"))?;
    ensure(
        !socket_path.exists(),
        "socket file must be unlinked after first shutdown",
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("second shutdown must be idempotent: {error}"))?;
    handle
        .shutdown()
        .map_err(|error| format!("third shutdown must be idempotent: {error}"))?;
    Ok(())
}

#[test]
fn daemon_drop_without_explicit_shutdown_unlinks_socket() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-drop-cleanup.sock");

    {
        let handle =
            start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;
        ensure(
            handle.socket_path().exists(),
            "socket file must exist while daemon handle is live",
        )?;
    }

    ensure(
        !socket_path.exists(),
        "dropping the daemon handle without explicit shutdown must unlink the socket",
    )?;
    Ok(())
}

#[test]
fn daemon_restart_on_same_path_after_shutdown_succeeds() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-restart-same-path.sock");

    let mut first =
        start_server(&socket_path).map_err(|error| format!("first start_server: {error}"))?;
    first
        .shutdown()
        .map_err(|error| format!("first shutdown: {error}"))?;
    ensure(
        !socket_path.exists(),
        "socket file must be absent before restarting on the same path",
    )?;

    let mut second =
        start_server(&socket_path).map_err(|error| format!("second start_server: {error}"))?;
    let request = context_request(
        "req-restart-same-path",
        TEST_AGENT_ID,
        serde_json::json!({"restart": true}),
    );
    let response = client_round_trip(second.socket_path(), &request)
        .map_err(|error| format!("client_round_trip after restart: {error}"))?;
    ensure(
        response.request_id == "req-restart-same-path",
        format!(
            "restarted daemon must echo request_id; got {}",
            response.request_id
        ),
    )?;
    ensure_error_code(&response, DAEMON_CONTEXT_PARAMS_INVALID_CODE)?;

    second
        .shutdown()
        .map_err(|error| format!("second shutdown: {error}"))?;
    ensure(
        !socket_path.exists(),
        "socket file must be unlinked after second shutdown",
    )?;
    Ok(())
}

#[test]
fn daemon_shutdown_unblocks_accept_loop_without_any_client_connection() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-idle-shutdown.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let started = Instant::now();
    handle
        .shutdown()
        .map_err(|error| format!("idle shutdown: {error}"))?;
    ensure(
        started.elapsed() < Duration::from_secs(1),
        format!(
            "idle shutdown should wake accept loop promptly; elapsed {:?}",
            started.elapsed()
        ),
    )?;
    ensure(
        !socket_path.exists(),
        "idle shutdown must unlink the socket",
    )?;
    Ok(())
}

#[test]
fn daemon_shutdown_during_connected_client_returns_structured_response() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-shutdown-client-race.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let mut stream = connect_client(handle.socket_path())?;

    let shutdown = thread::spawn(move || {
        handle
            .shutdown()
            .map_err(|error| format!("shutdown thread: {error}"))
    });
    thread::sleep(Duration::from_millis(25));

    let request = context_request(
        "req-shutdown-race",
        TEST_AGENT_ID,
        serde_json::json!({"race": "shutdown"}),
    );
    let body = serde_json::to_vec(&request).map_err(|error| format!("encode request: {error}"))?;
    write_raw_frame(&mut stream, &body)?;
    let response = read_response_frame(&mut stream)?;

    shutdown
        .join()
        .map_err(|_| "shutdown thread panicked".to_owned())??;

    ensure(
        response.schema == DAEMON_RESPONSE_SCHEMA_V1,
        format!(
            "shutdown race response schema must be {DAEMON_RESPONSE_SCHEMA_V1}; got {}",
            response.schema
        ),
    )?;
    let error = response
        .error
        .as_ref()
        .ok_or_else(|| format!("shutdown race must return an error envelope; got {response:?}"))?;
    ensure(
        error.code == DAEMON_SHUTTING_DOWN_CODE || error.code == DAEMON_CONTEXT_PARAMS_INVALID_CODE,
        format!(
            "shutdown race must return structured shutdown or normal context param error; got {}",
            error.code
        ),
    )?;
    if error.code == DAEMON_SHUTTING_DOWN_CODE {
        ensure(
            response
                .degraded_codes
                .contains(&DAEMON_SHUTTING_DOWN_CODE.to_owned()),
            format!(
                "shutdown response must carry degraded code; got {:?}",
                response.degraded_codes
            ),
        )?;
    } else {
        ensure(
            response.request_id == "req-shutdown-race",
            format!(
                "normal race response must echo request_id; got {}",
                response.request_id
            ),
        )?;
        ensure(
            response.agent_id == TEST_AGENT_ID,
            format!(
                "normal race response must echo agent_id; got {}",
                response.agent_id
            ),
        )?;
    }
    Ok(())
}

#[test]
fn daemon_request_schema_constants_match_v1() -> TestResult {
    ensure(
        DAEMON_REQUEST_SCHEMA_V1 == "ee.daemon.request.v1",
        format!("request schema constant drifted: {DAEMON_REQUEST_SCHEMA_V1}"),
    )?;
    ensure(
        DAEMON_RESPONSE_SCHEMA_V1 == "ee.daemon.response.v1",
        format!("response schema constant drifted: {DAEMON_RESPONSE_SCHEMA_V1}"),
    )?;
    Ok(())
}
