//! Real-binary E2E coverage for the MCP `prompts/get` error vocabulary
//! (bd-2j9z3).
//!
//! `src/mcp.rs::handle_prompts_get_pre_task_context_renders_arguments`
//! (the unit test at the bottom of `src/mcp.rs`) covers the happy path
//! for one prompt, and `tests/fixtures/golden/mcp/json_rpc_cases.json`
//! captures it via the library-level `handle_json_rpc_message` harness.
//! Neither path exercises `prompts/get` *failure modes* through the real
//! stdio loop, so the user-facing error envelope on
//! missing-params / unknown-prompt is unguarded against silent drift.
//!
//! `handle_prompts_get` returns two distinct -32602 envelopes:
//!   1. `"Missing params"` when the request omits `params` entirely
//!   2. `"Unknown prompt: <name>"` when `name` does not match
//!      `McpPrompt::parse`
//!
//! Companion to bd-4lz5u (prompts/list) and bd-2wlym
//! (resources/templates/list); same real-binary spawn pattern as
//! bd-3u7n5.

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
fn ee_mcp_serve_stdio_pins_prompts_get_missing_params_and_unknown_prompt_errors() -> TestResult {
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
            "clientInfo": { "name": "ee-mcp-prompts-get-errors-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;
    let _initialize_response = read_one_response_line(&mut stdout_reader)?;

    // Case 1: prompts/get with no params at all
    let missing_params_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "prompts/get"
    });
    writeln!(stdin, "{missing_params_request}")
        .map_err(|error| format!("write prompts/get(missing params): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush prompts/get(missing params): {error}"))?;
    let missing_params_response = read_one_response_line(&mut stdout_reader)?;
    assert_error_envelope(&missing_params_response, 2, -32602, "Missing params")?;

    // Case 2: prompts/get with an unknown prompt name
    let unknown_name = "definitely-not-a-real-prompt";
    let unknown_prompt_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "prompts/get",
        "params": {
            "name": unknown_name
        }
    });
    writeln!(stdin, "{unknown_prompt_request}")
        .map_err(|error| format!("write prompts/get(unknown): {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush prompts/get(unknown): {error}"))?;
    let unknown_prompt_response = read_one_response_line(&mut stdout_reader)?;
    let expected_unknown_message = format!("Unknown prompt: {unknown_name}");
    assert_error_envelope(
        &unknown_prompt_response,
        3,
        -32602,
        &expected_unknown_message,
    )?;

    let shutdown_request = json!({
        "jsonrpc": "2.0",
        "id": 4,
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
