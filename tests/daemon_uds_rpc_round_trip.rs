//! Integration test for the bd-oja31 daemon UDS RPC skeleton.
//!
//! Pins the wire-framing contract end-to-end: spin up the daemon
//! server on a tempdir UDS, send an `ee.daemon.echo` request, assert
//! the response echoes the params unchanged, send an
//! `ee.daemon.context` request, assert the `daemon_ann_warmload_not_yet_implemented`
//! stub error fires with the degraded code attached, and shut the
//! server down cleanly so the socket file is unlinked.
//!
//! Cfg-gated to Unix because the UDS server is Unix-only; non-Unix
//! builds get a no-op stub so the test binary compiles cleanly under
//! the Windows `cargo test --workspace` smoke job.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::Duration;

use ee::daemon::{
    DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE, DAEMON_REQUEST_MAX_BYTES,
    DAEMON_REQUEST_SCHEMA_V1, DAEMON_RESPONSE_MAX_BYTES, DAEMON_RESPONSE_SCHEMA_V1,
    protocol::{DaemonRequest, DaemonResponse},
    server::{
        DAEMON_REQUEST_DECODE_FAILED_CODE, DAEMON_REQUEST_SCHEMA_MISMATCH_CODE,
        DAEMON_UNKNOWN_METHOD_CODE, METHOD_CONTEXT, METHOD_ECHO, client_round_trip, start_server,
    },
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn wait_for_accept_loop() {
    // Tiny pause so the accept-loop thread enters `listener.accept()`
    // before the client opens its connection. The skeleton's accept
    // loop blocks inside the syscall, so this is just for test
    // robustness on slow CI hosts.
    thread::sleep(Duration::from_millis(75));
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
fn daemon_echo_round_trip_preserves_request_id_and_params() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-rt.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;
    wait_for_accept_loop();

    let request = DaemonRequest::new(
        "req-echo-roundtrip-001",
        METHOD_ECHO,
        serde_json::json!({
            "hello": "world",
            "n": 42,
            "nested": {"k": "v"}
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
        response.error.is_none(),
        format!("echo must not return error; got {:?}", response.error),
    )?;
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| format!("echo must return result; got response {response:?}"))?;
    ensure(
        result
            == &serde_json::json!({
                "hello": "world",
                "n": 42,
                "nested": {"k": "v"}
            }),
        format!("echo result must equal request params; got {result}"),
    )?;
    ensure(
        response.degraded_codes.is_empty(),
        format!(
            "echo must not attach degraded codes; got {:?}",
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
    wait_for_accept_loop();

    let request = DaemonRequest::new(
        "req-ctx-stub-001",
        METHOD_CONTEXT,
        serde_json::json!({"task": "ship daemon skeleton"}),
    );
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;

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
    wait_for_accept_loop();

    let mut request = DaemonRequest::new(
        "req-schema-mismatch-001",
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
    wait_for_accept_loop();

    let request = DaemonRequest::new(
        "req-unknown-method-001",
        "ee.daemon.nope",
        serde_json::json!({"hello": "world"}),
    );
    let response = client_round_trip(handle.socket_path(), &request)
        .map_err(|error| format!("client_round_trip: {error}"))?;

    ensure(
        response.request_id == "req-unknown-method-001",
        format!("request_id must round-trip; got {}", response.request_id),
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
    wait_for_accept_loop();

    let mut stream = connect_client(handle.socket_path())?;
    write_raw_frame(&mut stream, b"{not valid json")?;
    let response = read_response_frame(&mut stream)?;

    ensure(
        response.request_id == "<unknown>",
        format!(
            "malformed request must use <unknown> request_id; got {}",
            response.request_id
        ),
    )?;
    ensure_error_code(&response, DAEMON_REQUEST_DECODE_FAILED_CODE)?;

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
    wait_for_accept_loop();

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
    wait_for_accept_loop();

    let socket_a = handle.socket_path().to_path_buf();
    let socket_b = handle.socket_path().to_path_buf();
    let client_a = thread::spawn(move || {
        let request = DaemonRequest::new(
            "req-concurrent-a",
            METHOD_ECHO,
            serde_json::json!({"client": "a"}),
        );
        client_round_trip(&socket_a, &request).map_err(|error| format!("client a: {error}"))
    });
    let client_b = thread::spawn(move || {
        let request = DaemonRequest::new(
            "req-concurrent-b",
            METHOD_ECHO,
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
        response_b.request_id == "req-concurrent-b",
        format!("client b request_id drifted: {}", response_b.request_id),
    )?;
    ensure(
        response_a.error.is_none() && response_b.error.is_none(),
        format!("concurrent echo clients must both succeed; got {response_a:?} and {response_b:?}"),
    )?;

    handle
        .shutdown()
        .map_err(|error| format!("shutdown: {error}"))?;
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
