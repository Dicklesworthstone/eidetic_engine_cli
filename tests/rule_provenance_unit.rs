//! Focused CLI coverage for `ee rule provenance` happy-path output.

use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_run_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-rule-provenance-unit")
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn ee_binary_path() -> Result<PathBuf, String> {
    let cargo_path = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    if cargo_path.exists() {
        return Ok(cargo_path);
    }

    let current_exe = std::env::current_exe()
        .map_err(|error| format!("failed to resolve current test binary: {error}"))?;
    let debug_dir = current_exe.parent().and_then(Path::parent).ok_or_else(|| {
        format!(
            "failed to resolve debug directory from test binary {}",
            current_exe.display()
        )
    })?;
    let sibling = debug_dir.join("ee");
    if sibling.exists() {
        Ok(sibling)
    } else {
        Err(format!(
            "ee binary not found at {} or {}",
            cargo_path.display(),
            sibling.display()
        ))
    }
}

fn workspace_args(workspace: &Path) -> Vec<String> {
    vec![
        "--workspace".to_owned(),
        workspace.to_string_lossy().into_owned(),
        "--json".to_owned(),
    ]
}

fn run_ee(workspace: &Path, args: Vec<String>) -> Result<Output, String> {
    let output = Command::new(ee_binary_path()?)
        .current_dir(workspace)
        .args(&args)
        .output()
        .map_err(|error| format!("failed to run ee {args:?}: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "ee {args:?} failed: stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(output)
}

fn parse_stdout_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context} stdout was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn remember_source(workspace: &Path, content: &str) -> Result<String, String> {
    let mut args = workspace_args(workspace);
    args.extend([
        "remember".to_owned(),
        "--level".to_owned(),
        "semantic".to_owned(),
        "--kind".to_owned(),
        "fact".to_owned(),
        content.to_owned(),
    ]);
    let output = run_ee(workspace, args)?;
    parse_stdout_json(&output, "remember")?["data"]["memory_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "remember response missing memory_id".to_owned())
}

fn add_rule(workspace: &Path, source_memory_id: &str, content: &str) -> Result<String, String> {
    let mut args = workspace_args(workspace);
    args.extend([
        "rule".to_owned(),
        "add".to_owned(),
        "--source-memory".to_owned(),
        source_memory_id.to_owned(),
        content.to_owned(),
    ]);
    let output = run_ee(workspace, args)?;
    parse_stdout_json(&output, "rule add")?["data"]["ruleId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "rule add response missing ruleId".to_owned())
}

#[test]
fn rule_provenance_reports_cited_memory_and_co_citing_rule() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let mut init_args = workspace_args(&workspace);
    init_args.push("init".to_owned());
    run_ee(&workspace, init_args)?;

    let memory_id = remember_source(&workspace, "Shared release-rule source memory.")?;
    let center_rule_id = add_rule(
        &workspace,
        &memory_id,
        "Run cargo fmt before release handoff.",
    )?;
    let peer_rule_id = add_rule(
        &workspace,
        &memory_id,
        "Run cargo clippy before release handoff.",
    )?;

    let mut provenance_args = workspace_args(&workspace);
    provenance_args.extend([
        "rule".to_owned(),
        "provenance".to_owned(),
        center_rule_id.clone(),
    ]);
    let output = run_ee(&workspace, provenance_args)?;
    let json = parse_stdout_json(&output, "rule provenance")?;

    ensure(
        json["schema"] == "ee.response.v1",
        "response envelope schema",
    )?;
    ensure(json["success"] == true, "response envelope success")?;
    ensure(
        json["data"]["schema"] == "ee.graph.rule_provenance_ego.v1",
        "rule provenance schema",
    )?;
    ensure(
        json["data"]["ruleId"].as_str() == Some(center_rule_id.as_str()),
        "center rule id",
    )?;
    ensure(json["data"]["status"] == "available", "available status")?;
    ensure(
        json["data"]["citedMemories"][0]["memoryId"].as_str() == Some(memory_id.as_str()),
        "cited memory id",
    )?;
    ensure(
        json["data"]["citedMemories"][0]["otherRuleCount"] == 1,
        "other rule count",
    )?;
    ensure(
        json["data"]["coCitingRules"][0]["ruleId"].as_str() == Some(peer_rule_id.as_str()),
        "co-citing rule id",
    )?;
    ensure(
        json["data"]["coCitingRules"][0]["sharedMemoryIds"][0].as_str() == Some(memory_id.as_str()),
        "shared memory id",
    )
}
