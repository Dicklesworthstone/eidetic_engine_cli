//! Missing-rule contract for the public `ee rule provenance` command.

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
        .join("ee-rule-provenance-integration")
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

#[test]
fn rule_provenance_unknown_rule_returns_structured_empty_report() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let mut init_args = workspace_args(&workspace);
    init_args.push("init".to_owned());
    run_ee(&workspace, init_args)?;

    let unknown_rule_id = ee::models::RuleId::from_uuid(uuid::Uuid::from_u128(99)).to_string();
    let mut provenance_args = workspace_args(&workspace);
    provenance_args.extend([
        "rule".to_owned(),
        "provenance".to_owned(),
        unknown_rule_id.clone(),
    ]);
    let output = run_ee(&workspace, provenance_args)?;
    let json = parse_stdout_json(&output, "rule provenance missing rule")?;

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
        json["data"]["ruleId"].as_str() == Some(unknown_rule_id.as_str()),
        "reported rule id",
    )?;
    ensure(
        json["data"]["status"] == "rule_not_found",
        "rule not found status",
    )?;
    ensure(
        json["data"]["citedMemories"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "cited memories should be empty",
    )?;
    ensure(
        json["data"]["coCitingRules"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "co-citing rules should be empty",
    )
}
