//! Real-binary E2E coverage for MCP wave-surface tools (bd-2o0w0).
//!
//! The feature-gated parity suite exercises `ee::mcp::handle_json_rpc_message`
//! in-process. This test uses the actual `ee mcp serve-stdio` binary so the
//! published tools/list vocabulary and a representative tools/call round-trip
//! are pinned at the transport boundary downstream harnesses use.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn scenario_dir(name: &str) -> Result<PathBuf, String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join("mcp_wave_tools")
        .join(name)
        .join(format!("{}-{ts}", std::process::id())))
}

fn run_ee(args: &[&str]) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("run ee {args:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ee {args:?} failed: status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("ee stdout was not UTF-8: {error}"))
}

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

fn write_request(stdin: &mut std::process::ChildStdin, request: &Value) -> Result<(), String> {
    writeln!(stdin, "{request}").map_err(|error| format!("write mcp request: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush mcp request: {error}"))
}

fn assert_tool_present(tools_list_response: &Value, name: &str) -> TestResult {
    let tools = tools_list_response["result"]["tools"]
        .as_array()
        .ok_or_else(|| {
            format!("tools/list result.tools must be an array: {tools_list_response}")
        })?;
    if tools.iter().any(|tool| tool["name"].as_str() == Some(name)) {
        Ok(())
    } else {
        Err(format!("tools/list missing wave tool {name}"))
    }
}

fn tool_text(response: &Value) -> Result<&str, String> {
    response["result"]["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|item| item["text"].as_str())
        .ok_or_else(|| format!("tools/call response missing text content: {response}"))
}

fn init_workspace(workspace: &Path) -> TestResult {
    let workspace_arg = workspace.to_string_lossy();
    let _ = run_ee(&["init", "--workspace", workspace_arg.as_ref(), "--json"])?;
    Ok(())
}

#[test]
fn ee_mcp_serve_stdio_lists_wave_tools_and_round_trips_recall() -> TestResult {
    let workspace = scenario_dir("recall")?;
    init_workspace(&workspace)?;
    let workspace_arg = workspace.to_string_lossy();

    let cli_recall = run_ee(&[
        "recall",
        "--path",
        "src/mcp.rs",
        "--workspace",
        workspace_arg.as_ref(),
        "--budget-tokens",
        "200",
        "--json",
    ])?;

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

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": ee::mcp::MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "ee-mcp-wave-tools-e2e", "version": "0.0.0" }
            }
        }),
    )?;
    let _initialize = read_one_response_line(&mut stdout_reader)?;

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    )?;
    let tools_list = read_one_response_line(&mut stdout_reader)?;
    for name in [
        "ee_recall",
        "ee_ask",
        "ee_primer",
        "ee_journal_append",
        "ee_decide_record",
        "ee_decide_list",
        "ee_decide_revisit",
    ] {
        assert_tool_present(&tools_list, name)?;
    }

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "ee_recall",
                "arguments": {
                    "workspace": workspace_arg.as_ref(),
                    "path": ["src/mcp.rs"],
                    "budgetTokens": 200
                }
            }
        }),
    )?;
    let recall_response = read_one_response_line(&mut stdout_reader)?;
    if recall_response["result"]["isError"].as_bool() != Some(false) {
        return Err(format!("ee_recall tools/call failed: {recall_response}"));
    }
    let mcp_recall = tool_text(&recall_response)?;
    let cli_json: Value =
        serde_json::from_str(&cli_recall).map_err(|error| format!("CLI JSON: {error}"))?;
    let mcp_json: Value =
        serde_json::from_str(mcp_recall).map_err(|error| format!("MCP JSON: {error}"))?;
    if cli_json != mcp_json {
        return Err(format!(
            "ee_recall MCP payload must match CLI JSON\nCLI: {cli_json}\nMCP: {mcp_json}"
        ));
    }

    write_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "shutdown"
        }),
    )?;
    let shutdown_response = read_one_response_line(&mut stdout_reader)?;
    if shutdown_response["id"].as_u64() != Some(4) {
        return Err(format!(
            "shutdown id must echo request: {shutdown_response}"
        ));
    }
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
            "ee mcp serve-stdio must exit cleanly after shutdown: status={exit_status}, stderr={stderr}"
        ));
    }

    Ok(())
}
