//! E2E test for profile workflow: probe → recommend → plan → apply
//!
//! Validates the profile configuration workflow with logged outputs:
//! 1. Host probe produces structured JSON with CPU, memory, path, tool info
//! 2. Profile recommendation is deterministic and well-reasoned
//! 3. Config plan shows exact TOML changes before writing
//! 4. Config apply (dry-run) produces stable JSON report
//!
//! NO MOCKS. Real ee binary, real workspace, real host probe.

#![cfg(unix)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
        .as_nanos();
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("profile-workflow")
        .join(format!("{}-{}-{nanos}", name, std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn path_arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn run_ee(workspace: &Path, args: &[&str]) -> Result<(i32, Value, String), String> {
    let mut full_args = vec!["--workspace", path_arg(workspace)?, "--json"];
    full_args.extend(args);

    let output = Command::new(ee_bin())
        .args(&full_args)
        .env_remove("EE_WORKSPACE")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("spawn ee: {error}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let parsed: Value = serde_json::from_str(&stdout).map_err(|error| {
        format!("parse stdout JSON: {error}\nstdout: {stdout}\nstderr: {stderr}")
    })?;

    Ok((exit_code, parsed, stderr.into_owned()))
}

fn log_phase(name: &str, json: &Value) {
    eprintln!("\n=== PHASE: {} ===", name);
    if let Some(schema) = json.pointer("/schema").and_then(Value::as_str) {
        eprintln!("  schema: {}", schema);
    }
    if let Some(success) = json.pointer("/success").and_then(Value::as_bool) {
        eprintln!("  success: {}", success);
    }
    if let Some(data_schema) = json.pointer("/data/schema").and_then(Value::as_str) {
        eprintln!("  data.schema: {}", data_schema);
    }
}

#[test]
fn profile_workflow_probe_recommend_plan_apply_dryrun() -> TestResult {
    let root = unique_artifact_dir("workflow")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    // Phase 1: Initialize workspace
    eprintln!("\n=== PHASE 1: Initialize workspace ===");
    let (exit_code, parsed, stderr) = run_ee(&workspace, &["init"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "init exit code")?;
    ensure(
        parsed.pointer("/success") == Some(&json!(true)),
        format!("init must succeed: {parsed}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("init stderr must be empty: {stderr}"),
    )?;
    log_phase("init", &parsed);

    // Phase 2: Profile config plan (dry-run by default)
    eprintln!("\n=== PHASE 2: Profile config plan ===");
    let (exit_code, plan_json, stderr) = run_ee(&workspace, &["profile", "config", "plan"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "profile config plan exit code")?;
    ensure(
        stderr.is_empty(),
        format!("JSON mode must keep stderr empty, got: {stderr}"),
    )?;
    log_phase("profile config plan", &plan_json);

    // Validate plan JSON structure
    ensure_equal(
        &plan_json.pointer("/schema"),
        &Some(&json!("ee.response.v1")),
        "plan response schema",
    )?;
    ensure_equal(
        &plan_json.pointer("/data/schema"),
        &Some(&json!("ee.profile.config.plan.v1")),
        "plan data schema",
    )?;
    ensure_equal(
        &plan_json.pointer("/data/dryRun"),
        &Some(&json!(true)),
        "plan must be dry-run by default",
    )?;

    // Validate probe data is present
    let probe = plan_json
        .pointer("/data/probe")
        .ok_or("missing probe in plan")?;
    ensure_equal(
        &probe.pointer("/schema"),
        &Some(&json!("ee.host_profile.v1")),
        "probe schema",
    )?;
    ensure(
        probe
            .pointer("/cpu/logicalCores")
            .and_then(Value::as_u64)
            .is_some(),
        "probe must include CPU logical cores",
    )?;
    ensure(
        probe
            .pointer("/memory/totalBytes")
            .and_then(Value::as_u64)
            .is_some(),
        "probe must include memory total bytes",
    )?;

    // Log probe summary
    if let Some(cores) = probe.pointer("/cpu/logicalCores").and_then(Value::as_u64) {
        eprintln!("  probe.cpu.logicalCores: {}", cores);
    }
    if let Some(mem) = probe
        .pointer("/memory/availableBytes")
        .and_then(Value::as_u64)
    {
        eprintln!(
            "  probe.memory.availableBytes: {} GiB",
            mem / (1024 * 1024 * 1024)
        );
    }

    // Validate profile recommendation
    let profile = plan_json
        .pointer("/data/profile")
        .ok_or("missing profile in plan")?;
    let recommended = profile
        .pointer("/recommended")
        .and_then(Value::as_str)
        .ok_or("missing recommended profile")?;
    let confidence = profile
        .pointer("/confidence")
        .and_then(Value::as_str)
        .ok_or("missing confidence")?;
    eprintln!("  profile.recommended: {}", recommended);
    eprintln!("  profile.confidence: {}", confidence);
    ensure(
        ["constrained", "portable", "workstation", "swarm"].contains(&recommended),
        format!("recommended profile must be valid: {recommended}"),
    )?;
    ensure(
        ["low", "medium", "high"].contains(&confidence),
        format!("confidence must be valid: {confidence}"),
    )?;

    // Validate budgets are present
    let budgets = plan_json
        .pointer("/data/budgets")
        .ok_or("missing budgets in plan")?;
    ensure(
        budgets
            .pointer("/search/candidateLimit")
            .and_then(Value::as_u64)
            .is_some(),
        "budgets must include search.candidateLimit",
    )?;
    ensure(
        budgets
            .pointer("/pack/maxTokens")
            .and_then(Value::as_u64)
            .is_some(),
        "budgets must include pack.maxTokens",
    )?;
    eprintln!(
        "  budgets.search.candidateLimit: {:?}",
        budgets.pointer("/search/candidateLimit")
    );
    eprintln!(
        "  budgets.pack.maxTokens: {:?}",
        budgets.pointer("/pack/maxTokens")
    );

    // Validate planned TOML edits
    let edits = plan_json
        .pointer("/data/edits")
        .and_then(Value::as_array)
        .ok_or("missing edits array in plan")?;
    ensure(!edits.is_empty(), "plan must include at least one edit")?;
    eprintln!("  edits.count: {}", edits.len());

    // Validate planned TOML string
    let planned_toml = plan_json
        .pointer("/data/plannedToml")
        .and_then(Value::as_str)
        .ok_or("missing plannedToml in plan")?;
    ensure(
        planned_toml.contains("profile"),
        "plannedToml must mention profile",
    )?;
    eprintln!("  plannedToml.len: {} bytes", planned_toml.len());

    // Phase 3: Profile config apply (with --dry-run to avoid writing)
    eprintln!("\n=== PHASE 3: Profile config apply --dry-run ===");
    let (exit_code, apply_json, stderr) =
        run_ee(&workspace, &["profile", "config", "apply", "--dry-run"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "profile config apply exit code")?;
    ensure(
        stderr.is_empty(),
        format!("JSON mode must keep stderr empty, got: {stderr}"),
    )?;
    log_phase("profile config apply --dry-run", &apply_json);

    // Validate apply JSON structure matches plan
    ensure_equal(
        &apply_json.pointer("/data/schema"),
        &Some(&json!("ee.profile.config.plan.v1")),
        "apply data schema",
    )?;
    ensure_equal(
        &apply_json.pointer("/data/dryRun"),
        &Some(&json!(true)),
        "apply must respect --dry-run",
    )?;
    ensure_equal(
        &apply_json.pointer("/data/applied"),
        &Some(&json!(false)),
        "dry-run must not apply changes",
    )?;

    // Verify profile recommendation is consistent between plan and apply
    let apply_recommended = apply_json
        .pointer("/data/profile/recommended")
        .and_then(Value::as_str)
        .ok_or("missing recommended in apply")?;
    ensure_equal(
        &apply_recommended,
        &recommended,
        "profile recommendation must be deterministic",
    )?;

    eprintln!("\n=== PROFILE WORKFLOW COMPLETE ===");
    eprintln!("  Recommended profile: {}", recommended);
    eprintln!("  Planned edits: {}", edits.len());
    eprintln!("  Dry-run: verified");

    Ok(())
}

#[test]
fn profile_probe_includes_tool_availability() -> TestResult {
    let root = unique_artifact_dir("tool-probe")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    // Initialize workspace
    let (exit_code, _, _) = run_ee(&workspace, &["init"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "init exit code")?;

    // Get profile config plan to access probe
    let (exit_code, plan_json, _) = run_ee(&workspace, &["profile", "config", "plan"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "profile config plan exit code")?;

    // Validate tool probe data
    let tools = plan_json
        .pointer("/data/probe/tools")
        .and_then(Value::as_array)
        .ok_or("missing tools array in probe")?;

    // Should probe at least cargo, br, bv
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.pointer("/name").and_then(Value::as_str))
        .collect();
    ensure(
        tool_names.contains(&"cargo"),
        format!("tools must include cargo: {tool_names:?}"),
    )?;
    ensure(
        tool_names.contains(&"br"),
        format!("tools must include br: {tool_names:?}"),
    )?;
    ensure(
        tool_names.contains(&"bv"),
        format!("tools must include bv: {tool_names:?}"),
    )?;

    // Log tool availability
    eprintln!("\n=== TOOL PROBE ===");
    for tool in tools {
        if let (Some(name), Some(available)) = (
            tool.pointer("/name").and_then(Value::as_str),
            tool.pointer("/available").and_then(Value::as_bool),
        ) {
            eprintln!(
                "  {}: {}",
                name,
                if available { "available" } else { "missing" }
            );
        }
    }

    Ok(())
}

#[test]
fn profile_probe_includes_path_filesystem_info() -> TestResult {
    let root = unique_artifact_dir("path-probe")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    // Initialize workspace
    let (exit_code, _, _) = run_ee(&workspace, &["init"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "init exit code")?;

    // Get profile config plan to access probe
    let (exit_code, plan_json, _) = run_ee(&workspace, &["profile", "config", "plan"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "profile config plan exit code")?;

    // Validate path probe data
    let paths = plan_json
        .pointer("/data/probe/paths")
        .and_then(Value::as_array)
        .ok_or("missing paths array in probe")?;

    // Should probe at least workspace, temp, cargo_target
    let path_labels: Vec<&str> = paths
        .iter()
        .filter_map(|p| p.pointer("/label").and_then(Value::as_str))
        .collect();
    ensure(
        path_labels.contains(&"workspace"),
        format!("paths must include workspace: {path_labels:?}"),
    )?;
    ensure(
        path_labels.contains(&"temp"),
        format!("paths must include temp: {path_labels:?}"),
    )?;

    // Log path info
    eprintln!("\n=== PATH PROBE ===");
    for path in paths {
        if let (Some(label), Some(exists)) = (
            path.pointer("/label").and_then(Value::as_str),
            path.pointer("/exists").and_then(Value::as_bool),
        ) {
            let available = path
                .pointer("/availableBytes")
                .and_then(Value::as_u64)
                .map(|b| format!("{} GiB", b / (1024 * 1024 * 1024)))
                .unwrap_or_else(|| "unknown".to_string());
            eprintln!("  {}: exists={}, available={}", label, exists, available);
        }
    }

    Ok(())
}

#[test]
fn profile_config_plan_is_idempotent() -> TestResult {
    let root = unique_artifact_dir("idempotent")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    // Initialize workspace
    let (exit_code, _, _) = run_ee(&workspace, &["init"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "init exit code")?;

    // Run profile config plan twice
    let (_, plan1, _) = run_ee(&workspace, &["profile", "config", "plan"])?;
    let (_, plan2, _) = run_ee(&workspace, &["profile", "config", "plan"])?;

    // Extract deterministic fields (excluding timing)
    let recommended1 = plan1.pointer("/data/profile/recommended");
    let recommended2 = plan2.pointer("/data/profile/recommended");
    ensure_equal(
        &recommended1,
        &recommended2,
        "recommended profile must be stable",
    )?;

    let edits1 = plan1
        .pointer("/data/edits")
        .and_then(Value::as_array)
        .map(|a| a.len());
    let edits2 = plan2
        .pointer("/data/edits")
        .and_then(Value::as_array)
        .map(|a| a.len());
    ensure_equal(&edits1, &edits2, "edit count must be stable")?;

    let toml1 = plan1.pointer("/data/plannedToml");
    let toml2 = plan2.pointer("/data/plannedToml");
    ensure_equal(&toml1, &toml2, "planned TOML must be stable")?;

    eprintln!("\n=== IDEMPOTENCY CHECK PASSED ===");

    Ok(())
}
