//! Real-binary E2E coverage for the MCP top-level error envelopes
//! that are not tied to a specific method handler (bd-2bnfv).
//!
//! Companion to bd-3fdhs (tests/e2e_mcp_resources_read_errors.rs;
//! resources/read errors), bd-2j9z3 (tests/e2e_mcp_prompts_get_errors.rs;
//! prompts/get errors), bd-2fthw (resources/list), bd-2wlym
//! (resources/templates/list), bd-4lz5u (prompts/list), and bd-3u7n5
//! (initialize + tools/list + shutdown). Library-level coverage in
//! `tests/fixtures/golden/mcp/json_rpc_cases.json` drives
//! `handle_json_rpc_message` but does not spawn the real stdio loop,
//! leaving three orthogonal error paths exposed by `run_stdio_server`
//! unguarded against silent drift:
//!
//!   1. `-32700 "Parse error: <details>"` (id=null) when the line
//!      sent on stdin is not valid JSON. Emitted directly by the
//!      parse-fallback branch in `run_stdio_server` before any
//!      request dispatch.
//!   2. `-32601 "Unknown method: <method>"` (id echoed) for any
//!      method that `McpMethod::parse` rejects.
//!   3. `-32600 "notifications/cancelled must be sent as a JSON-RPC
//!      notification without id"` (id echoed) when
//!      `notifications/cancelled` arrives with an id.
//!
//! These envelopes are bound by downstream agent harnesses for
//! repair routing, so an accidental text change should fail loudly.
//!
//! NOTE: The `-32600 "<method> requires id"` branches inside
//! `handle_request` (initialize/prompts-list/tools-list/shutdown
//! without id) are intentionally not exercised here because the
//! stdio loop treats id-less requests with non-empty method as
//! JSON-RPC notifications and drops them silently before dispatch
//! (see `handle_json_rpc_message` and `is_json_rpc_notification`).
//! Those branches are only reachable via direct unit tests of
//! `handle_request`, not the real stdio surface.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn read_one_response_line(
    reader: &mut BufReader<std::process::ChildStdout>,
) -> Result<Value, String> {
    let mut line = String::new();
    let bytes = reader
        .read_line(&mut line)
        .map_err(|error| format!("read mcp response line: {error}"))?;
    if bytes == 0 {
        return Err("mcp stdio closed before delivering a response".to_owned());
    }
    serde_json::from_str(line.trim())
        .map_err(|error| format!("mcp response not valid JSON: {error}; line={line:?}"))
}

fn assert_error_envelope(
    response: &Value,
    expected_id: Value,
    expected_code: i64,
    expected_message: &str,
) -> TestResult {
    if response["jsonrpc"].as_str() != Some("2.0") {
        return Err(format!("response missing jsonrpc=2.0: {response}"));
    }
    if response["id"] != expected_id {
        return Err(format!(
            "response id drifted: expected {expected_id}, got {} (full: {response})",
            response["id"]
        ));
    }
    let error = &response["error"];
    if !error.is_object() {
        return Err(format!("response must carry an error envelope: {response}"));
    }
    if error["code"].as_i64() != Some(expected_code) {
        return Err(format!(
            "error.code drifted: expected {expected_code}, got {} (full: {response})",
            error["code"]
        ));
    }
    let message = error["message"]
        .as_str()
        .ok_or_else(|| format!("error.message missing: {response}"))?;
    if message != expected_message {
        return Err(format!(
            "error.message drifted: expected {expected_message:?}, got {message:?}"
        ));
    }
    if !response["result"].is_null() {
        return Err(format!(
            "error envelope must not include a result field: {response}"
        ));
    }
    Ok(())
}

#[test]
fn ee_mcp_serve_stdio_pins_top_level_error_envelopes_parse_unknown_method_and_cancelled()
-> TestResult {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["mcp", "serve-stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn ee mcp serve-stdio: {error}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ee mcp serve-stdio stdin was not piped".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ee mcp serve-stdio stdout was not piped".to_owned())?;
    let mut stdout_reader = BufReader::new(stdout);

    let initialize_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": ee::mcp::MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "ee-mcp-request-error-envelopes-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;
    let _initialize_response = read_one_response_line(&mut stdout_reader)?;

    // Case 1: malformed JSON line yields the -32700 Parse error envelope
    // emitted by run_stdio_server's parse-fallback branch (id=null because
    // the request body could not be parsed at all).
    writeln!(stdin, "not-valid-json").map_err(|error| format!("write malformed json: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush malformed json: {error}"))?;
    let parse_error_response = read_one_response_line(&mut stdout_reader)?;
    if parse_error_response["jsonrpc"].as_str() != Some("2.0") {
        return Err(format!(
            "parse-error response missing jsonrpc=2.0: {parse_error_response}"
        ));
    }
    if parse_error_response["id"] != Value::Null {
        return Err(format!(
            "parse-error id must be null per JSON-RPC 2.0 (no id available): {parse_error_response}"
        ));
    }
    if parse_error_response["error"]["code"].as_i64() != Some(-32700) {
        return Err(format!(
            "parse-error code drifted: expected -32700, got {} (full: {parse_error_response})",
            parse_error_response["error"]["code"]
        ));
    }
    let parse_message = parse_error_response["error"]["message"]
        .as_str()
        .ok_or_else(|| format!("parse-error message missing: {parse_error_response}"))?;
    if !parse_message.starts_with("Parse error: ") {
        return Err(format!(
            "parse-error message must begin with \"Parse error: \", got {parse_message:?}"
        ));
    }
    if !parse_error_response["result"].is_null() {
        return Err(format!(
            "parse-error envelope must not include a result field: {parse_error_response}"
        ));
    }

    // Case 2: unknown method with id (id echoed) - reaches handle_request's
    // McpMethod::Unknown branch.
    let unknown_method = "definitely/not/a/real/method";
    let unknown_method_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": unknown_method
    });
    writeln!(stdin, "{unknown_method_request}")
        .map_err(|error| format!("write unknown method: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush unknown method: {error}"))?;
    let unknown_method_response = read_one_response_line(&mut stdout_reader)?;
    let expected_unknown_message = format!("Unknown method: {unknown_method}");
    assert_error_envelope(
        &unknown_method_response,
        json!(2),
        -32601,
        &expected_unknown_message,
    )?;

    // Case 3: notifications/cancelled with an id (forbidden — the method is
    // a notification per the MCP spec, so id MUST be absent). The server
    // emits the -32600 envelope with the offending id echoed.
    let cancelled_with_id_request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "notifications/cancelled",
        "params": { "requestId": 1 }
    });
    writeln!(stdin, "{cancelled_with_id_request}")
        .map_err(|error| format!("write notifications/cancelled(with id): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush notifications/cancelled(with id): {error}"))?;
    let cancelled_response = read_one_response_line(&mut stdout_reader)?;
    assert_error_envelope(
        &cancelled_response,
        json!(4),
        -32600,
        "notifications/cancelled must be sent as a JSON-RPC notification without id",
    )?;

    let shutdown_request = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "shutdown"
    });
    writeln!(stdin, "{shutdown_request}").map_err(|error| format!("write shutdown: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush shutdown: {error}"))?;
    let _shutdown_response = read_one_response_line(&mut stdout_reader)?;
    drop(stdin);

    let mut trailing_stdout = String::new();
    stdout_reader
        .read_to_string(&mut trailing_stdout)
        .map_err(|error| format!("read trailing stdout: {error}"))?;
    if !trailing_stdout.trim().is_empty() {
        return Err(format!(
            "ee mcp serve-stdio emitted trailing stdout after shutdown: {trailing_stdout:?}"
        ));
    }

    let exit_status = child
        .wait()
        .map_err(|error| format!("wait for ee mcp serve-stdio child: {error}"))?;
    if !exit_status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        return Err(format!(
            "ee mcp serve-stdio must exit cleanly after shutdown: status={exit_status}, \
             stderr={stderr}"
        ));
    }

    Ok(())
}
