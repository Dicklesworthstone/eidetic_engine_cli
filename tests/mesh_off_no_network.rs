//! bd-x4hn7: mesh-off ordinary commands stay quiet and do not leave listeners.
//!
//! This is the Cargo-test companion to `scripts/e2e_overhaul/mesh_off_no_network.sh`
//! so remote-only RCH verification can exercise the built `ee` binary directly.

use std::collections::BTreeSet;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn temp_workspace() -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("ee-mesh-off-no-network-")
        .tempdir_in("/tmp")
        .map_err(|error| format!("failed to create temp workspace under /tmp: {error}"))
}

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env("EE_MESH_ENABLED", "0")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn ensure_success(output: &Output, label: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label}: ee exited {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ))
    }
}

fn collect_codes(value: &serde_json::Value, codes: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(code) = map.get("code").and_then(|code| code.as_str()) {
                codes.push(code.to_owned());
            }
            for child in map.values() {
                collect_codes(child, codes);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_codes(child, codes);
            }
        }
        _ => {}
    }
}

fn assert_no_mesh_degradation(json: &serde_json::Value, label: &str) -> TestResult {
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

fn assert_no_mesh_data_key(json: &serde_json::Value, label: &str) -> TestResult {
    let Some(data) = json.get("data").and_then(|data| data.as_object()) else {
        return Ok(());
    };
    let mesh_keys: Vec<&String> = data
        .keys()
        .filter(|key| {
            let lowered = key.to_ascii_lowercase();
            lowered.contains("mesh") || lowered.contains("tailscale") || lowered.contains("peer")
        })
        .collect();
    if mesh_keys.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{label}: unexpected mesh data key(s): {mesh_keys:?}"
        ))
    }
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
            "mesh-off status left unexpected listener(s): {new_mesh:?}"
        ))
    }
}

#[test]
fn mesh_off_status_reports_capability_without_polluting_ordinary_json() -> TestResult {
    let workspace_dir = temp_workspace()?;
    let workspace = workspace_dir.path().to_string_lossy().into_owned();

    let init = run_ee(&workspace, &["init", "--json"])?;
    ensure_success(&init, "init")?;

    let status = run_ee(&workspace, &["status", "--json"])?;
    ensure_success(&status, "status")?;
    let status_json = stdout_json(&status, "status")?;
    let posture = status_json
        .pointer("/data/capabilities/mesh")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "status: missing data.capabilities.mesh".to_owned())?;
    if !matches!(
        posture,
        "disabled" | "pending" | "unavailable" | "degraded" | "ok"
    ) {
        return Err(format!(
            "status: unexpected mesh capability posture {posture:?}"
        ));
    }

    let cases: [(&str, Vec<&str>); 3] = [
        (
            "remember",
            vec![
                "remember",
                "--level",
                "procedural",
                "--kind",
                "rule",
                "Mesh-off e2e ordinary command fixture.",
                "--json",
            ],
        ),
        (
            "search",
            vec!["search", "mesh-off ordinary command fixture", "--json"],
        ),
        (
            "context",
            vec![
                "context",
                "mesh-off ordinary command fixture",
                "--max-tokens",
                "500",
                "--json",
            ],
        ),
    ];

    for (label, args) in cases {
        let output = run_ee(&workspace, &args)?;
        ensure_success(&output, label)?;
        let json = stdout_json(&output, label)?;
        assert_no_mesh_degradation(&json, label)?;
        assert_no_mesh_data_key(&json, label)?;
    }

    let before = listener_snapshot()?;
    let status = run_ee(&workspace, &["status", "--json"])?;
    ensure_success(&status, "status listener probe")?;
    let after = listener_snapshot()?;
    assert_no_new_mesh_listener(&before, &after)
}
