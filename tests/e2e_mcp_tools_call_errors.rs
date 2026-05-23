//! Real-binary E2E coverage for the MCP `tools/call` error
//! vocabulary (bd-2el5l).
//!
//! Companion to bd-2bnfv (tests/e2e_mcp_request_error_envelopes.rs;
//! top-level error envelopes), bd-3fdhs (tests/e2e_mcp_resources_read_errors.rs;
//! resources/read errors), bd-2j9z3 (prompts/get errors), bd-2fthw
//! (resources/list), bd-2wlym (resources/templates/list), bd-4lz5u
//! (prompts/list), and bd-3u7n5 (initialize + tools/list + shutdown).
//! Library-level coverage in `tests/fixtures/golden/mcp/json_rpc_cases.json`
//! drives `handle_json_rpc_message` but does not spawn the real stdio
//! loop, leaving the three method-level error envelopes
//! `handle_tools_call` emits unguarded against silent drift in
//! user-facing message text:
//!
//!   1. `-32602 "Missing params"` when the request omits `params`.
//!   2. `-32601 "Unknown tool: <name>"` when `params.name` does not
//!      resolve via `mcp_tool_entry`.
//!   3. `-32602 "Tool arguments must be an object"` when
//!      `params.arguments` is present but not a JSON object.
//!
//! The bad-arguments-type case uses `ee_status` as the known tool
//! because it is a stable read-only entry registered at the top of
//! the MCP tool table.

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
    expected_id: u64,
    expected_code: i64,
    expected_message: &str,
) -> TestResult {
    if response["jsonrpc"].as_str() != Some("2.0") {
        return Err(format!("response missing jsonrpc=2.0: {response}"));
    }
    if response["id"].as_u64() != Some(expected_id) {
        return Err(format!("response id must echo {expected_id}: {response}"));
    }
    let error = &response["error"];
    if !error.is_object() {
        return Err(format!(
            "response must carry an error envelope (id={expected_id}): {response}"
        ));
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
fn ee_mcp_serve_stdio_pins_tools_call_error_envelope_vocabulary() -> TestResult {
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
            "clientInfo": { "name": "ee-mcp-tools-call-errors-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;
    let _initialize_response = read_one_response_line(&mut stdout_reader)?;

    // Case 1: tools/call with no params at all
    let missing_params_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call"
    });
    writeln!(stdin, "{missing_params_request}")
        .map_err(|error| format!("write tools/call(missing params): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush tools/call(missing params): {error}"))?;
    let missing_params_response = read_one_response_line(&mut stdout_reader)?;
    assert_error_envelope(&missing_params_response, 2, -32602, "Missing params")?;

    // Case 2: tools/call with an unknown tool name
    let unknown_tool = "definitely_not_a_real_tool";
    let unknown_tool_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": unknown_tool }
    });
    writeln!(stdin, "{unknown_tool_request}")
        .map_err(|error| format!("write tools/call(unknown): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush tools/call(unknown): {error}"))?;
    let unknown_tool_response = read_one_response_line(&mut stdout_reader)?;
    let expected_unknown_message = format!("Unknown tool: {unknown_tool}");
    assert_error_envelope(&unknown_tool_response, 3, -32601, &expected_unknown_message)?;

    // Case 3: tools/call against a known tool but arguments is not an object
    // (string instead of object). Use ee_status because it is a stable
    // read-only entry at the top of the MCP tool registry.
    let bad_args_request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "ee_status",
            "arguments": "not-an-object"
        }
    });
    writeln!(stdin, "{bad_args_request}")
        .map_err(|error| format!("write tools/call(bad args): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush tools/call(bad args): {error}"))?;
    let bad_args_response = read_one_response_line(&mut stdout_reader)?;
    assert_error_envelope(
        &bad_args_response,
        4,
        -32602,
        "Tool arguments must be an object",
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
