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

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value as JsonValue, json};

type TestResult = Result<(), String>;

const PARITY_TESTED_TOOLS: &[&str] = &[
    "ee_health",
    "ee_status",
    "ee_doctor",
    "ee_capabilities",
    "ee_search",
    "ee_context",
    "ee_insights",
    "ee_proximity",
    "ee_pack_dna_explain",
    "ee_revision_impact",
    "ee_memory_show",
    "ee_why",
    "ee_remember",
    "ee_outcome",
    "ee_mesh_discovery_policy",
];

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn trace_mcp_top_level(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "mcp_parity_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.3"),
        surface = "mcp_top_level",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "MCP top-level parity checkpoint"
    );
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

fn parity_fixture_path(surface: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("mcp_parity")
        .join(surface)
        .join("inputs")
        .join(format!("{name}.json"))
}

fn load_parity_fixture(surface: &str, name: &str) -> Result<JsonValue, String> {
    let path = parity_fixture_path(surface, name);
    let contents =
        fs::read_to_string(&path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&contents).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

fn fixture_str<'a>(fixture: &'a JsonValue, field: &str) -> Result<&'a str, String> {
    fixture
        .get(field)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("MCP parity fixture missing string field {field}"))
}

fn optional_fixture_str<'a>(
    fixture: &'a JsonValue,
    field: &str,
) -> Result<Option<&'a str>, String> {
    match fixture.get(field) {
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("MCP parity fixture field {field} must be a string")),
        None => Ok(None),
    }
}

fn replace_fixture_placeholders(
    text: &str,
    workspace: &Path,
    memory_id: Option<&str>,
    event_id: Option<&str>,
) -> Result<String, String> {
    let workspace_text = workspace.to_string_lossy();
    let mut replaced = text.replace("__WORKSPACE__", workspace_text.as_ref());
    if let Some(memory_id) = memory_id {
        replaced = replaced.replace("__MEMORY_ID__", memory_id);
    }
    if let Some(event_id) = event_id {
        replaced = replaced.replace("__EVENT_ID__", event_id);
    }
    if replaced.contains("__MEMORY_ID__") {
        return Err("MCP parity fixture needs a memory id but none was provided".to_string());
    }
    if replaced.contains("__EVENT_ID__") {
        return Err("MCP parity fixture needs an event id but none was provided".to_string());
    }
    Ok(replaced)
}

fn replace_fixture_value(
    value: &JsonValue,
    workspace: &Path,
    memory_id: Option<&str>,
    event_id: Option<&str>,
) -> Result<JsonValue, String> {
    match value {
        JsonValue::String(text) => Ok(JsonValue::String(replace_fixture_placeholders(
            text, workspace, memory_id, event_id,
        )?)),
        JsonValue::Array(items) => items
            .iter()
            .map(|item| replace_fixture_value(item, workspace, memory_id, event_id))
            .collect::<Result<Vec<_>, _>>()
            .map(JsonValue::Array),
        JsonValue::Object(fields) => {
            let mut replaced = serde_json::Map::new();
            for (key, value) in fields {
                replaced.insert(
                    key.clone(),
                    replace_fixture_value(value, workspace, memory_id, event_id)?,
                );
            }
            Ok(JsonValue::Object(replaced))
        }
        _ => Ok(value.clone()),
    }
}

fn fixture_cli_args(
    fixture: &JsonValue,
    workspace: &Path,
    memory_id: Option<&str>,
    event_id: Option<&str>,
) -> Result<Vec<OsString>, String> {
    let args = fixture
        .get("cliArgs")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "MCP parity fixture missing cliArgs array".to_string())?;
    args.iter()
        .map(|arg| {
            let arg = arg
                .as_str()
                .ok_or_else(|| "MCP parity fixture cliArgs entries must be strings".to_string())?;
            replace_fixture_placeholders(arg, workspace, memory_id, event_id).map(OsString::from)
        })
        .collect()
}

fn fixture_mcp_arguments(
    fixture: &JsonValue,
    workspace: &Path,
    memory_id: Option<&str>,
    event_id: Option<&str>,
) -> Result<JsonValue, String> {
    let arguments = fixture
        .get("mcpArguments")
        .ok_or_else(|| "MCP parity fixture missing mcpArguments".to_string())?;
    replace_fixture_value(arguments, workspace, memory_id, event_id)
}

fn fixture_mcp_tool(fixture: &JsonValue) -> Result<&str, String> {
    fixture_str(fixture, "mcpTool")
}

fn init_workspace(dir: &Path) -> TestResult {
    trace_mcp_top_level("input", 0, &[]);
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
    trace_mcp_top_level("dispatch", 0, &[]);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = ee::cli::run(args, &mut stdout, &mut stderr);
    trace_mcp_top_level("response", 0, &[]);
    (
        exit,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

fn run_mcp_tool_call(name: &str, arguments: JsonValue) -> Result<JsonValue, String> {
    trace_mcp_top_level("dependency_check", 0, &[]);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    });
    let response = ee::mcp::handle_json_rpc_message(&request)
        .ok_or_else(|| "MCP handler returned None for tools/call".to_owned())?;
    trace_mcp_top_level("response", 0, &[]);
    Ok(response)
}

fn run_mcp_tools_list() -> Result<JsonValue, String> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    });
    ee::mcp::handle_json_rpc_message(&request)
        .ok_or_else(|| "MCP handler returned None for tools/list".to_owned())
}

fn remember_test_memory(dir: &Path, content: &str) -> Result<String, String> {
    let (remember_exit, remember_stdout, remember_stderr) = run_cli(vec![
        OsString::from("ee"),
        OsString::from("remember"),
        OsString::from(content),
        OsString::from("--workspace"),
        OsString::from(dir),
        OsString::from("--kind"),
        OsString::from("rule"),
        OsString::from("--level"),
        OsString::from("procedural"),
        OsString::from("--json"),
    ]);
    ensure(
        remember_exit == ee::models::ProcessExitCode::Success,
        format!(
            "seed remember failed: {}",
            if remember_stderr.is_empty() {
                remember_stdout.as_str()
            } else {
                remember_stderr.as_str()
            }
        ),
    )?;

    let remember_json: JsonValue = serde_json::from_str(&remember_stdout)
        .map_err(|e| format!("seed remember JSON parse error: {e}"))?;
    remember_json
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("seed remember response missing data.memory_id: {remember_json}"))
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

fn extract_json_pointer_text(
    json_text: &str,
    pointer: &str,
    context: &str,
) -> Result<String, String> {
    let value: JsonValue =
        serde_json::from_str(json_text).map_err(|e| format!("{context}: JSON parse error: {e}"))?;
    let extracted = value
        .pointer(pointer)
        .ok_or_else(|| format!("{context}: missing JSON pointer {pointer}"))?;
    serde_json::to_string(extracted)
        .map_err(|e| format!("{context}: extracted JSON serialize error: {e}"))
}

/// Parity test: `ee status --json` vs `ee_status` MCP tool
#[test]
fn mcp_parity_status_command() -> TestResult {
    let fixture = load_parity_fixture("status", "basic")?;
    let dir = scenario_dir("status")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI status failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "status")
}

/// Parity test: `ee doctor --json` vs `ee_doctor` MCP tool
#[test]
fn mcp_parity_doctor_command() -> TestResult {
    let fixture = load_parity_fixture("doctor", "basic")?;
    let dir = scenario_dir("doctor")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI doctor failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "doctor")
}

/// Parity test: `ee health --json` vs `ee_health` MCP tool
#[test]
fn mcp_parity_health_command() -> TestResult {
    let fixture = load_parity_fixture("health", "basic")?;
    let dir = scenario_dir("health")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI health failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "health")
}

/// Parity test: `ee capabilities --json` vs `ee_capabilities` MCP tool
#[test]
fn mcp_parity_capabilities_command() -> TestResult {
    let fixture = load_parity_fixture("capabilities", "basic")?;
    let dir = scenario_dir("capabilities")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI capabilities failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "capabilities")
}

/// Parity test: `ee search --json` vs `ee_search` MCP tool
#[test]
fn mcp_parity_search_command() -> TestResult {
    let fixture = load_parity_fixture("search", "basic")?;
    let dir = scenario_dir("search")?;
    init_workspace(&dir)?;

    if let Some(seed_content) = optional_fixture_str(&fixture, "seedMemoryContent")? {
        remember_test_memory(&dir, seed_content)?;
    }

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI search failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "search")
}

/// Parity test: `ee context --json` vs `ee_context` MCP tool
#[test]
fn mcp_parity_context_command() -> TestResult {
    let fixture = load_parity_fixture("context", "basic")?;
    let dir = scenario_dir("context")?;
    init_workspace(&dir)?;

    if let Some(seed_content) = optional_fixture_str(&fixture, "seedMemoryContent")? {
        remember_test_memory(&dir, seed_content)?;
    }

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI context failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "context")
}

/// Parity test: `ee insights --json` vs `ee_insights` MCP tool
#[test]
fn mcp_parity_insights_command() -> TestResult {
    let fixture = load_parity_fixture("insights", "basic")?;
    let dir = scenario_dir("insights")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI insights failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "insights")
}

/// Parity test: `ee proximity --json` vs `ee_proximity` MCP tool
#[test]
fn mcp_parity_proximity_command() -> TestResult {
    let fixture = load_parity_fixture("proximity", "basic")?;
    let dir = scenario_dir("proximity")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI proximity failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "proximity")
}

/// Parity test: `ee context --explain --json` packDna vs `ee_pack_dna_explain` MCP tool
#[test]
fn mcp_parity_pack_dna_explain_command() -> TestResult {
    let fixture = load_parity_fixture("pack_dna_explain", "basic")?;
    let dir = scenario_dir("pack_dna_explain")?;
    init_workspace(&dir)?;

    if let Some(seed_content) = optional_fixture_str(&fixture, "seedMemoryContent")? {
        remember_test_memory(&dir, seed_content)?;
    }

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI context --explain failed",
    )?;
    let cli_pack_dna = extract_json_pointer_text(
        &cli_stdout,
        "/data/pack/packDna",
        "context --explain packDna",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_pack_dna, &mcp_text, "packDna")
}

/// Parity test: `ee memory revise --dry-run --json` impactAnalysis vs `ee_revision_impact`
#[test]
fn mcp_parity_revision_impact_command() -> TestResult {
    let fixture = load_parity_fixture("revision_impact", "basic")?;
    let dir = scenario_dir("revision_impact")?;
    init_workspace(&dir)?;
    let seed_content = optional_fixture_str(&fixture, "seedMemoryContent")?
        .ok_or_else(|| "revision_impact parity fixture missing seedMemoryContent".to_string())?;
    let memory_id = remember_test_memory(&dir, seed_content)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, Some(&memory_id), None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI memory revise --dry-run failed",
    )?;
    let cli_impact =
        extract_json_pointer_text(&cli_stdout, "/data/impactAnalysis", "revision impact")?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, Some(&memory_id), None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_impact, &mcp_text, "revision impact")
}

/// Parity test: `ee why <memory-id> --json` vs `ee_why` MCP tool
#[test]
fn mcp_parity_why_command() -> TestResult {
    let fixture = load_parity_fixture("why", "basic")?;
    let dir = scenario_dir("why")?;
    init_workspace(&dir)?;
    let seed_content = optional_fixture_str(&fixture, "seedMemoryContent")?
        .ok_or_else(|| "why parity fixture missing seedMemoryContent".to_string())?;
    let memory_id = remember_test_memory(&dir, seed_content)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, Some(&memory_id), None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI why failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, Some(&memory_id), None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "why")
}

/// Parity test: `ee memory show <memory-id> --json` vs `ee_memory_show` MCP tool
#[test]
fn mcp_parity_memory_show_command() -> TestResult {
    let fixture = load_parity_fixture("memory_show", "basic")?;
    let dir = scenario_dir("memory_show")?;
    init_workspace(&dir)?;
    let seed_content = optional_fixture_str(&fixture, "seedMemoryContent")?
        .ok_or_else(|| "memory_show parity fixture missing seedMemoryContent".to_string())?;
    let memory_id = remember_test_memory(&dir, seed_content)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, Some(&memory_id), None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI memory show failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, Some(&memory_id), None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "memory show")
}

/// Parity test: `ee remember --dry-run --json` vs `ee_remember` MCP tool (default dry-run)
#[test]
fn mcp_parity_remember_dry_run_command() -> TestResult {
    let fixture = load_parity_fixture("remember", "dry_run")?;
    let dir = scenario_dir("remember")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI remember --dry-run failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "remember --dry-run")
}

/// Parity test: `ee outcome --dry-run --json` vs `ee_outcome` MCP tool
#[test]
fn mcp_parity_outcome_dry_run_command() -> TestResult {
    let fixture = load_parity_fixture("outcome", "dry_run")?;
    let dir = scenario_dir("outcome")?;
    init_workspace(&dir)?;
    let seed_content = optional_fixture_str(&fixture, "seedMemoryContent")?
        .ok_or_else(|| "outcome parity fixture missing seedMemoryContent".to_string())?;
    let memory_id = remember_test_memory(&dir, seed_content)?;
    let event_id = "fb_01234567890123456789012345";

    let (cli_exit, cli_stdout, _cli_stderr) = run_cli(fixture_cli_args(
        &fixture,
        &dir,
        Some(&memory_id),
        Some(event_id),
    )?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI outcome --dry-run failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, Some(&memory_id), Some(event_id))?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "outcome --dry-run")
}

/// Parity test: `ee mesh discovery-policy --json` vs `ee_mesh_discovery_policy` MCP tool.
#[test]
fn mcp_parity_mesh_discovery_policy_command() -> TestResult {
    let fixture = load_parity_fixture("mesh_discovery_policy", "inspect")?;
    let dir = scenario_dir("mesh_discovery_policy")?;
    init_workspace(&dir)?;

    let (cli_exit, cli_stdout, _cli_stderr) =
        run_cli(fixture_cli_args(&fixture, &dir, None, None)?);
    ensure(
        cli_exit == ee::models::ProcessExitCode::Success,
        "CLI mesh discovery-policy failed",
    )?;

    let mcp_response = run_mcp_tool_call(
        fixture_mcp_tool(&fixture)?,
        fixture_mcp_arguments(&fixture, &dir, None, None)?,
    )?;
    let mcp_text = extract_mcp_tool_text(&mcp_response)?;

    assert_json_equal_modulo_timestamps(&cli_stdout, &mcp_text, "mesh discovery-policy")
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
        "ee_doctor",
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

/// Verify MCP tools/list covers every tool with an explicit parity test here.
#[test]
fn mcp_tools_list_covers_parity_tested_tools() -> TestResult {
    let response = run_mcp_tools_list()?;
    let tools = response
        .get("result")
        .and_then(|result| result.get("tools"))
        .and_then(JsonValue::as_array)
        .ok_or("tools/list response missing result.tools array")?;

    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(JsonValue::as_str))
        .collect();

    for tool in PARITY_TESTED_TOOLS {
        ensure(
            tool_names.contains(&tool),
            format!("tools/list missing parity-tested tool: {tool}"),
        )?;
    }

    let listed: BTreeSet<&str> = tool_names.into_iter().collect();
    let tested: BTreeSet<&str> = PARITY_TESTED_TOOLS.iter().copied().collect();
    let registered: BTreeSet<&str> = ee::mcp::registered_tool_names().collect();
    ensure(
        listed == registered,
        format!(
            "tools/list must be generated from the MCP registry; listed={listed:?}, registered={registered:?}"
        ),
    )?;
    ensure(
        listed == tested,
        format!("tools/list parity coverage mismatch; listed={listed:?}, tested={tested:?}"),
    )?;

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

    let required_schemas = ["ee.response.v2", "ee.error.v2"];

    for schema in required_schemas {
        ensure(
            schema_ids.contains(&schema),
            format!("schema list missing required schema: {schema}"),
        )?;
    }

    Ok(())
}
