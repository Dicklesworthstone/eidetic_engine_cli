//! Real-binary E2E coverage for the MCP `resources/read` error
//! vocabulary (bd-3fdhs).
//!
//! Companion to bd-2j9z3 (tests/e2e_mcp_prompts_get_errors.rs;
//! prompts/get errors), bd-2fthw (tests/e2e_mcp_resources_list.rs;
//! resources/list per-resource vocabulary), bd-2wlym (resources/
//! templates/list), bd-4lz5u (prompts/list), and bd-3u7n5
//! (initialize + tools/list + shutdown). Library-level coverage in
//! `tests/fixtures/golden/mcp/json_rpc_cases.json` does not exercise
//! the real stdio loop, leaving the four distinct -32602 error
//! envelopes `handle_resources_read` emits unguarded against silent
//! drift in user-facing message text.
//!
//! `handle_resources_read` returns:
//!   1. `"Missing params"` when the request omits `params` entirely
//!   2. `"resources/read requires uri"` when `params` lacks `uri`
//!   3. `"Unsupported resource URI '<uri>'; expected ee://"` for any
//!      non-ee:// uri (build_cli_args_for_resource prefix guard)
//!   4. `"Unknown ee resource URI: <uri>"` for an `ee://` uri that
//!      does not match a known resource pattern
//!
//! All four are -32602 (invalid params) and downstream agent
//! harnesses bind to the exact message text for repair routing.

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
fn ee_mcp_serve_stdio_pins_resources_read_error_envelope_vocabulary() -> TestResult {
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
            "clientInfo": { "name": "ee-mcp-resources-read-errors-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;
    let _initialize_response = read_one_response_line(&mut stdout_reader)?;

    // Case 1: resources/read with no params at all
    let missing_params_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/read"
    });
    writeln!(stdin, "{missing_params_request}")
        .map_err(|error| format!("write resources/read(missing params): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush resources/read(missing params): {error}"))?;
    let missing_params_response = read_one_response_line(&mut stdout_reader)?;
    assert_error_envelope(&missing_params_response, 2, -32602, "Missing params")?;

    // Case 2: resources/read with empty params object (missing uri)
    let missing_uri_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/read",
        "params": {}
    });
    writeln!(stdin, "{missing_uri_request}")
        .map_err(|error| format!("write resources/read(missing uri): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush resources/read(missing uri): {error}"))?;
    let missing_uri_response = read_one_response_line(&mut stdout_reader)?;
    assert_error_envelope(
        &missing_uri_response,
        3,
        -32602,
        "resources/read requires uri",
    )?;

    // Case 3: resources/read with a non-ee:// uri (bogus prefix)
    let bogus_uri = "http://example.com/oops";
    let bogus_uri_request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/read",
        "params": { "uri": bogus_uri }
    });
    writeln!(stdin, "{bogus_uri_request}")
        .map_err(|error| format!("write resources/read(bogus uri): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush resources/read(bogus uri): {error}"))?;
    let bogus_uri_response = read_one_response_line(&mut stdout_reader)?;
    let expected_bogus_message = format!("Unsupported resource URI '{bogus_uri}'; expected ee://");
    assert_error_envelope(&bogus_uri_response, 4, -32602, &expected_bogus_message)?;

    // Case 4: resources/read with an ee:// uri that does not match any pattern
    let unknown_uri = "ee://definitely-not-a-real-resource";
    let unknown_uri_request = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "resources/read",
        "params": { "uri": unknown_uri }
    });
    writeln!(stdin, "{unknown_uri_request}")
        .map_err(|error| format!("write resources/read(unknown ee uri): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush resources/read(unknown ee uri): {error}"))?;
    let unknown_uri_response = read_one_response_line(&mut stdout_reader)?;
    let expected_unknown_message = format!("Unknown ee resource URI: {unknown_uri}");
    assert_error_envelope(&unknown_uri_response, 5, -32602, &expected_unknown_message)?;

    let shutdown_request = json!({
        "jsonrpc": "2.0",
        "id": 6,
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
