//! Determinism tests for graph CLI commands.
//!
//! Graph commands must produce identical JSON output when invoked multiple times
//! on the same database. This verifies that node lists, community memberships,
//! and edge lists are sorted deterministically.

#![cfg(unix)]
#![cfg(feature = "graph")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::config::GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY;
use ee::db::{
    CreateCausalEvidenceInput, CreateMemoryLinkInput, DatabaseConfig, DbConnection,
    GraphSnapshotStatus, GraphSnapshotType, MemoryLinkRelation, MemoryLinkSource,
};
use ee::models::MemoryLinkId;
use serde_json::Value;
use uuid::Uuid;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-graph-determinism")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn unique_tmp_path(label: &str, extension: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(format!(
        "/tmp/ee-{label}-{}-{now}.{extension}",
        std::process::id()
    )))
}

fn assert_graph_determinism_shell_log(log_path: &Path) -> TestResult {
    let contents = fs::read_to_string(log_path)
        .map_err(|error| format!("failed to read {}: {error}", log_path.display()))?;
    let mut saw_schema = false;
    let mut saw_test_start = false;
    let mut saw_test_end = false;
    let mut saw_environment_note = false;
    let mut saw_assert_fail = false;
    let mut final_assert_fail_count = None;
    let mut assert_labels = Vec::new();

    for (line_index, line) in contents.lines().enumerate() {
        let event: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "{}:{}: malformed test event JSON: {error}\n{line}",
                log_path.display(),
                line_index + 1
            )
        })?;
        if event.get("schema").and_then(Value::as_str) == Some("ee.test_event.v1") {
            saw_schema = true;
        }
        if event.get("kind").and_then(Value::as_str) == Some("assert_fail") {
            saw_assert_fail = true;
        }
        let fields = event
            .get("fields")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if event.get("kind").and_then(Value::as_str) == Some("assert_ok") {
            if let Some(label) = fields.get("label").and_then(Value::as_str) {
                assert_labels.push(label.to_owned());
            }
        }
        if let Some(message) = fields.get("message").and_then(Value::as_str) {
            if message == "test_start: epic_F4_graph_determinism" {
                saw_test_start = true;
            }
            if message.starts_with("graph_determinism_environment ") {
                saw_environment_note = true;
            }
            if message == "test_end: epic_F4_graph_determinism" {
                saw_test_end = true;
                final_assert_fail_count = fields
                    .get("asserts_fail")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
    }

    if !saw_schema {
        return Err("graph determinism shell log did not emit ee.test_event.v1 events".to_owned());
    }
    if !saw_test_start || !saw_test_end {
        return Err("graph determinism shell log missing test_start or test_end note".to_owned());
    }
    if !saw_environment_note {
        return Err("graph determinism shell log missing environment note".to_owned());
    }
    if saw_assert_fail {
        return Err(
            "graph determinism shell log emitted at least one assert_fail event".to_owned(),
        );
    }
    if final_assert_fail_count.as_deref() != Some("0") {
        return Err(format!(
            "graph determinism shell reported non-zero assertion failures: {final_assert_fail_count:?}"
        ));
    }

    for required in [
        "graph_determinism_health_robot_insights",
        "graph_determinism_context_explain_performance",
        "graph_determinism_why_graph_badges",
        "graph_determinism_why_causal_explain",
        "graph_determinism_graph_centrality_refresh_dry_run",
        "graph_determinism_graph_feature_enrichment_dry_run",
        "graph_determinism_curate_candidates_read_only",
    ] {
        if !assert_labels.iter().any(|label| label == required) {
            return Err(format!(
                "graph determinism shell log missing required assert label {required}; labels={assert_labels:?}"
            ));
        }
    }

    Ok(())
}

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn seed_graph_workspace() -> Result<(PathBuf, Vec<String>), String> {
    let workspace = unique_workspace("determinism")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    if !init.status.success() {
        return Err(format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ));
    }
    fs::write(
        workspace.join(".ee").join("config.toml"),
        "[graph.feature.pack_dna]\nenabled = true\n",
    )
    .map_err(|error| error.to_string())?;

    let mut memory_ids = Vec::new();
    for i in 0..5 {
        let id = remember(
            &workspace_arg,
            &format!("Memory {i} for graph determinism test."),
        )?;
        memory_ids.push(id);
    }

    let db_path = workspace.join(".ee").join("ee.db");
    let connection =
        DbConnection::open(DatabaseConfig::file(&db_path)).map_err(|e| e.to_string())?;

    for i in 0..4 {
        let input = CreateMemoryLinkInput {
            src_memory_id: memory_ids[i].clone(),
            dst_memory_id: memory_ids[i + 1].clone(),
            relation: MemoryLinkRelation::Supports,
            weight: 0.8,
            confidence: 0.9,
            directed: false,
            evidence_count: 1,
            last_reinforced_at: None,
            source: MemoryLinkSource::Human,
            created_by: None,
            metadata_json: None,
        };
        let link_id = MemoryLinkId::from_uuid(Uuid::now_v7()).to_string();
        connection
            .insert_memory_link(&link_id, &input)
            .map_err(|e| e.to_string())?;
    }

    let input = CreateMemoryLinkInput {
        src_memory_id: memory_ids[0].clone(),
        dst_memory_id: memory_ids[4].clone(),
        relation: MemoryLinkRelation::Supports,
        weight: 0.7,
        confidence: 0.8,
        directed: false,
        evidence_count: 1,
        last_reinforced_at: None,
        source: MemoryLinkSource::Human,
        created_by: None,
        metadata_json: None,
    };
    let link_id = MemoryLinkId::from_uuid(Uuid::now_v7()).to_string();
    connection
        .insert_memory_link(&link_id, &input)
        .map_err(|e| e.to_string())?;

    Ok((workspace, memory_ids))
}

fn run_graph_command(workspace: &Path, subcommand: &str) -> Result<String, String> {
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?;
    let output = run_ee(&["--workspace", workspace_arg, "--json", "graph", subcommand])?;
    if !output.status.success() {
        return Err(format!(
            "graph {} failed: stderr={}",
            subcommand,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_context_pack_dna(workspace: &Path) -> Result<String, String> {
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?;
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "context",
        "--explain",
        "graph determinism memory",
    ])?;
    if !output.status.success() {
        return Err(format!(
            "context --explain failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "context --explain wrote stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("context --explain stdout must be JSON: {error}"))?;
    let pack_dna = parsed
        .pointer("/data/pack/packDna")
        .ok_or_else(|| "context --explain missing data.pack.packDna".to_string())?;
    if pack_dna["schema"] != Value::String("ee.context.pack_dna.v1".to_string()) {
        return Err(format!("unexpected packDna schema: {}", pack_dna["schema"]));
    }
    for field in [
        "snapshotVersion",
        "voronoiDominator",
        "communityOfMass",
        "egoSubgraph",
        "pprNeighbors",
        "degraded",
    ] {
        if pack_dna.get(field).is_none() {
            return Err(format!("packDna missing required field {field}"));
        }
    }
    for field in [
        "packMemoryCount",
        "querySeedCount",
        "trustAnchorCount",
        "dominator",
    ] {
        if pack_dna.get(field).is_some() {
            return Err(format!("packDna exposes implementation-only field {field}"));
        }
    }
    if !pack_dna["pprNeighbors"].is_array() {
        return Err("packDna.pprNeighbors must be an array".to_string());
    }
    if !pack_dna["degraded"].is_array() {
        return Err("packDna.degraded must be an array".to_string());
    }
    serde_json::to_string(pack_dna).map_err(|error| error.to_string())
}

struct CausalWhyFixture {
    workspace: PathBuf,
    failure_id: String,
    bridge_id: String,
    root_id: String,
}

fn seed_causal_why_workspace(causal_explain_enabled: bool) -> Result<CausalWhyFixture, String> {
    let workspace = unique_workspace("why-causal")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    if !init.status.success() {
        return Err(format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ));
    }
    if causal_explain_enabled {
        fs::write(
            workspace.join(".ee").join("config.toml"),
            "[graph.feature.causal_explain]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())?;
    }

    let failure_id = remember(&workspace_arg, "causal why determinism failure memory")?;
    let bridge_id = remember(&workspace_arg, "causal why determinism bridge memory")?;
    let root_id = remember(&workspace_arg, "causal why determinism root memory")?;
    let db_path = workspace.join(".ee").join("ee.db");
    let connection =
        DbConnection::open(DatabaseConfig::file(&db_path)).map_err(|e| e.to_string())?;
    let workspace_id = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "initialized workspace should be stored".to_string())?
        .id;

    for (edge_id, source_id, target_id, score, computed_at) in [
        (
            "cev_graph_determinism_why_bridge",
            failure_id.as_str(),
            bridge_id.as_str(),
            0.82,
            "2026-05-15T12:30:00Z",
        ),
        (
            "cev_graph_determinism_why_root",
            bridge_id.as_str(),
            root_id.as_str(),
            0.91,
            "2026-05-15T12:31:00Z",
        ),
    ] {
        connection
            .insert_causal_evidence(
                edge_id,
                &CreateCausalEvidenceInput {
                    workspace_id: workspace_id.clone(),
                    failure_id: source_id.to_string(),
                    candidate_cause_id: target_id.to_string(),
                    contribution_score: score,
                    evidence_uris: vec![format!("agent-mail://bd-qnfw.4/{edge_id}")],
                    computed_at: Some(computed_at.to_string()),
                    method: "manual".to_string(),
                },
            )
            .map_err(|error| error.to_string())?;
    }
    connection.close().map_err(|error| error.to_string())?;

    Ok(CausalWhyFixture {
        workspace,
        failure_id,
        bridge_id,
        root_id,
    })
}

fn run_why_causal_explain(workspace: &Path, memory_id: &str) -> Result<String, String> {
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?;
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "why",
        memory_id,
        "--causal-explain",
    ])?;
    if !output.status.success() {
        return Err(format!(
            "why --causal-explain failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "why --causal-explain wrote stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn graph_determinism_shell_driver_emits_structured_log() -> TestResult {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = repo_root.join("scripts/e2e_overhaul/graph_determinism.sh");
    let log_path = unique_tmp_path("graph-determinism", "jsonl")?;
    let retention_manifest = unique_tmp_path("graph-determinism-retention", "json")?;
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(repo_root)
        .env("EE_BINARY", env!("CARGO_BIN_EXE_ee"))
        .env("EE_TEST_LOG_PATH", &log_path)
        .env("EE_TEST_LOG_LEVEL", "normal")
        .env("EE_TEST_EXECUTION_SUBSTRATE", "cargo_integration_test")
        .env("EE_E2E_KEEP_WORKSPACE", "1")
        .env("EE_E2E_KEEP_ARTIFACTS", "1")
        .env("EE_E2E_TMPDIR", "/tmp")
        .env("EE_E2E_ARTIFACT_TMPDIR", "/tmp")
        .env("EE_E2E_RETENTION_MANIFEST", &retention_manifest)
        .env("EE_GRAPH_DETERMINISM_MAX_SECONDS", "600")
        .env("EE_GRAPH_DETERMINISM_TIMEOUT_SECONDS", "60")
        .env("TMPDIR", "/tmp")
        .output()
        .map_err(|error| format!("failed to run {}: {error}", script.display()))?;
    if !output.status.success() {
        let log_contents = fs::read_to_string(&log_path)
            .unwrap_or_else(|error| format!("failed to read {}: {error}", log_path.display()));
        return Err(format!(
            "graph determinism shell driver exited {:?}\nlog_path: {}\nretention_manifest: {}\nlog:\n{}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            log_path.display(),
            retention_manifest.display(),
            log_contents,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    assert_graph_determinism_shell_log(&log_path)
}

#[test]
fn graph_snapshot_refresh_accepts_each_graph_target() -> TestResult {
    let workspace = unique_workspace("snapshot-refresh")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    if !init.status.success() {
        return Err(format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ));
    }

    let cases = [
        ("memory_links", vec!["memory_links"]),
        ("causal", vec!["causal_evidence"]),
        ("revision", vec!["revision_dag"]),
        ("rules", vec!["rule_provenance"]),
        ("contradictions", vec!["contradiction_subgraph"]),
        (
            "all",
            vec![
                "memory_links",
                "causal_evidence",
                "revision_dag",
                "rule_provenance",
                "contradiction_subgraph",
            ],
        ),
    ];

    for (input, expected_graph_types) in cases {
        let output = run_ee(&[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "graph",
            "snapshot",
            "refresh",
            "--graph",
            input,
            "--dry-run",
        ])?;
        if !output.status.success() {
            return Err(format!(
                "graph snapshot refresh --graph {input} failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if !output.stderr.is_empty() {
            return Err(format!(
                "graph snapshot refresh --graph {input} wrote stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let parsed: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("snapshot refresh stdout must be JSON: {error}"))?;
        if parsed["schema"] != Value::String("ee.graph.centrality_refresh.v1".to_string()) {
            return Err(format!(
                "unexpected schema for {input}: {}",
                parsed["schema"]
            ));
        }
        if parsed["success"] != Value::Bool(true) {
            return Err(format!("success must be true for {input}"));
        }
        let reports = parsed["data"]["reports"]
            .as_array()
            .ok_or_else(|| format!("reports array missing for {input}"))?;
        let actual_graph_types: Vec<&str> = reports
            .iter()
            .filter_map(|report| report["graphType"].as_str())
            .collect();
        if actual_graph_types != expected_graph_types {
            return Err(format!(
                "graph target expansion mismatch for {input}: actual={actual_graph_types:?} expected={expected_graph_types:?}"
            ));
        }
        for report in reports {
            if report["dryRun"] != Value::Bool(true) {
                return Err(format!("report for {input} must be dryRun=true"));
            }
            if report["status"] != Value::String("dry_run".to_string()) {
                return Err(format!("report for {input} must have dry_run status"));
            }
        }
    }

    Ok(())
}

#[test]
fn graph_snapshot_refresh_causal_persists_stable_snapshot_row() -> TestResult {
    let workspace = unique_workspace("snapshot-refresh-causal")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    if !init.status.success() {
        return Err(format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ));
    }

    let failure_id = remember(&workspace_arg, "causal snapshot failure memory")?;
    let cause_id = remember(&workspace_arg, "causal snapshot candidate cause")?;
    let db_path = workspace.join(".ee").join("ee.db");
    let connection =
        DbConnection::open(DatabaseConfig::file(&db_path)).map_err(|e| e.to_string())?;
    let workspace_id = connection
        .list_workspaces()
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "initialized workspace should be stored".to_string())?
        .id;
    connection
        .insert_causal_evidence(
            "cev_graph_determinism_001",
            &CreateCausalEvidenceInput {
                workspace_id: workspace_id.clone(),
                failure_id,
                candidate_cause_id: cause_id,
                contribution_score: 0.8,
                evidence_uris: vec!["agent-mail://graph-determinism/causal".to_string()],
                computed_at: Some("2026-05-14T05:00:01Z".to_string()),
                method: "manual".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let mut content_hashes = Vec::new();
    for expected_version in 1..=3 {
        let output = run_ee(&[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "graph",
            "snapshot",
            "refresh",
            "--graph",
            "causal",
        ])?;
        if !output.status.success() {
            return Err(format!(
                "graph snapshot refresh --graph causal failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if !output.stderr.is_empty() {
            return Err(format!(
                "graph snapshot refresh --graph causal wrote stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let parsed: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("snapshot refresh stdout must be JSON: {error}"))?;
        let reports = parsed["data"]["reports"]
            .as_array()
            .ok_or_else(|| "causal refresh reports array missing".to_string())?;
        if reports.len() != 1 {
            return Err(format!(
                "causal refresh should emit one report, got {}",
                reports.len()
            ));
        }
        let report = &reports[0];
        if report["graphType"] != Value::String("causal_evidence".to_string()) {
            return Err(format!(
                "causal refresh graphType mismatch: {}",
                report["graphType"]
            ));
        }
        if report["status"] != Value::String("refreshed".to_string()) {
            return Err(format!(
                "causal refresh status mismatch: {}",
                report["status"]
            ));
        }
        if report["graph"]["nodeCount"] != 2 {
            return Err(format!(
                "causal refresh node count mismatch: {}",
                report["graph"]["nodeCount"]
            ));
        }
        if report["graph"]["edgeCount"] != 1 {
            return Err(format!(
                "causal refresh edge count mismatch: {}",
                report["graph"]["edgeCount"]
            ));
        }
        let snapshot = &report["snapshot"];
        if snapshot["snapshotVersion"] != expected_version {
            return Err(format!(
                "causal snapshot version mismatch for run {expected_version}: {}",
                snapshot["snapshotVersion"]
            ));
        }
        let content_hash = snapshot["contentHash"]
            .as_str()
            .ok_or_else(|| format!("causal snapshot missing contentHash: {snapshot}"))?;
        if !content_hash.starts_with("blake3:") {
            return Err(format!(
                "causal content hash must be blake3: {content_hash}"
            ));
        }
        content_hashes.push(content_hash.to_string());
    }

    if content_hashes[0] != content_hashes[1] || content_hashes[1] != content_hashes[2] {
        return Err(format!(
            "causal typed snapshot hash changed across unchanged refreshes: {content_hashes:?}"
        ));
    }

    let connection =
        DbConnection::open(DatabaseConfig::file(&db_path)).map_err(|e| e.to_string())?;
    let latest = connection
        .get_latest_graph_snapshot(&workspace_id, GraphSnapshotType::CausalEvidence)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "expected persisted causal_evidence graph snapshot".to_string())?;
    if latest.snapshot_version != 3 {
        return Err(format!(
            "latest causal snapshot version should be 3, got {}",
            latest.snapshot_version
        ));
    }
    if latest.status != GraphSnapshotStatus::Valid {
        return Err(format!(
            "latest causal snapshot status should be valid, got {:?}",
            latest.status
        ));
    }
    if latest.node_count != 2 || latest.edge_count != 1 {
        return Err(format!(
            "latest causal snapshot topology mismatch: nodes={} edges={}",
            latest.node_count, latest.edge_count
        ));
    }
    if latest.content_hash != content_hashes[0] {
        return Err(format!(
            "latest causal snapshot hash {} did not match CLI hash {}",
            latest.content_hash, content_hashes[0]
        ));
    }
    connection.close().map_err(|error| error.to_string())
}

#[test]
fn why_causal_explain_output_is_deterministic() -> TestResult {
    let fixture = seed_causal_why_workspace(true)?;

    let first = run_why_causal_explain(&fixture.workspace, &fixture.failure_id)?;
    let second = run_why_causal_explain(&fixture.workspace, &fixture.failure_id)?;
    let third = run_why_causal_explain(&fixture.workspace, &fixture.failure_id)?;

    if first != second {
        return Err(format!(
            "why --causal-explain output differs between runs:\nfirst={first}\nsecond={second}"
        ));
    }
    if second != third {
        return Err(format!(
            "why --causal-explain output differs between runs:\nsecond={second}\nthird={third}"
        ));
    }

    let parsed: Value = serde_json::from_str(&first).map_err(|e| e.to_string())?;
    let causal = parsed
        .pointer("/data/causalExplanation")
        .ok_or_else(|| "why output missing data.causalExplanation".to_string())?;
    if causal["schema"] != Value::String("ee.why.causal.v1".to_string()) {
        return Err(format!(
            "unexpected causal explanation schema: {}",
            causal["schema"]
        ));
    }
    if causal["memoryId"] != Value::String(fixture.failure_id.clone()) {
        return Err(format!(
            "causal explanation memoryId mismatch: {}",
            causal["memoryId"]
        ));
    }
    let paths = causal["paths"]
        .as_array()
        .ok_or_else(|| "causalExplanation.paths must be an array".to_string())?;
    if paths.len() != 1 {
        return Err(format!("expected one causal path, got {}", paths.len()));
    }
    if paths[0]["edgeCount"] != 2 {
        return Err(format!(
            "expected two causal path edges, got {}",
            paths[0]["edgeCount"]
        ));
    }
    if paths[0]["sourceMemoryId"] != Value::String(fixture.root_id.clone()) {
        return Err(format!(
            "expected root cause as path source, got {}",
            paths[0]["sourceMemoryId"]
        ));
    }
    if paths[0]["targetMemoryId"] != Value::String(fixture.failure_id.clone()) {
        return Err(format!(
            "expected failure memory as path target, got {}",
            paths[0]["targetMemoryId"]
        ));
    }
    let total_contribution = paths[0]["totalContribution"]
        .as_f64()
        .ok_or_else(|| "causal path totalContribution must be numeric".to_string())?;
    if (total_contribution - 1.73).abs() > 0.000_001 {
        return Err(format!(
            "expected total contribution 1.73, got {total_contribution}"
        ));
    }
    let min_cost = paths[0]["minCost"]
        .as_f64()
        .ok_or_else(|| "causal path minCost must be numeric".to_string())?;
    if (min_cost - 0.27).abs() > 0.000_001 {
        return Err(format!("expected min-cost path cost 0.27, got {min_cost}"));
    }
    let steps = paths[0]["steps"]
        .as_array()
        .ok_or_else(|| "causal path steps must be an array".to_string())?;
    if steps.len() != 2 {
        return Err(format!(
            "expected two causal path steps, got {}",
            steps.len()
        ));
    }
    let expected_steps = [
        (
            fixture.failure_id.as_str(),
            fixture.bridge_id.as_str(),
            "cev_graph_determinism_why_bridge",
            0.82,
            0.18,
        ),
        (
            fixture.bridge_id.as_str(),
            fixture.root_id.as_str(),
            "cev_graph_determinism_why_root",
            0.91,
            0.09,
        ),
    ];
    for (index, (source, target, edge_id, contribution, cost)) in expected_steps.iter().enumerate()
    {
        let step = &steps[index];
        if step["source"] != Value::String((*source).to_string())
            || step["target"] != Value::String((*target).to_string())
            || step["edgeId"] != Value::String((*edge_id).to_string())
        {
            return Err(format!(
                "causal path step {index} mismatch: expected source={source} target={target} edge={edge_id}, got {step}"
            ));
        }
        let actual_contribution = step["contributionScore"]
            .as_f64()
            .ok_or_else(|| format!("causal path step {index} contributionScore must be numeric"))?;
        if (actual_contribution - contribution).abs() > 0.000_001 {
            return Err(format!(
                "causal path step {index} contribution mismatch: expected {contribution}, got {actual_contribution}"
            ));
        }
        let actual_cost = step["cost"]
            .as_f64()
            .ok_or_else(|| format!("causal path step {index} cost must be numeric"))?;
        if (actual_cost - cost).abs() > 0.000_001 {
            return Err(format!(
                "causal path step {index} cost mismatch: expected {cost}, got {actual_cost}"
            ));
        }
    }
    if !causal["degraded"]
        .as_array()
        .ok_or_else(|| "causalExplanation.degraded must be an array".to_string())?
        .is_empty()
    {
        return Err(format!(
            "causal explanation should not be degraded: {}",
            causal["degraded"]
        ));
    }

    Ok(())
}

#[test]
fn why_causal_explain_disabled_by_feature_flag() -> TestResult {
    let fixture = seed_causal_why_workspace(false)?;

    let output = run_why_causal_explain(&fixture.workspace, &fixture.failure_id)?;
    let parsed: Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
    let causal = parsed
        .pointer("/data/causalExplanation")
        .ok_or_else(|| "why output missing data.causalExplanation".to_string())?;
    if causal["schema"] != Value::String("ee.why.causal.v1".to_string()) {
        return Err(format!(
            "unexpected disabled causal explanation schema: {}",
            causal["schema"]
        ));
    }
    if causal["memoryId"] != Value::String(fixture.failure_id.clone()) {
        return Err(format!(
            "disabled causal explanation memoryId mismatch: {}",
            causal["memoryId"]
        ));
    }
    let paths = causal["paths"]
        .as_array()
        .ok_or_else(|| "disabled causalExplanation.paths must be an array".to_string())?;
    if !paths.is_empty() {
        return Err(format!(
            "disabled causal explanation must not expose stale paths: {}",
            causal["paths"]
        ));
    }
    let degraded = causal["degraded"]
        .as_array()
        .ok_or_else(|| "disabled causalExplanation.degraded must be an array".to_string())?;
    if degraded.len() != 1 {
        return Err(format!(
            "disabled causal explanation should emit one degraded entry, got {}",
            degraded.len()
        ));
    }
    if degraded[0]["code"] != Value::String("graph_feature_disabled".to_string()) {
        return Err(format!(
            "disabled causal explanation code mismatch: {}",
            degraded[0]["code"]
        ));
    }
    if degraded[0]["severity"] != Value::String("medium".to_string()) {
        return Err(format!(
            "disabled causal explanation severity mismatch: {}",
            degraded[0]["severity"]
        ));
    }
    if degraded[0]["message"]
        != Value::String(format!(
            "Causal explanation is disabled by {GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY}."
        ))
    {
        return Err(format!(
            "disabled causal explanation message mismatch: {}",
            degraded[0]["message"]
        ));
    }
    if degraded[0]["repair"]
        != Value::String(format!(
            "ee config set {GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY} true"
        ))
    {
        return Err(format!(
            "disabled causal explanation repair mismatch: {}",
            degraded[0]["repair"]
        ));
    }

    Ok(())
}

#[test]
fn context_pack_dna_output_is_deterministic() -> TestResult {
    let (workspace, _) = seed_graph_workspace()?;

    let first = run_context_pack_dna(&workspace)?;
    let second = run_context_pack_dna(&workspace)?;
    let third = run_context_pack_dna(&workspace)?;

    if first != second {
        return Err(format!(
            "context packDna output differs between runs:\nfirst={first}\nsecond={second}"
        ));
    }
    if second != third {
        return Err(format!(
            "context packDna output differs between runs:\nsecond={second}\nthird={third}"
        ));
    }

    Ok(())
}

#[test]
fn graph_communities_output_is_deterministic() -> TestResult {
    let (workspace, _) = seed_graph_workspace()?;

    let first = run_graph_command(&workspace, "communities")?;
    let second = run_graph_command(&workspace, "communities")?;
    let third = run_graph_command(&workspace, "communities")?;

    if first != second {
        return Err(format!(
            "graph communities output differs between runs:\nfirst={first}\nsecond={second}"
        ));
    }
    if second != third {
        return Err(format!(
            "graph communities output differs between runs:\nsecond={second}\nthird={third}"
        ));
    }

    let parsed: Value = serde_json::from_str(&first).map_err(|e| e.to_string())?;
    let communities = parsed["data"]["communities"]
        .as_array()
        .ok_or_else(|| "communities field missing".to_string())?;

    for community in communities {
        let nodes = community["nodes"]
            .as_array()
            .ok_or_else(|| "nodes field missing".to_string())?;
        let node_strs: Vec<&str> = nodes.iter().filter_map(Value::as_str).collect();
        let mut sorted = node_strs.clone();
        sorted.sort();
        if node_strs != sorted {
            return Err(format!(
                "nodes within community are not sorted: {node_strs:?}"
            ));
        }
    }

    Ok(())
}

#[test]
fn graph_louvain_output_is_deterministic() -> TestResult {
    let (workspace, _) = seed_graph_workspace()?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?;

    let run = || -> Result<String, String> {
        let output = run_ee(&[
            "--workspace",
            workspace_arg,
            "--json",
            "graph",
            "louvain",
            "--seed",
            "42",
        ])?;
        if !output.status.success() {
            return Err(format!(
                "graph louvain failed: stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };

    let first = run()?;
    let second = run()?;
    let third = run()?;

    if first != second {
        return Err(format!(
            "graph louvain output differs between runs:\nfirst={first}\nsecond={second}"
        ));
    }
    if second != third {
        return Err(format!(
            "graph louvain output differs between runs:\nsecond={second}\nthird={third}"
        ));
    }

    Ok(())
}

#[test]
fn graph_articulation_output_is_deterministic() -> TestResult {
    let (workspace, _) = seed_graph_workspace()?;

    let first = run_graph_command(&workspace, "articulation")?;
    let second = run_graph_command(&workspace, "articulation")?;
    let third = run_graph_command(&workspace, "articulation")?;

    if first != second {
        return Err(format!(
            "graph articulation output differs between runs:\nfirst={first}\nsecond={second}"
        ));
    }
    if second != third {
        return Err(format!(
            "graph articulation output differs between runs:\nsecond={second}\nthird={third}"
        ));
    }

    let parsed: Value = serde_json::from_str(&first).map_err(|e| e.to_string())?;
    if let Some(nodes) = parsed["data"]["articulationPoints"].as_array() {
        let node_strs: Vec<&str> = nodes.iter().filter_map(Value::as_str).collect();
        let mut sorted = node_strs.clone();
        sorted.sort();
        if node_strs != sorted {
            return Err(format!("articulation points are not sorted: {node_strs:?}"));
        }
    }

    Ok(())
}

#[test]
fn graph_k_core_output_is_deterministic() -> TestResult {
    let (workspace, _) = seed_graph_workspace()?;

    let first = run_graph_command(&workspace, "k-core")?;
    let second = run_graph_command(&workspace, "k-core")?;
    let third = run_graph_command(&workspace, "k-core")?;

    if first != second {
        return Err(format!(
            "graph k-core output differs between runs:\nfirst={first}\nsecond={second}"
        ));
    }
    if second != third {
        return Err(format!(
            "graph k-core output differs between runs:\nsecond={second}\nthird={third}"
        ));
    }

    let parsed: Value = serde_json::from_str(&first).map_err(|e| e.to_string())?;
    if let Some(nodes) = parsed["data"]["nodes"].as_array() {
        let node_strs: Vec<&str> = nodes.iter().filter_map(Value::as_str).collect();
        let mut sorted = node_strs.clone();
        sorted.sort();
        if node_strs != sorted {
            return Err(format!("k-core nodes are not sorted: {node_strs:?}"));
        }
    }

    Ok(())
}
