//! Real-binary E2E coverage for `ee mcp serve-stdio` (bd-3u7n5).
//!
//! `tests/mcp_parity.rs` covers tool-call parity at the library level via
//! `ee::mcp::handle_json_rpc_message`, and `tests/smoke.rs` covers the
//! mcp-feature-disabled capability-gap path. Neither spawns the real binary
//! with the mcp feature enabled and exercises the actual stdio loop in
//! `src/mcp.rs::run_stdio_server` end-to-end.
//!
//! This file fills that gap: it boots the real `ee` binary, pipes a small
//! JSON-RPC dialog (`initialize`, `tools/list`, `shutdown`) through its
//! stdin, reads each response line off its stdout, and asserts the
//! protocol-level invariants downstream agents rely on.

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

#[test]
fn ee_mcp_serve_stdio_completes_initialize_tools_list_shutdown_handshake() -> TestResult {
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
            "clientInfo": { "name": "ee-mcp-e2e-test", "version": "0.0.0" }
        }
    });
    writeln!(stdin, "{initialize_request}")
        .map_err(|error| format!("write initialize: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush initialize: {error}"))?;

    let initialize_response = read_one_response_line(&mut stdout_reader)?;
    assert_eq!(
        initialize_response["jsonrpc"].as_str(),
        Some("2.0"),
        "initialize response missing jsonrpc=2.0: {initialize_response}"
    );
    assert_eq!(
        initialize_response["id"].as_u64(),
        Some(1),
        "initialize response id must echo the request id: {initialize_response}"
    );
    let result = &initialize_response["result"];
    assert_eq!(
        result["protocolVersion"].as_str(),
        Some(ee::mcp::MCP_PROTOCOL_VERSION),
        "initialize result.protocolVersion must match MCP_PROTOCOL_VERSION: {initialize_response}"
    );
    assert_eq!(
        result["serverInfo"]["name"].as_str(),
        Some("ee"),
        "initialize serverInfo.name must be 'ee': {initialize_response}"
    );
    if result["serverInfo"]["version"].as_str().is_none() {
        return Err(format!(
            "initialize serverInfo.version missing: {initialize_response}"
        ));
    }
    if !result["capabilities"].is_object() {
        return Err(format!(
            "initialize result.capabilities must be an object: {initialize_response}"
        ));
    }

    let tools_list_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    writeln!(stdin, "{tools_list_request}")
        .map_err(|error| format!("write tools/list: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush tools/list: {error}"))?;

    let tools_list_response = read_one_response_line(&mut stdout_reader)?;
    assert_eq!(
        tools_list_response["jsonrpc"].as_str(),
        Some("2.0"),
        "tools/list response missing jsonrpc=2.0: {tools_list_response}"
    );
    assert_eq!(
        tools_list_response["id"].as_u64(),
        Some(2),
        "tools/list id must echo: {tools_list_response}"
    );
    let tools = tools_list_response["result"]["tools"]
        .as_array()
        .ok_or_else(|| {
            format!("tools/list result.tools must be an array: {tools_list_response}")
        })?;
    if tools.is_empty() {
        return Err(format!(
            "tools/list result.tools must be non-empty (real registry has many tools): {tools_list_response}"
        ));
    }
    for tool in tools {
        let name = tool["name"]
            .as_str()
            .ok_or_else(|| format!("tool missing name field: {tool}"))?;
        if name.trim().is_empty() {
            return Err(format!("tool has empty name: {tool}"));
        }
        if tool["description"].as_str().is_none() {
            return Err(format!("tool {name} missing description: {tool}"));
        }
        if !tool["inputSchema"].is_object() {
            return Err(format!("tool {name} missing inputSchema object: {tool}"));
        }
    }

    let shutdown_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "shutdown"
    });
    writeln!(stdin, "{shutdown_request}").map_err(|error| format!("write shutdown: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush shutdown: {error}"))?;

    let shutdown_response = read_one_response_line(&mut stdout_reader)?;
    assert_eq!(
        shutdown_response["jsonrpc"].as_str(),
        Some("2.0"),
        "shutdown response missing jsonrpc=2.0: {shutdown_response}"
    );
    assert_eq!(
        shutdown_response["id"].as_u64(),
        Some(3),
        "shutdown id must echo: {shutdown_response}"
    );

    // Closing stdin after the loop has broken is a courtesy; the server
    // already exited the read loop on the shutdown method.
    drop(stdin);

    let mut trailing_stdout = String::new();
    stdout_reader
        .read_to_string(&mut trailing_stdout)
        .map_err(|error| format!("read trailing stdout: {error}"))?;
    if !trailing_stdout.trim().is_empty() {
        return Err(format!(
            "ee mcp serve-stdio emitted trailing stdout after shutdown response: {trailing_stdout:?}"
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
            "ee mcp serve-stdio must exit cleanly after shutdown: status={exit_status}, stderr={stderr}"
        ));
    }

    Ok(())
}
