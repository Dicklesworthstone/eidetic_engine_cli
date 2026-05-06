//! MCP adapter parity tests (eidetic_engine_cli-phje).
//!
//! Verifies that MCP tools produce identical JSON output to their CLI counterparts,
//! modulo timestamp fields that vary between runs.
//!
//! The core assertion: for every CLI command with an MCP tool counterpart,
//! `ee <command> --json` and `tools/call { name: "ee_<command>", arguments: {...} }`
//! must produce byte-identical JSON responses after normalizing timestamps.
//!
//! This test is gated behind the `mcp` feature since the MCP module is optional.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value as JsonValue, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn scenario_dir(name: &str) -> Result<PathBuf, String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join("mcp_parity")
        .join(name)
        .join(format!("{}-{ts}", std::process::id())))
}

fn init_workspace(dir: &Path) -> TestResult {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(
        vec![
            OsString::from("ee"),
            OsString::from("init"),
            OsString::from("--workspace"),
            OsString::from(dir),
            OsString::from("--json"),
        ],
        &mut stdout,
        &mut stderr,
    );
    ensure(
        exit == ee::models::ProcessExitCode::Success,
        format!("init failed: {}", String::from_utf8_lossy(&stderr)),
    )
}

fn run_cli(args: Vec<OsString>) -> (ee::models::ProcessExitCode, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(args, &mut stdout, &mut stderr);
    (
        exit,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn run_mcp_tool_call(name: &str, arguments: JsonValue) -> JsonValue {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    });
    ee::mcp::handle_json_rpc_message(&request).expect("MCP handler returned None for tools/call")
}

fn extract_mcp_tool_text(response: &JsonValue) -> Result<String, String> {
    response
        .get("result")
        .ok_or("MCP response missing result")?
        .get("content")
        .ok_or("MCP result missing content")?
        .as_array()
        .ok_or("MCP content is not an array")?
        .first()
        .ok_or("MCP content array is empty")?
        .get("text")
        .ok_or("MCP content item missing text")?
        .as_str()
        .ok_or("MCP text is not a string".to_string())
        .map(String::from)
}

fn normalize_runtime_varying_fields(value: &mut JsonValue) {
    match value {
        JsonValue::Object(map) => {
            for (key, val) in map.iter_mut() {
                let is_timestamp = key.ends_with("_at")
                    || key.ends_with("At")
                    || key == "timestamp"
                    || key == "created"
                    || key == "updated"
                    || key == "generated_at"
                    || key == "generatedAt"
                    || key == "last_access"
                    || key == "lastAccess"
                    || key == "checked_at"
                    || key == "checkedAt"
                    || key == "last_check"
                    || key == "lastCheck";

                let is_timing = key == "elapsedMs"
                    || key == "elapsed_ms"
                    || key.ends_with("_ms")
                    || key.ends_with("Ms");

                let is_generated_id = key == "memory_id"
                    || key == "memoryId"
                    || key == "audit_id"
                    || key == "auditId"
                    || key == "workflow_id"
                    || key == "workflowId"
                    || key == "index_job_id"
                    || key == "indexJobId"
                    || key == "revision_group_id"
                    || key == "revisionGroupId";

                if is_timestamp {
                    if val.is_string() {
                        *val = JsonValue::String("__TIMESTAMP__".to_string());
                    }
                } else if is_timing {
                    if val.is_number() {
                        *val = JsonValue::Number(serde_json::Number::from(0));
                    }
                } else if is_generated_id {
                    if val.is_string() {
                        *val = JsonValue::String("__GENERATED_ID__".to_string());
                    }
                } else {
                    normalize_runtime_varying_fields(val);
                }
            }
        }
        JsonValue::Array(arr) => {
            for item in arr.iter_mut() {
                normalize_runtime_varying_fields(item);
            }
        }
        _ => {}
    }
}

fn assert_json_equal_modulo_timestamps(
    cli_json: &str,
    mcp_json: &str,
    context: &str,
) -> TestResult {
    let mut cli_value: JsonValue = serde_json::from_str(cli_json)
        .map_err(|e| format!("{context}: CLI JSON parse error: {e}"))?;
    let mut mcp_value: JsonValue = serde_json::from_str(mcp_json)
        .map_err(|e| format!("{context}: MCP JSON parse error: {e}"))?;

    normalize_runtime_varying_fields(&mut cli_value);
    normalize_runtime_varying_fields(&mut mcp_value);

    let cli_normalized = serde_json::to_string_pretty(&cli_value)
        .map_err(|e| format!("{context}: CLI JSON serialize error: {e}"))?;
    let mcp_normalized = serde_json::to_string_pretty(&mcp_value)
        .map_err(|e| format!("{context}: MCP JSON serialize error: {e}"))?;

    if cli_normalized != mcp_normalized {
        return Err(format!(
            "{context}: JSON mismatch\n--- CLI ---\n{cli_normalized}\n--- MCP ---\n{mcp_normalized}"
        ));
    }
    Ok(())
}

/// Parity test: `ee status --json` vs `ee_status` MCP tool
#[test]
fn mcp_parity_status_command() -> TestResult {
    let dir = scenario_dir("status")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("status"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI status failed",
    )?;

    let mcp_response =
        run_mcp_tool_call("ee_status", json!({ "workspace": dir.to_string_lossy() }));
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "status")
}

/// Parity test: `ee health --json` vs `ee_health` MCP tool
#[test]
fn mcp_parity_health_command() -> TestResult {
    let dir = scenario_dir("health")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("health"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI health failed",
    )?;

    let mcp_response =
        run_mcp_tool_call("ee_health", json!({ "workspace": dir.to_string_lossy() }));
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "health")
}

/// Parity test: `ee capabilities --json` vs `ee_capabilities` MCP tool
#[test]
fn mcp_parity_capabilities_command() -> TestResult {
    let dir = scenario_dir("capabilities")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("capabilities"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI capabilities failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        "ee_capabilities",
        json!({ "workspace": dir.to_string_lossy() }),
    );
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "capabilities")
}

/// Parity test: `ee search --json` vs `ee_search` MCP tool
#[test]
fn mcp_parity_search_command() -> TestResult {
    let dir = scenario_dir("search")?;
    init_workspace(&dir)?;

    let (remember_exit, _stdout, _stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("remember"),
        OsString::from("Always run cargo test before release."),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--kind"),
        OsString::from("rule"),
        OsString::from("--level"),
        OsString::from("procedural"),
        OsString::from("--json"),
    ]);
    ensure(
        remember_exit == ee::models::ProcessExitCode::Success,
        "seed remember failed",
    )?;

    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("search"),
        OsString::from("cargo test release"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--limit"),
        OsString::from("5"),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI search failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        "ee_search",
        json!({
            "workspace": dir.to_string_lossy(),
            "query": "cargo test release",
            "limit": 5
        }),
    );
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "search")
}

/// Parity test: `ee context --json` vs `ee_context` MCP tool
#[test]
fn mcp_parity_context_command() -> TestResult {
    let dir = scenario_dir("context")?;
    init_workspace(&dir)?;

    let (remember_exit, _stdout, _stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("remember"),
        OsString::from("Run cargo fmt --check before committing."),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--kind"),
        OsString::from("rule"),
        OsString::from("--level"),
        OsString::from("procedural"),
        OsString::from("--json"),
    ]);
    ensure(
        remember_exit == ee::models::ProcessExitCode::Success,
        "seed remember failed",
    )?;

    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("context"),
        OsString::from("prepare a release"),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--max-tokens"),
        OsString::from("2000"),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI context failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        "ee_context",
        json!({
            "workspace": dir.to_string_lossy(),
            "query": "prepare a release",
            "maxTokens": 2000
        }),
    );
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "context")
}

/// Parity test: `ee remember --dry-run --json` vs `ee_remember` MCP tool (default dry-run)
#[test]
fn mcp_parity_remember_dry_run_command() -> TestResult {
    let dir = scenario_dir("remember")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("remember"),
        OsString::from("Test memory content for parity check."),
        OsString::from("--workspace"),
        OsString::from(&dir),
        OsString::from("--kind"),
        OsString::from("fact"),
        OsString::from("--level"),
        OsString::from("semantic"),
        OsString::from("--confidence"),
        OsString::from("0.85"),
        OsString::from("--dry-run"),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI remember --dry-run failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        "ee_remember",
        json!({
            "workspace": dir.to_string_lossy(),
            "content": "Test memory content for parity check.",
            "kind": "fact",
            "level": "semantic",
            "confidence": 0.85
        }),
    );
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "remember --dry-run")
}

/// Verify MCP manifest lists all walking-skeleton CLI commands
#[test]
fn mcp_manifest_covers_walking_skeleton_commands() -> TestResult {
    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("mcp"),
        OsString::from("manifest"),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI mcp manifest failed",
    )?;

    let manifest: JsonValue = serde_json::from_str(&cli_stdout)
        .map_err(|e| format!("mcp manifest JSON parse error: {e}"))?;

    let tools = manifest
        .get("data")
        .and_then(|d| d.get("tools"))
        .and_then(JsonValue::as_array)
        .ok_or("manifest missing tools array")?;

    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(JsonValue::as_str))
        .collect();

    let walking_skeleton_commands = [
        "ee_status",
        "ee_health",
        "ee_capabilities",
        "ee_search",
        "ee_remember",
    ];

    for cmd in walking_skeleton_commands {
        ensure(
            tool_names.contains(&cmd),
            format!("manifest missing walking-skeleton tool: {cmd}"),
        )?;
    }

    Ok(())
}

/// Verify MCP manifest has no divergence (empty divergence list)
#[test]
fn mcp_manifest_divergence_is_empty() -> TestResult {
    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("mcp"),
        OsString::from("manifest"),
        OsString::from("--json"),
    ]);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI mcp manifest failed",
    )?;

    let manifest: JsonValue = serde_json::from_str(&cli_stdout)
        .map_err(|e| format!("mcp manifest JSON parse error: {e}"))?;

    let divergence = manifest
        .get("data")
        .and_then(|d| d.get("divergence"))
        .and_then(JsonValue::as_array);

    if let Some(divergence_list) = divergence {
        ensure(
            divergence_list.is_empty(),
            format!(
                "MCP manifest has divergence (commands without MCP counterparts): {:?}",
                divergence_list
            ),
        )?;
    }

    Ok(())
}

/// Verify ee schema list covers all schemas used by MCP manifest
#[test]
fn schema_list_covers_mcp_schemas() -> TestResult {
    let (schema_exit, schema_stdout, _schema_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("schema"),
        OsString::from("list"),
        OsString::from("--json"),
    ]);
    ensure(
        schema_exit == ee::models::ProcessExitCode::Success,
        "CLI schema list failed",
    )?;

    let schema_list: JsonValue = serde_json::from_str(&schema_stdout)
        .map_err(|e| format!("schema list JSON parse error: {e}"))?;

    let schemas = schema_list
        .get("data")
        .and_then(|d| d.get("schemas"))
        .and_then(JsonValue::as_array)
        .ok_or("schema list missing schemas array")?;

    let schema_ids: Vec<&str> = schemas
        .iter()
        .filter_map(|s| s.get("id").and_then(JsonValue::as_str))
        .collect();

    let required_schemas = ["ee.response.v1", "ee.error.v1"];

    for schema in required_schemas {
        ensure(
            schema_ids.contains(&schema),
            format!("schema list missing required schema: {schema}"),
        )?;
    }

    Ok(())
}
