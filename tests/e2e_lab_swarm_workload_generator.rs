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

fn run_promote_workload(
    workspace: &Path,
    trace_path: &Path,
    profile: &str,
    agents: u16,
) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .arg("lab")
        .arg("promote-workload")
        .arg("--trace")
        .arg(trace_path)
        .arg("--profile")
        .arg(profile)
        .arg("--agents")
        .arg(agents.to_string())
        .output()
        .map_err(|error| error.to_string())
}

fn run_swarm_replay(
    workspace: &Path,
    trace_path: &Path,
    rch_proof_path: Option<&Path>,
) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .arg("lab")
        .arg("swarm")
        .arg("replay")
        .arg("--trace")
        .arg(trace_path);
    if let Some(rch_proof_path) = rch_proof_path {
        command.arg("--rch-proof").arg(rch_proof_path);
    }
    command.output().map_err(|error| error.to_string())
}

fn run_swarm_replay_dry_run(workspace: &Path, trace_path: &Path) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .arg("lab")
        .arg("swarm")
        .arg("replay")
        .arg("--trace")
        .arg(trace_path)
        .arg("--dry-run")
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
            value["generatorEvidence"]["commandCount"].as_u64() == Some(command_count as u64),
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
fn lab_promote_workload_turns_redacted_agent_trace_into_replayable_swarm_trace() -> TestResult {
    let workspace = unique_workspace("promote-recorded")?;
    let source_trace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("agent_workloads")
        .join("redacted_trace_minimal.jsonl");

    let promote_output = run_promote_workload(&workspace, &source_trace, "small", 4)?;
    let (promote_stdout, promote_stderr) = output_text(&promote_output)?;
    ensure(
        promote_output.status.success(),
        format!(
            "ee lab promote-workload failed: status={:?} stdout={promote_stdout} stderr={promote_stderr}",
            promote_output.status.code()
        ),
    )?;
    ensure(
        promote_stderr.trim().is_empty(),
        format!("expected promote-workload stderr to be empty, got {promote_stderr}"),
    )?;

    let promoted: Value = serde_json::from_str(&promote_stdout)
        .map_err(|error| format!("parse promoted workload JSON: {error}: {promote_stdout}"))?;
    ensure(
        promoted["schema"] == "ee.swarm_workload.v1",
        format!("promoted schema mismatch: {promoted}"),
    )?;
    ensure(
        promoted["provenance"]["kind"] == "recorded",
        format!("promoted provenance kind mismatch: {promoted}"),
    )?;
    ensure(
        promoted["provenance"]["sourceTraceHashes"]
            .as_array()
            .is_some_and(|hashes| hashes.len() == 1),
        format!("promoted source trace hashes missing: {promoted}"),
    )?;
    ensure(
        promoted["generatorEvidence"]["profile"] == "recorded",
        format!("promoted generator evidence profile mismatch: {promoted}"),
    )?;
    ensure(
        promoted["agentCount"].as_u64() == Some(4),
        format!("promoted agent count mismatch: {promoted}"),
    )?;
    ensure(
        promoted["commandSequence"].as_array().map(Vec::len) == Some(4),
        format!("promoted command count mismatch: {promoted}"),
    )?;
    ensure(
        promoted["commandSequence"][0]["command"]["verbs"][0] == "context",
        format!("promoted command shape should preserve recorded context verb: {promoted}"),
    )?;
    ensure(
        promoted["expectedDegradedPosture"] == "recoverable",
        format!("promoted degraded posture mismatch: {promoted}"),
    )?;

    for forbidden in [
        workspace.display().to_string(),
        source_trace.display().to_string(),
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
            !promote_stdout.contains(&forbidden),
            format!("promoted workload leaked forbidden marker {forbidden}"),
        )?;
    }

    let promoted_trace_path = workspace.join("recorded-swarm-workload.json");
    fs::write(&promoted_trace_path, &promote_stdout)
        .map_err(|error| format!("write {}: {error}", promoted_trace_path.display()))?;

    let replay_output = run_swarm_replay_dry_run(&workspace, &promoted_trace_path)?;
    let (replay_stdout, replay_stderr) = output_text(&replay_output)?;
    ensure(
        replay_output.status.code() == Some(6),
        format!(
            "dry-run replay should exit degraded-required without RCH proof; status={:?} stdout={replay_stdout} stderr={replay_stderr}",
            replay_output.status.code()
        ),
    )?;
    ensure(
        replay_stderr.trim().is_empty(),
        format!("expected dry-run replay stderr to be empty, got {replay_stderr}"),
    )?;

    let replay: Value = serde_json::from_str(&replay_stdout)
        .map_err(|error| format!("parse replay JSON: {error}: {replay_stdout}"))?;
    ensure(
        replay["schema"] == "ee.swarm_replay_result.v1",
        format!("replay schema mismatch: {replay}"),
    )?;
    ensure(
        replay["status"] == "degraded",
        format!("replay status mismatch: {replay}"),
    )?;
    ensure(
        replay["aggregate"]["commandCount"].as_u64() == Some(4),
        format!("replay command count mismatch: {replay}"),
    )?;
    let dry_run_slo_total = replay["aggregate"]["sloPassCount"]
        .as_u64()
        .unwrap_or_default()
        + replay["aggregate"]["sloWarningCount"]
            .as_u64()
            .unwrap_or_default()
        + replay["aggregate"]["sloFailureCount"]
            .as_u64()
            .unwrap_or_default()
        + replay["aggregate"]["sloExemptCount"]
            .as_u64()
            .unwrap_or_default();
    ensure(
        dry_run_slo_total == 4,
        format!("dry-run replay SLO counts should cover all commands: {replay}"),
    )?;
    let dry_run_commands = replay["commandResults"]
        .as_array()
        .ok_or_else(|| format!("dry-run replay missing commandResults array: {replay}"))?;
    for result in dry_run_commands {
        ensure(
            result["elapsedMs"].as_u64().is_some()
                && result["stdoutBytes"].as_u64().is_some()
                && result["stderrBytes"].as_u64().is_some()
                && result["slo"]["class"].is_string()
                && result["slo"]["status"].is_string(),
            format!("dry-run replay command missing SLO-visible metrics: {result}"),
        )?;
    }
    ensure(
        replay["firstFailure"].is_null(),
        format!("dry-run recorded replay should have no first failure: {replay}"),
    )?;
    ensure(
        replay["warnings"].as_array().is_some_and(|warnings| {
            warnings.iter().any(|warning| {
                warning
                    .as_str()
                    .is_some_and(|text| text.contains("swarm_replay_dry_run_admission_only"))
            })
        }),
        format!("dry-run replay warning missing: {replay}"),
    )?;

    Ok(())
}

#[test]
fn lab_swarm_replay_executes_small_generated_fixture_with_artifact_ledger() -> TestResult {
    let workspace = unique_workspace("replay-small")?;
    let generate_output =
        run_generate_workload(&workspace, "small", "stable_small_replay_seed_001")?;
    let (generate_stdout, generate_stderr) = output_text(&generate_output)?;
    ensure(
        generate_output.status.success(),
        format!(
            "ee lab generate-workload small failed: status={:?} stdout={generate_stdout} stderr={generate_stderr}",
            generate_output.status.code()
        ),
    )?;

    let mut trace: Value = serde_json::from_str(&generate_stdout)
        .map_err(|error| format!("parse generated workload JSON: {error}: {generate_stdout}"))?;
    let commands = trace["commandSequence"]
        .as_array_mut()
        .ok_or_else(|| "generated trace missing commandSequence array".to_owned())?;
    for command in commands {
        command["timeoutMs"] = Value::from(30_000u64);
    }
    let trace_path = workspace.join("small-swarm-workload.json");
    fs::write(
        &trace_path,
        serde_json::to_string_pretty(&trace).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("write {}: {error}", trace_path.display()))?;

    let replay_output = run_swarm_replay(&workspace, &trace_path, None)?;
    let (replay_stdout, replay_stderr) = output_text(&replay_output)?;
    ensure(
        replay_output.status.code() == Some(6),
        format!(
            "swarm replay should exit degraded-required because RCH proof is not attached; status={:?} stdout={replay_stdout} stderr={replay_stderr}",
            replay_output.status.code()
        ),
    )?;
    ensure(
        replay_stderr.trim().is_empty(),
        format!("expected machine command stderr to be empty, got {replay_stderr}"),
    )?;

    let replay: Value = serde_json::from_str(&replay_stdout)
        .map_err(|error| format!("parse replay JSON: {error}: {replay_stdout}"))?;
    ensure(
        replay["schema"] == "ee.swarm_replay_result.v1",
        format!("replay schema mismatch: {replay}"),
    )?;
    ensure(
        replay["status"] == "degraded",
        format!("replay status mismatch: {replay}"),
    )?;
    ensure(
        replay["aggregate"]["commandCount"].as_u64() == Some(6),
        format!("replay command count mismatch: {replay}"),
    )?;
    ensure(
        replay["aggregate"]["successCount"].as_u64() == Some(6),
        format!("replay success count mismatch: {replay}"),
    )?;
    ensure(
        replay["aggregate"]["failureCount"].as_u64() == Some(0),
        format!("replay failure count mismatch: {replay}"),
    )?;
    let slo_total = replay["aggregate"]["sloPassCount"]
        .as_u64()
        .unwrap_or_default()
        + replay["aggregate"]["sloWarningCount"]
            .as_u64()
            .unwrap_or_default()
        + replay["aggregate"]["sloFailureCount"]
            .as_u64()
            .unwrap_or_default()
        + replay["aggregate"]["sloExemptCount"]
            .as_u64()
            .unwrap_or_default();
    ensure(
        slo_total == 6,
        format!("replay SLO counts should cover all commands: {replay}"),
    )?;
    ensure(
        replay["aggregate"].get("firstSloFailureStepId").is_some(),
        format!("replay aggregate missing first SLO failure pointer: {replay}"),
    )?;
    ensure(
        replay["verification"]["rchStatus"] == "blocked_before_cargo",
        format!("replay RCH status mismatch: {replay}"),
    )?;
    ensure(
        replay["warnings"].as_array().is_some_and(|warnings| {
            warnings.iter().any(|warning| {
                warning
                    .as_str()
                    .is_some_and(|text| text.contains("swarm_replay_rch_proof_missing"))
            })
        }),
        format!("replay warning missing RCH proof posture: {replay}"),
    )?;

    let command_results = replay["commandResults"]
        .as_array()
        .ok_or_else(|| format!("replay missing commandResults array: {replay}"))?;
    for result in command_results {
        ensure(
            result["artifactPaths"]
                .as_array()
                .is_some_and(|artifacts| artifacts.len() == 2),
            format!("replay command missing stdout/stderr artifacts: {result}"),
        )?;
        let artifacts = result["artifactPaths"]
            .as_array()
            .ok_or_else(|| format!("replay command missing artifact array: {result}"))?;
        for artifact in artifacts {
            let path_tail = artifact["pathTail"].as_str().unwrap_or_default();
            let path_hash = artifact["pathHash"].as_str().unwrap_or_default();
            ensure(
                path_tail.starts_with(".ee/lab/swarm-replay/"),
                format!("artifact path tail is not redaction-safe: {artifact}"),
            )?;
            ensure(
                path_hash.starts_with("blake3:"),
                format!("artifact path hash is not a BLAKE3 marker: {artifact}"),
            )?;
        }
        ensure(
            result["elapsedMs"].as_u64().is_some()
                && result["stdoutBytes"].as_u64().is_some()
                && result["stderrBytes"].as_u64().is_some()
                && result["slo"]["class"].is_string()
                && result["slo"]["status"].is_string()
                && (result["slo"]["diagnosis"].is_string() || result["slo"]["diagnosis"].is_null()),
            format!("replay command missing SLO metrics or diagnosis: {result}"),
        )?;
    }

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
            !replay_stdout.contains(&forbidden),
            format!("replay ledger leaked forbidden marker {forbidden}"),
        )?;
    }

    let rch_proof_path = workspace.join("rch-proof.json");
    fs::write(
        &rch_proof_path,
        serde_json::json!({
            "schema": "ee.rch.verify.v1",
            "success": true,
            "generated_at": "2026-06-03T10:00:00Z",
            "started_at": "2026-06-03T09:59:00Z",
            "completed_at": "2026-06-03T10:00:00Z",
            "command": ["cargo", "test", "--test", "e2e_lab_swarm_workload_generator"],
            "command_text": "cargo test --test e2e_lab_swarm_workload_generator",
            "command_kind": "cargo_test",
            "command_hash": "sha256:3333333333333333333333333333333333333333333333333333333333333333",
            "remote_required": true,
            "would_offload": true,
            "worker_id": "vmi1227854",
            "exit_code": 0,
            "elapsed_ms": 415000,
            "degraded_codes": [],
            "selector_admission_probe": {
                "schema": "ee.rch.selector_admission_probe.v1",
                "status": "selected",
                "required_runtime": "Rust",
                "workers_reported": ["vmi1227854"],
                "daemon_workers_reported": ["vmi1227854"],
                "workers_reported_count": 1,
                "daemon_workers_reported_count": 1,
                "selected_worker": "vmi1227854",
                "selection_failure_reason": null,
                "workers_vs_selection_contradiction": false,
                "path_normalization_warning": null,
                "remote_required": true,
                "local_fallback_refused": false,
                "admission_blocker": null
            },
            "local_cargo_processes": {
                "schema": "ee.rch_local_cargo_tripwire.v1",
                "status": "checked",
                "count": 0,
                "processes": []
            }
        })
        .to_string(),
    )
    .map_err(|error| format!("write {}: {error}", rch_proof_path.display()))?;
    let proof_replay_output = run_swarm_replay(&workspace, &trace_path, Some(&rch_proof_path))?;
    let (proof_stdout, proof_stderr) = output_text(&proof_replay_output)?;
    ensure(
        proof_replay_output.status.success(),
        format!(
            "swarm replay with RCH proof should pass; status={:?} stdout={proof_stdout} stderr={proof_stderr}",
            proof_replay_output.status.code()
        ),
    )?;
    let proof_replay: Value = serde_json::from_str(&proof_stdout)
        .map_err(|error| format!("parse proof replay JSON: {error}: {proof_stdout}"))?;
    ensure(
        proof_replay["verification"]["rchStatus"] == "passed",
        format!("proof replay RCH status mismatch: {proof_replay}"),
    )?;
    ensure(
        proof_replay["verification"]["proofCapsule"]["proofLevel"] == "remote_verified",
        format!("proof replay proof level mismatch: {proof_replay}"),
    )?;
    ensure(
        proof_replay["verification"]["proofCapsule"]["rch"]["cargoStarted"] == Value::Bool(true),
        format!("proof replay cargo-started mismatch: {proof_replay}"),
    )?;

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
