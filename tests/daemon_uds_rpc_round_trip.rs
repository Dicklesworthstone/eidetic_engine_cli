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
use ee::core::outcome::cancel_message;
use ee::core::search::SearchSourceMode;
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
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::{MemoryScope, QueryFilters, RedactionLevel};
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
const SEEDED_DB_WORKSPACE_ID: &str = "wsp_01234567890123456789012345";

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
    request_id: &'static str,
    workspace: &Path,
    database: &Path,
    index_dir: &Path,
) -> DaemonRequest {
    let workspace_id = workspace.display().to_string();
    let mut request = DaemonRequest::new(
        request_id,
        TEST_AGENT_ID,
        METHOD_SEARCH,
        serde_json::json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            "query": "release provenance",
            "workspacePath": workspace_id,
            "databasePath": database.display().to_string(),
            "indexDir": index_dir.display().to_string(),
            "limit": 5,
            "speed": "instant",
            "sourceMode": "hybrid",
            "memoryScope": "swarm"
        }),
    );
    request.workspace_id = Some(workspace.display().to_string());
    request
}

fn seed_context_workspace(root: &Path) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let workspace = root.join("workspace");
    let ee_dir = workspace.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee dir: {error}"))?;
    let database = ee_dir.join("ee.db");
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            SEEDED_DB_WORKSPACE_ID,
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
                workspace_id: SEEDED_DB_WORKSPACE_ID.to_string(),
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
