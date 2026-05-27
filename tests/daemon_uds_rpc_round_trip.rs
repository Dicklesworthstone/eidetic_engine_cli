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

use std::thread;
use std::time::Duration;

use ee::daemon::{
    DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE, DAEMON_REQUEST_SCHEMA_V1,
    DAEMON_RESPONSE_SCHEMA_V1,
    protocol::DaemonRequest,
    server::{METHOD_CONTEXT, METHOD_ECHO, client_round_trip, start_server},
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn daemon_echo_round_trip_preserves_request_id_and_params() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let socket_path = temp.path().join("ee-daemon-rt.sock");

    let mut handle =
        start_server(&socket_path).map_err(|error| format!("start_server: {error}"))?;
    // Tiny pause so the accept-loop thread enters `listener.accept()`
    // before the client opens its connection. The skeleton's accept
    // loop blocks inside the syscall, so this is just for test
    // robustness on slow CI hosts.
    thread::sleep(Duration::from_millis(75));

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
    thread::sleep(Duration::from_millis(75));

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
