//! Real-binary E2E coverage for the MCP `initialize` result
//! vocabulary (bd-1td34).
//!
//! The existing `tests/e2e_mcp_top_level.rs` (bd-3u7n5) round-trips
//! `initialize` + `tools/list` + `shutdown` but only asserts that
//! `result.capabilities` is an object and `result.serverInfo.name`
//! is `"ee"` with `serverInfo.version` present. It does NOT pin the
//! per-key vocabulary downstream agents bind to:
//!
//!   * `result.protocolVersion` must equal
//!     `ee::mcp::MCP_PROTOCOL_VERSION` exactly.
//!   * `result.serverInfo` must have exactly `{name, version}` with
//!     `name == "ee"` and `version == env!("CARGO_PKG_VERSION")`.
//!   * `result.capabilities` must have exactly the three keys
//!     `{prompts, resources, tools}` each mapped to an empty object
//!     `{}` (per `handle_initialize` in `src/mcp.rs`).
//!
//! Companion to bd-2bnfv, bd-2el5l, bd-3fdhs, bd-2j9z3, bd-2fthw,
//! bd-2wlym, bd-4lz5u, and bd-3u7n5.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
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

#[test]
fn ee_mcp_serve_stdio_pins_initialize_result_capabilities_and_server_info() -> TestResult {
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
            "clientInfo": { "name": "ee-mcp-initialize-capabilities-e2e", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;

    let initialize_response = read_one_response_line(&mut stdout_reader)?;
    if initialize_response["jsonrpc"].as_str() != Some("2.0") {
        return Err(format!(
            "initialize response missing jsonrpc=2.0: {initialize_response}"
        ));
    }
    if initialize_response["id"].as_u64() != Some(1) {
        return Err(format!(
            "initialize response id must echo request id (1): {initialize_response}"
        ));
    }
    if !initialize_response["error"].is_null() {
        return Err(format!(
            "initialize must succeed, got error envelope: {initialize_response}"
        ));
    }

    let result = initialize_response
        .get("result")
        .ok_or_else(|| format!("initialize response missing result: {initialize_response}"))?;

    // protocolVersion strictly equals MCP_PROTOCOL_VERSION
    let protocol_version = result["protocolVersion"].as_str().ok_or_else(|| {
        format!("initialize result.protocolVersion must be a string: {initialize_response}")
    })?;
    if protocol_version != ee::mcp::MCP_PROTOCOL_VERSION {
        return Err(format!(
            "initialize result.protocolVersion drifted: expected {:?}, got {:?}",
            ee::mcp::MCP_PROTOCOL_VERSION,
            protocol_version
        ));
    }

    // serverInfo per-key vocabulary
    let server_info = result["serverInfo"]
        .as_object()
        .ok_or_else(|| format!("result.serverInfo must be an object: {initialize_response}"))?;
    let server_info_keys: BTreeSet<&str> = server_info.keys().map(String::as_str).collect();
    let expected_server_info_keys: BTreeSet<&str> = ["name", "version"].into_iter().collect();
    if server_info_keys != expected_server_info_keys {
        return Err(format!(
            "result.serverInfo keys drifted: expected {expected_server_info_keys:?}, got {server_info_keys:?}"
        ));
    }
    let server_name = server_info["name"]
        .as_str()
        .ok_or_else(|| format!("serverInfo.name must be a string: {initialize_response}"))?;
    if server_name != "ee" {
        return Err(format!(
            "serverInfo.name drifted: expected \"ee\", got {server_name:?}"
        ));
    }
    let server_version = server_info["version"]
        .as_str()
        .ok_or_else(|| format!("serverInfo.version must be a string: {initialize_response}"))?;
    if server_version != env!("CARGO_PKG_VERSION") {
        return Err(format!(
            "serverInfo.version drifted from compile-time CARGO_PKG_VERSION: expected {:?}, got {:?}",
            env!("CARGO_PKG_VERSION"),
            server_version
        ));
    }

    // capabilities per-key vocabulary: exactly {prompts, resources, tools}
    // each mapped to an empty object per handle_initialize.
    let capabilities = result["capabilities"]
        .as_object()
        .ok_or_else(|| format!("result.capabilities must be an object: {initialize_response}"))?;
    let capabilities_keys: BTreeSet<&str> = capabilities.keys().map(String::as_str).collect();
    let expected_capabilities_keys: BTreeSet<&str> =
        ["prompts", "resources", "tools"].into_iter().collect();
    if capabilities_keys != expected_capabilities_keys {
        return Err(format!(
            "result.capabilities keys drifted: expected {expected_capabilities_keys:?}, got {capabilities_keys:?}"
        ));
    }
    for key in &expected_capabilities_keys {
        let entry = capabilities
            .get(*key)
            .ok_or_else(|| format!("capabilities.{key} missing: {initialize_response}"))?;
        let object = entry.as_object().ok_or_else(|| {
            format!("capabilities.{key} must be an object: {initialize_response}")
        })?;
        if !object.is_empty() {
            return Err(format!(
                "capabilities.{key} must be the empty object {{}}, got {entry}"
            ));
        }
    }

    let shutdown_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
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
