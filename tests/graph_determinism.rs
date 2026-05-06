//! Determinism tests for graph CLI commands.
//!
//! Graph commands must produce identical JSON output when invoked multiple times
//! on the same database. This verifies that node lists, community memberships,
//! and edge lists are sorted deterministically.

#![cfg(unix)]
#![cfg(feature = "graph")]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::db::{CreateMemoryLinkInput, DatabaseConfig, DbConnection, MemoryLinkRelation, MemoryLinkSource};
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

    let mut memory_ids = Vec::new();
    for i in 0..5 {
        let id = remember(&workspace_arg, &format!("Memory {i} for graph determinism test."))?;
        memory_ids.push(id);
    }

    let db_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open(DatabaseConfig::file(&db_path)).map_err(|e| e.to_string())?;

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

fn run_graph_command(workspace: &PathBuf, subcommand: &str) -> Result<String, String> {
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?;
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        subcommand,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "graph {} failed: stderr={}",
            subcommand,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
        let node_strs: Vec<&str> = nodes
            .iter()
            .filter_map(Value::as_str)
            .collect();
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
        let node_strs: Vec<&str> = nodes
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut sorted = node_strs.clone();
        sorted.sort();
        if node_strs != sorted {
            return Err(format!(
                "articulation points are not sorted: {node_strs:?}"
            ));
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
        let node_strs: Vec<&str> = nodes
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut sorted = node_strs.clone();
        sorted.sort();
        if node_strs != sorted {
            return Err(format!("k-core nodes are not sorted: {node_strs:?}"));
        }
    }

    Ok(())
}
