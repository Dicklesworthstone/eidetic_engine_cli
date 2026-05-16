//! bd-3omr5: explicit mesh command modes stay local-first until cached or
//! revisable mesh material is implemented.
//!
//! This test exercises the real `ee` binary because the contract is for agent
//! command surfaces, not just Clap parsing. It deliberately keeps its temporary
//! workspace on disk; AGENTS.md forbids agent-side file deletion.

use std::collections::BTreeSet;
use std::fs;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

const MODES: [&str; 3] = ["off", "cache", "revisable"];

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
        "{}/ee-mesh-command-modes-{}-{nanos}",
        root.trim_end_matches('/'),
        std::process::id()
    );
    fs::create_dir_all(&path)
        .map_err(|error| format!("failed to create retained workspace {path}: {error}"))?;
    Ok(path)
}

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env("EE_MESH_ENABLED", "0")
        .env_remove("EE_MESH_MODE")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
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

fn collect_codes(value: &Value, codes: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(code) = map.get("code").and_then(Value::as_str) {
                codes.push(code.to_owned());
            }
            for child in map.values() {
                collect_codes(child, codes);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_codes(child, codes);
            }
        }
        _ => {}
    }
}

fn assert_no_mesh_degradation(json: &Value, label: &str) -> TestResult {
    let mut codes = Vec::new();
    collect_codes(json, &mut codes);
    let mesh_codes: Vec<String> = codes
        .into_iter()
        .filter(|code| {
            let lowered = code.to_ascii_lowercase();
            lowered.contains("mesh") || lowered.contains("tailscale") || lowered.contains("peer")
        })
        .collect();
    if mesh_codes.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{label}: unexpected mesh degraded code(s): {mesh_codes:?}"
        ))
    }
}

fn search_doc_ids(json: &Value) -> Result<Vec<String>, String> {
    let results = json
        .pointer("/data/results")
        .and_then(Value::as_array)
        .ok_or_else(|| "search output missing /data/results array".to_string())?;
    Ok(results
        .iter()
        .filter_map(|item| item.get("docId").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}

fn lsof_program() -> Option<&'static str> {
    if Command::new("sh")
        .args(["-c", "command -v lsof >/dev/null 2>&1"])
        .status()
        .ok()
        .is_some_and(|status| status.success())
    {
        Some("lsof")
    } else if std::path::Path::new("/usr/sbin/lsof").is_file() {
        Some("/usr/sbin/lsof")
    } else {
        None
    }
}

fn listener_snapshot() -> Result<Option<BTreeSet<String>>, String> {
    let Some(program) = lsof_program() else {
        return Ok(None);
    };
    let output = Command::new(program)
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output()
        .map_err(|error| format!("failed to run {program}: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{program} stdout was not UTF-8: {error}"))?;
    let listeners = stdout
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let command = fields.next()?;
            let pid = fields.next()?;
            let address = line.split_whitespace().last()?;
            Some(format!("{command} {pid} {address}"))
        })
        .collect();
    Ok(Some(listeners))
}

fn assert_no_new_mesh_listener(
    before: &Option<BTreeSet<String>>,
    after: &Option<BTreeSet<String>>,
) -> TestResult {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(());
    };
    let new_mesh: Vec<String> = after
        .difference(before)
        .filter(|line| {
            let lowered = line.to_ascii_lowercase();
            lowered.contains("ee")
                || lowered.contains("eidetic")
                || lowered.contains("mesh")
                || lowered.contains("tailscale")
        })
        .cloned()
        .collect();
    if new_mesh.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "mesh command modes left unexpected listener(s): {new_mesh:?}"
        ))
    }
}

#[test]
fn mesh_command_modes_preserve_local_first_results_without_network() -> TestResult {
    let workspace = workspace_dir()?;

    let init = run_ee(&workspace, &["init", "--json"])?;
    ensure_success(&init, "init")?;

    let remember = run_ee(
        &workspace,
        &[
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "Mesh command-mode e2e fixture memory.",
            "--json",
        ],
    )?;
    ensure_success(&remember, "remember")?;
    let remember_json = stdout_json(&remember, "remember")?;
    let memory_id = remember_json
        .pointer("/data/memory_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "remember output missing /data/memory_id".to_string())?;

    let before = listener_snapshot()?;
    let mut off_search_ids = Vec::new();

    for mode in MODES {
        let search = run_ee(
            &workspace,
            &[
                "search",
                "mesh command-mode e2e fixture",
                "--mesh",
                mode,
                "--json",
            ],
        )?;
        ensure_success(&search, &format!("search --mesh {mode}"))?;
        let search_json = stdout_json(&search, &format!("search --mesh {mode}"))?;
        assert_no_mesh_degradation(&search_json, &format!("search --mesh {mode}"))?;
        let ids = search_doc_ids(&search_json)?;
        if mode == "off" {
            off_search_ids = ids;
        } else if ids != off_search_ids {
            return Err(format!(
                "search --mesh {mode} returned different local doc ids: {ids:?} != {off_search_ids:?}"
            ));
        }

        for (label, args) in [
            (
                "context",
                vec![
                    "context",
                    "mesh command-mode e2e fixture",
                    "--max-tokens",
                    "500",
                    "--mesh",
                    mode,
                    "--json",
                ],
            ),
            (
                "pack",
                vec![
                    "pack",
                    "mesh command-mode e2e fixture",
                    "--max-tokens",
                    "500",
                    "--mesh",
                    mode,
                    "--json",
                ],
            ),
            ("why", vec!["why", memory_id, "--mesh", mode, "--json"]),
            ("status", vec!["status", "--mesh", mode, "--json"]),
        ] {
            let output = run_ee(&workspace, &args)?;
            ensure_success(&output, &format!("{label} --mesh {mode}"))?;
            let json = stdout_json(&output, &format!("{label} --mesh {mode}"))?;
            assert_no_mesh_degradation(&json, &format!("{label} --mesh {mode}"))?;
        }
    }

    let after = listener_snapshot()?;
    assert_no_new_mesh_listener(&before, &after)
}
