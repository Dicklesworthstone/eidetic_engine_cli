//! Integration test for the bd-oja31 daemon UDS RPC skeleton.
//!
//! Pins the wire-framing contract end-to-end: spin up the daemon
//! server on a tempdir UDS, send an `ee.daemon.echo` request, assert
//! default production servers refuse the diagnostic reflector, send an
//! `ee.daemon.context` request, assert the `daemon_ann_warmload_not_yet_implemented`
//! stub error fires with the degraded code attached, and shut the
//! server down cleanly so the socket file is unlinked.
//!
//! Cfg-gated to Unix because the UDS server is Unix-only; non-Unix
//! builds get a no-op stub so the test binary compiles cleanly under
//! the Windows `cargo test --workspace` smoke job.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use ee::daemon::{
    DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE, DAEMON_REQUEST_MAX_BYTES,
    DAEMON_REQUEST_SCHEMA_V1, DAEMON_RESPONSE_MAX_BYTES, DAEMON_RESPONSE_SCHEMA_V1,
    DAEMON_SHUTTING_DOWN_CODE,
    protocol::{DaemonRequest, DaemonResponse},
    server::{
        DAEMON_ECHO_DISABLED_CODE, DAEMON_REQUEST_DECODE_FAILED_CODE,
        DAEMON_REQUEST_SCHEMA_MISMATCH_CODE, DAEMON_UNKNOWN_METHOD_CODE, METHOD_CONTEXT,
        METHOD_ECHO, client_round_trip, start_server,
    },
};

type TestResult = Result<(), String>;
const TEST_AGENT_ID: &str = "agent-daemon-uds-test";

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
fn daemon_context_returns_warmload_not_yet_implemented_with_degraded_code() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-ctx.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;

    let mut request = DaemonRequest::new(
        "req-ctx-stub-001",
        TEST_AGENT_ID,
        METHOD_CONTEXT,
        serde_json::json!({"task": "ship daemon skeleton"}),
    );
    request.workspace_id = Some("workspace-daemon-uds-test".to_owned());
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
        response.result.is_none(),
        format!(
            "context stub must NOT return result; got {:?}",
            response.result
        ),
    )?;
    let error = response
        .error
        .as_ref()
        .ok_or_else(|| format!("context stub must return error; got {response:?}"))?;
    ensure(
        error.code == DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE,
        format!(
            "context stub code must be {DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE}; got {}",
            error.code
        ),
    )?;
    ensure(
        response
            .degraded_codes
            .contains(&DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE.to_owned()),
        format!(
            "context stub must attach the warmload degraded code; got {:?}",
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
        let request = DaemonRequest::new(
            "req-concurrent-a",
            "agent-daemon-uds-a",
            METHOD_CONTEXT,
            serde_json::json!({"client": "a"}),
        );
        client_round_trip(&socket_a, &request).map_err(|error| format!("client a: {error}"))
    });
    let client_b = thread::spawn(move || {
        let request = DaemonRequest::new(
            "req-concurrent-b",
            "agent-daemon-uds-b",
            METHOD_CONTEXT,
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
    ensure_error_code(&response_a, DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE)?;
    ensure_error_code(&response_b, DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE)?;

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
    let request = DaemonRequest::new(
        "req-restart-same-path",
        TEST_AGENT_ID,
        METHOD_CONTEXT,
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
    ensure_error_code(&response, DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE)?;

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

    let request = DaemonRequest::new(
        "req-shutdown-race",
        TEST_AGENT_ID,
        METHOD_CONTEXT,
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
        error.code == DAEMON_SHUTTING_DOWN_CODE
            || error.code == DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE,
        format!(
            "shutdown race must return structured shutdown or normal stub error; got {}",
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
