//! No-mock CLI smoke coverage for deterministic swarm workload fixture generation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn unique_workspace(label: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let workspace = std::env::temp_dir().join(format!(
        "ee-lab-swarm-workload-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("create {}: {error}", workspace.display()))?;
    Ok(workspace)
}

fn run_generate_workload(
    workspace: &Path,
    profile: &str,
    fixture_seed: &str,
) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .arg("lab")
        .arg("generate-workload")
        .arg("--fixture-seed")
        .arg(fixture_seed)
        .arg("--profile")
        .arg(profile)
        .output()
        .map_err(|error| error.to_string())
}

fn output_text(output: &Output) -> Result<(String, String), String> {
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|error| error.to_string())?
        .to_owned();
    let stderr = std::str::from_utf8(&output.stderr)
        .map_err(|error| error.to_string())?
        .to_owned();
    Ok((stdout, stderr))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn lab_generate_workload_emits_all_profiles_as_redaction_safe_json() -> TestResult {
    let workspace = unique_workspace("profiles")?;
    let cases = [
        ("small", "stable_small_seed_001", 4, 6, "ci_smoke"),
        (
            "medium",
            "stable_medium_seed_001",
            24,
            9,
            "developer_crowded_checkout",
        ),
        (
            "large",
            "stable_large_seed_001",
            128,
            12,
            "stress_256gb_host",
        ),
    ];

    for (profile, fixture_seed, agent_count, command_count, resource_profile) in cases {
        let output = run_generate_workload(&workspace, profile, fixture_seed)?;
        let (stdout, stderr) = output_text(&output)?;
        ensure(
            output.status.success(),
            format!(
                "ee lab generate-workload {profile} failed: status={:?} stdout={stdout} stderr={stderr}",
                output.status.code()
            ),
        )?;
        ensure(
            stderr.trim().is_empty(),
            format!("expected machine command stderr to be empty, got {stderr}"),
        )?;

        let value: Value = serde_json::from_str(&stdout)
            .map_err(|error| format!("parse workload JSON for {profile}: {error}: {stdout}"))?;
        ensure(
            value["schema"] == "ee.swarm_workload.v1",
            format!("schema mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["fixtureSeed"] == fixture_seed,
            format!("fixtureSeed mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["sideEffectFree"] == Value::Bool(true),
            format!("sideEffectFree mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["agentCount"].as_u64() == Some(agent_count),
            format!("agentCount mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["commandSequence"].as_array().map(Vec::len) == Some(command_count),
            format!("commandSequence length mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["resourceProfileHints"]["profile"] == resource_profile,
            format!("resource profile mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["schema"] == "ee.swarm_workload.generator_evidence.v1",
            format!("generator evidence schema mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["fixtureSeed"] == fixture_seed,
            format!("generator evidence seed mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["profile"] == profile,
            format!("generator evidence profile mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["commandCount"].as_u64() == Some(command_count),
            format!("generator evidence command count mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["generatedMemoryCount"].as_u64() == Some(1),
            format!("generator evidence memory count mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["redactionProbeCount"].as_u64()
                == value["redactionProbes"]
                    .as_array()
                    .map(|probes| probes.len() as u64),
            format!("generator evidence redaction count mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["schemaId"]
                == "https://eidetic-engine/schemas/ee.swarm_workload.v1.json",
            format!("generator evidence schema id mismatch for {profile}: {value}"),
        )?;
        ensure(
            value["generatorEvidence"]["workspacePathHash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("blake3:"))
                && value["generatorEvidence"]["fixtureHash"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("blake3:")),
            format!("generator evidence hashes are not redaction-safe for {profile}: {value}"),
        )?;

        for forbidden in [
            workspace.display().to_string(),
            "/Users/".to_owned(),
            "/data/projects/".to_owned(),
            "raw task content".to_owned(),
            "raw query text".to_owned(),
            "memory body payload".to_owned(),
            "mail body payload".to_owned(),
            "SECRET_TOKEN".to_owned(),
            "HOME=/".to_owned(),
        ] {
            ensure(
                !stdout.contains(&forbidden),
                format!("workload {profile} leaked forbidden marker {forbidden}"),
            )?;
        }
    }

    Ok(())
}

#[test]
fn lab_generate_workload_rejects_schema_invalid_fixture_seed() -> TestResult {
    let workspace = unique_workspace("invalid-seed")?;
    let output = run_generate_workload(&workspace, "small", "Raw Prompt Seed")?;
    let (stdout, stderr) = output_text(&output)?;

    ensure(
        !output.status.success(),
        "invalid fixture seed unexpectedly succeeded",
    )?;
    ensure(
        stderr.trim().is_empty(),
        format!("expected JSON usage error on stdout only, stderr={stderr}"),
    )?;

    let value: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("parse usage error JSON: {error}: {stdout}"))?;
    ensure(
        value["schema"] == "ee.error.v2",
        format!("error schema mismatch: {value}"),
    )?;
    ensure(
        value["error"]["code"] == "usage",
        format!("error code mismatch: {value}"),
    )
}
