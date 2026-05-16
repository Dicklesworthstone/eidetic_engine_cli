//! bd-1prrl.2.5: per-agent context-profile e2e evidence.
//!
//! This test drives the real `ee` binary because the contract spans remember,
//! outcome, context, and why surfaces. It deliberately retains its temporary
//! workspace on disk; AGENTS.md forbids agent-side file deletion.

use std::fs;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use insta::assert_json_snapshot;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const AGENT_ALPHA: &str = "AgentProfileAlpha";
const AGENT_BETA: &str = "AgentProfileBeta";
const AGENT_GAMMA: &str = "AgentProfileGamma";
const OUTCOME_REPETITIONS: usize = 10;
const QUERY: &str = "agent profile calibration sentinel";

fn workspace_dir() -> Result<String, String> {
    let mut root = std::env::var("EE_E2E_TMPDIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    if root.starts_with("/Volumes/") {
        root = "/tmp".to_string();
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX epoch: {error}"))?
        .as_nanos();
    let path = format!(
        "{}/ee-agent-profile-e2e-{}-{nanos}",
        root.trim_end_matches('/'),
        std::process::id()
    );
    fs::create_dir_all(&path)
        .map_err(|error| format!("failed to create retained workspace {path}: {error}"))?;
    Ok(path)
}

fn run_ee(workspace: &str, args: &[&str], agent_name: Option<&str>) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY");
    if let Some(agent_name) = agent_name {
        command.env("EE_AGENT_NAME", agent_name);
    } else {
        command.env_remove("EE_AGENT_NAME");
    }
    command
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure_success(output: &Output, label: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label}: ee exited {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ))
    }
}

fn stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn remember(workspace: &str, label: &str) -> Result<String, String> {
    let content = format!(
        "{QUERY}: {label} memory. This memory has identical retrieval terms for deterministic profile-rank comparison."
    );
    let output = run_ee(
        workspace,
        &[
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            &content,
            "--json",
        ],
        None,
    )?;
    ensure_success(&output, &format!("remember {label}"))?;
    let json = stdout_json(&output, &format!("remember {label}"))?;
    json.pointer("/data/memory_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("remember {label}: missing /data/memory_id"))
}

fn record_outcomes(
    workspace: &str,
    agent_name: &str,
    memory_id: &str,
    signal: &str,
    label: &str,
) -> TestResult {
    for index in 0..OUTCOME_REPETITIONS {
        let agent_seed = match agent_name {
            AGENT_ALPHA => 1,
            AGENT_BETA => 2,
            AGENT_GAMMA => 3,
            _ => 9,
        };
        let label_seed = if label == "alpha" { 1 } else { 2 };
        let signal_seed = if signal == "helpful" { 1 } else { 2 };
        let event_seed = agent_seed * 1000 + label_seed * 100 + signal_seed * 10 + index;
        let event_id = format!("fb_{event_seed:026}");
        let source_id = format!("src_{agent_name}_{label}_{index}");
        let reason = format!("{agent_name} {signal} profile evidence for {label} #{index}");
        let output = run_ee(
            workspace,
            &[
                "outcome",
                memory_id,
                "--signal",
                signal,
                "--source-id",
                &source_id,
                "--event-id",
                &event_id,
                "--reason",
                &reason,
                "--harmful-per-source-per-hour",
                "100",
                "--json",
            ],
            Some(agent_name),
        )?;
        ensure_success(
            &output,
            &format!("outcome {agent_name} {signal} {label} {index}"),
        )?;
    }
    Ok(())
}

fn context_json(workspace: &str, agent_name: &str) -> Result<Value, String> {
    let output = run_ee(
        workspace,
        &[
            "context",
            QUERY,
            "--max-tokens",
            "1000",
            "--explain",
            "--json",
        ],
        Some(agent_name),
    )?;
    ensure_success(&output, &format!("context {agent_name}"))?;
    stdout_json(&output, &format!("context {agent_name}"))
}

fn why_json(workspace: &str, agent_name: &str, memory_id: &str) -> Result<Value, String> {
    let output = run_ee(workspace, &["why", memory_id, "--json"], Some(agent_name))?;
    ensure_success(&output, &format!("why {agent_name} {memory_id}"))?;
    stdout_json(&output, &format!("why {agent_name} {memory_id}"))
}

fn context_memory_ids(value: &Value) -> Result<Vec<String>, String> {
    let items = value
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .ok_or_else(|| "context output missing /data/pack/items".to_string())?;
    items
        .iter()
        .map(|item| {
            item.get("memoryId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("context item missing memoryId: {item:?}"))
        })
        .collect()
}

fn agent_profile(value: &Value) -> Result<&Value, String> {
    value
        .pointer("/data/pack/agentProfile")
        .ok_or_else(|| "context output missing /data/pack/agentProfile".to_string())
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

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn assert_profile_contract(profile: &Value, agent_name: &str) -> TestResult {
    ensure_equal(
        &profile["schema"],
        &json!("ee.context.agent_profile.v1"),
        "profile schema",
    )?;
    ensure_equal(&profile["agentName"], &json!(agent_name), "profile agent")?;
    ensure_equal(&profile["coldStart"], &json!(false), "profile cold start")?;
    ensure_equal(
        &profile["observedOutcomes"],
        &json!(OUTCOME_REPETITIONS * 2),
        "profile observed outcomes",
    )?;
    let bias = profile["biasMagnitude"]
        .as_f64()
        .ok_or_else(|| "profile biasMagnitude must be numeric".to_string())?;
    ensure(
        bias <= 0.05,
        format!("profile biasMagnitude must stay capped at 0.05, got {bias}"),
    )?;
    ensure(
        profile["memoryBiasApplied"].as_u64().unwrap_or(0) >= 2,
        "profile should apply bias to both fixture memories",
    )
}

fn normalized_profile_snapshot(profile: &Value, alpha_id: &str, beta_id: &str) -> Value {
    let mut value = profile.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("workspaceId".to_string(), json!("<workspace-id>"));
        object.insert("agentNameHash".to_string(), json!("<agent-hash>"));
        if let Some(determinism) = object
            .get_mut("determinismKey")
            .and_then(Value::as_object_mut)
        {
            determinism.insert("agentNameHash".to_string(), json!("<agent-hash>"));
            determinism.insert("basePackHash".to_string(), json!("<pack-hash>"));
        }
        if let Some(top_biases) = object.get_mut("topBiases").and_then(Value::as_array_mut) {
            for bias in top_biases {
                if let Some(bias_object) = bias.as_object_mut() {
                    let normalized_id = match bias_object.get("memoryId").and_then(Value::as_str) {
                        Some(id) if id == alpha_id => "<alpha-memory>",
                        Some(id) if id == beta_id => "<beta-memory>",
                        _ => "<other-memory>",
                    };
                    bias_object.insert("memoryId".to_string(), json!(normalized_id));
                    bias_object.insert("lastSeenAt".to_string(), json!("<timestamp>"));
                }
            }
        }
    }
    value
}

fn assert_agent_profile_snapshot(value: Value) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        assert_json_snapshot!("agent_profile", value);
    });
}

#[test]
fn asymmetric_agent_outcomes_bias_context_and_why_without_cross_agent_leakage() -> TestResult {
    let workspace = workspace_dir()?;
    ensure_success(&run_ee(&workspace, &["init", "--json"], None)?, "init")?;

    let alpha_memory = remember(&workspace, "alpha preferred")?;
    let beta_memory = remember(&workspace, "beta preferred")?;

    record_outcomes(&workspace, AGENT_ALPHA, &alpha_memory, "helpful", "alpha")?;
    record_outcomes(&workspace, AGENT_ALPHA, &beta_memory, "harmful", "beta")?;
    record_outcomes(&workspace, AGENT_BETA, &alpha_memory, "harmful", "alpha")?;
    record_outcomes(&workspace, AGENT_BETA, &beta_memory, "helpful", "beta")?;

    let alpha_context = context_json(&workspace, AGENT_ALPHA)?;
    let beta_context = context_json(&workspace, AGENT_BETA)?;
    let gamma_context_first = context_json(&workspace, AGENT_GAMMA)?;
    let gamma_context_second = context_json(&workspace, AGENT_GAMMA)?;

    let alpha_ids = context_memory_ids(&alpha_context)?;
    let beta_ids = context_memory_ids(&beta_context)?;
    ensure_equal(
        &alpha_ids.first(),
        &Some(&alpha_memory),
        "agent alpha should rank its helpful memory first",
    )?;
    ensure_equal(
        &beta_ids.first(),
        &Some(&beta_memory),
        "agent beta should rank its helpful memory first",
    )?;

    let alpha_profile = agent_profile(&alpha_context)?;
    let beta_profile = agent_profile(&beta_context)?;
    assert_profile_contract(alpha_profile, AGENT_ALPHA)?;
    assert_profile_contract(beta_profile, AGENT_BETA)?;
    ensure(
        gamma_context_first
            .pointer("/data/pack/agentProfile")
            .is_none(),
        "third agent without profile history should not receive another agent's profile block",
    )?;
    ensure_equal(
        &context_memory_ids(&gamma_context_first)?,
        &context_memory_ids(&gamma_context_second)?,
        "third-agent unbiased ranking must be deterministic",
    )?;

    let alpha_why = why_json(&workspace, AGENT_ALPHA, &alpha_memory)?;
    ensure_equal(
        &alpha_why["data"]["agentProfile"]["helpfulCount"],
        &json!(OUTCOME_REPETITIONS),
        "why helpful count for alpha memory",
    )?;
    ensure_equal(
        &alpha_why["data"]["agentProfile"]["harmfulCount"],
        &json!(0),
        "why harmful count for alpha memory",
    )?;

    assert_agent_profile_snapshot(normalized_profile_snapshot(
        alpha_profile,
        &alpha_memory,
        &beta_memory,
    ));
    Ok(())
}
