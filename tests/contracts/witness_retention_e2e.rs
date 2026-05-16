//! Contract coverage for the graph witness retention maintenance command.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use chrono::{Duration, Utc};
use ee::db::{
    CreateGraphAlgorithmWitnessInput, CreateGraphSnapshotInput, CreateWorkspaceInput, DbConnection,
    GraphSnapshotStatus, GraphSnapshotType,
};
use serde_json::Value;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "{context} should succeed; stdout={stdout}; stderr={stderr}"
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!("{context} stderr should be empty; got {stderr}"));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{context} stdout should be JSON: {error}; stdout={stdout}"))
}

fn ensure_json_eq(actual: &Value, expected: Value, context: &str) -> TestResult {
    if actual == &expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected}, got {actual}"))
    }
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn sql_quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn set_witness_recorded_at(
    conn: &DbConnection,
    workspace_id: &str,
    snapshot_id: &str,
    algorithm: &str,
    recorded_at: &str,
) -> TestResult {
    conn.execute_raw(&format!(
        "UPDATE graph_algorithm_witnesses \
         SET recorded_at = '{}' \
         WHERE workspace_id = '{}' AND snapshot_id = '{}' AND algorithm = '{}'",
        sql_quote(recorded_at),
        sql_quote(workspace_id),
        sql_quote(snapshot_id),
        sql_quote(algorithm),
    ))
    .map_err(|error| format!("backdate {algorithm} witness: {error}"))
}

fn insert_snapshot(
    conn: &DbConnection,
    workspace_id: &str,
    snapshot_id: &str,
    version: u32,
    status: GraphSnapshotStatus,
) -> TestResult {
    conn.insert_graph_snapshot(
        snapshot_id,
        &CreateGraphSnapshotInput {
            workspace_id: workspace_id.to_string(),
            snapshot_version: version,
            schema_version: "ee.graph.snapshot.v1".to_string(),
            graph_type: GraphSnapshotType::MemoryLinks,
            node_count: 3,
            edge_count: 2,
            metrics_json: r#"{"nodes":[],"edges":[]}"#.to_string(),
            content_hash: format!("blake3:witness-retention-{version}"),
            source_generation: version,
            expires_at: None,
        },
    )
    .map_err(|error| format!("insert graph snapshot {snapshot_id}: {error}"))?;
    if status != GraphSnapshotStatus::Valid {
        conn.update_graph_snapshot_status(snapshot_id, status)
            .map_err(|error| format!("update snapshot status {snapshot_id}: {error}"))?;
    }
    Ok(())
}

fn insert_witness(
    conn: &DbConnection,
    workspace_id: &str,
    snapshot_id: &str,
    algorithm: &str,
    recorded_at: &str,
) -> TestResult {
    conn.insert_graph_algorithm_witness(&CreateGraphAlgorithmWitnessInput {
        workspace_id: workspace_id.to_string(),
        snapshot_id: snapshot_id.to_string(),
        algorithm: algorithm.to_string(),
        params_json: format!(r#"{{"algorithm":"{algorithm}"}}"#),
        witness_json: format!(
            r#"{{"elapsed_ms":11,"sampling_choice":"exact","decision_path_hash":"blake3:{algorithm}"}}"#
        ),
    })
    .map_err(|error| format!("insert witness {algorithm}: {error}"))?;
    set_witness_recorded_at(conn, workspace_id, snapshot_id, algorithm, recorded_at)
}

fn seed_witness_fixture(workspace: &Path) -> Result<(PathBuf, String), String> {
    let ee_dir = workspace.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee dir: {error}"))?;
    let database_path = ee_dir.join("ee.db");
    let conn = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    conn.migrate().map_err(|error| error.to_string())?;

    let workspace_path = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?
        .to_string_lossy()
        .into_owned();
    let workspace_id = "wsp_0123456789abcdef0123456789".to_string();
    conn.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: workspace_path,
            name: Some("witness-retention-e2e".to_string()),
        },
    )
    .map_err(|error| format!("insert workspace: {error}"))?;

    let active_snapshot = "gsnap_activewitnessretention001";
    let archived_old_snapshot = "gsnap_archivedoldwitnessret001";
    let archived_recent_snapshot = "gsnap_archivednewwitnessret001";
    insert_snapshot(
        &conn,
        &workspace_id,
        active_snapshot,
        1,
        GraphSnapshotStatus::Valid,
    )?;
    insert_snapshot(
        &conn,
        &workspace_id,
        archived_old_snapshot,
        2,
        GraphSnapshotStatus::Archived,
    )?;
    insert_snapshot(
        &conn,
        &workspace_id,
        archived_recent_snapshot,
        3,
        GraphSnapshotStatus::Archived,
    )?;

    let now = Utc::now();
    let expired_recorded_at = (now - Duration::days(30) - Duration::hours(1)).to_rfc3339();
    let active_recorded_at = (now - Duration::days(60)).to_rfc3339();
    let same_day_recorded_at = now.to_rfc3339();
    insert_witness(
        &conn,
        &workspace_id,
        archived_old_snapshot,
        "expired_pagerank",
        &expired_recorded_at,
    )?;
    insert_witness(
        &conn,
        &workspace_id,
        active_snapshot,
        "active_pagerank",
        &active_recorded_at,
    )?;
    insert_witness(
        &conn,
        &workspace_id,
        archived_recent_snapshot,
        "same_day_hits",
        &same_day_recorded_at,
    )?;

    conn.close().map_err(|error| error.to_string())?;
    Ok((database_path, workspace_id))
}

fn classification_for<'a>(data: &'a Value, algorithm: &str) -> Result<&'a Value, String> {
    data.pointer("/report/classifications")
        .and_then(Value::as_array)
        .ok_or_else(|| "classifications array missing".to_string())?
        .iter()
        .find(|classification| {
            classification.get("algorithm").and_then(Value::as_str) == Some(algorithm)
        })
        .ok_or_else(|| format!("classification for {algorithm} missing"))
}

#[test]
fn graph_witnesses_prune_preserves_active_and_fresh_rows() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?;
    let (database_path, workspace_id) = seed_witness_fixture(workspace)?;

    let dry_run_output = run_ee(&[
        "--workspace",
        workspace_arg,
        "maintenance",
        "graph-witnesses-prune",
        "--dry-run",
        "--json",
    ])?;
    let dry_run_json = stdout_json(&dry_run_output, "graph witnesses prune dry run")?;
    let dry_run_data = dry_run_json
        .get("data")
        .ok_or_else(|| "dry-run response missing data".to_string())?;

    ensure_json_eq(
        &dry_run_json["schema"],
        Value::String("ee.response.v1".to_string()),
        "dry-run envelope schema",
    )?;
    ensure_json_eq(
        &dry_run_json["success"],
        Value::Bool(true),
        "dry-run success",
    )?;
    ensure_json_eq(
        &dry_run_data["schema"],
        Value::String("ee.graph.witness_prune_report.v1".to_string()),
        "dry-run data schema",
    )?;
    ensure_json_eq(
        &dry_run_data["command"],
        Value::String("maintenance graph-witnesses-prune".to_string()),
        "dry-run command",
    )?;
    ensure_json_eq(
        &dry_run_data["workspaceId"],
        Value::String(workspace_id.clone()),
        "dry-run workspace id",
    )?;
    ensure_json_eq(
        &dry_run_data["summary"]["totalCount"],
        Value::from(3),
        "dry-run total count",
    )?;
    ensure_json_eq(
        &dry_run_data["summary"]["pruneCount"],
        Value::from(1),
        "dry-run prune count",
    )?;
    ensure_json_eq(
        &dry_run_data["summary"]["deletedCount"],
        Value::from(0),
        "dry-run deleted count",
    )?;
    ensure_json_eq(
        &dry_run_data["summary"]["keepActiveSnapshotCount"],
        Value::from(1),
        "dry-run active snapshot keep count",
    )?;
    ensure_json_eq(
        &dry_run_data["summary"]["keepWithinTtlCount"],
        Value::from(1),
        "dry-run within-TTL keep count",
    )?;

    let expired = classification_for(dry_run_data, "expired_pagerank")?;
    ensure_json_eq(
        &expired["action"]["kind"],
        Value::String("prune".to_string()),
        "expired witness action",
    )?;
    ensure(
        expired["action"]["ageDays"]
            .as_u64()
            .is_some_and(|age| age >= 30),
        "expired witness should be at least thirty days old",
    )?;
    ensure_json_eq(
        &expired["action"]["ttlDays"],
        Value::from(30),
        "expired witness TTL",
    )?;

    let active = classification_for(dry_run_data, "active_pagerank")?;
    ensure_json_eq(
        &active["action"]["kind"],
        Value::String("keep".to_string()),
        "active witness action",
    )?;
    ensure_json_eq(
        &active["action"]["reason"]["code"],
        Value::String("active_snapshot".to_string()),
        "active witness keep reason",
    )?;

    let same_day = classification_for(dry_run_data, "same_day_hits")?;
    ensure_json_eq(
        &same_day["action"]["kind"],
        Value::String("keep".to_string()),
        "same-day witness action",
    )?;
    ensure_json_eq(
        &same_day["action"]["reason"]["code"],
        Value::String("within_ttl".to_string()),
        "same-day witness keep reason",
    )?;

    let apply_output = run_ee(&[
        "--workspace",
        workspace_arg,
        "maintenance",
        "graph-witnesses-prune",
        "--json",
    ])?;
    let apply_json = stdout_json(&apply_output, "graph witnesses prune apply")?;
    let apply_data = apply_json
        .get("data")
        .ok_or_else(|| "apply response missing data".to_string())?;
    ensure_json_eq(
        &apply_data["summary"]["deletedCount"],
        Value::from(1),
        "apply deleted count",
    )?;
    ensure_json_eq(
        &apply_data["durableMutation"],
        Value::Bool(true),
        "apply durable mutation",
    )?;

    let conn = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let remaining = conn
        .list_graph_algorithm_witnesses_with_snapshot_active(&workspace_id)
        .map_err(|error| format!("list remaining witnesses: {error}"))?;
    let remaining_algorithms = remaining
        .iter()
        .map(|(witness, _)| witness.algorithm.as_str())
        .collect::<Vec<_>>();
    ensure(
        !remaining_algorithms.contains(&"expired_pagerank"),
        "expired witness should be deleted",
    )?;
    ensure(
        remaining_algorithms.contains(&"active_pagerank"),
        "active snapshot witness should remain",
    )?;
    ensure(
        remaining_algorithms.contains(&"same_day_hits"),
        "same-day witness should remain",
    )?;
    conn.close().map_err(|error| error.to_string())?;
    Ok(())
}
