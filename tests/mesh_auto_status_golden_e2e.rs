//! Golden E2E coverage for the read-only mesh auto-enrollment status block.
//!
//! The existing mesh auto-status golden covers a constructed foreground
//! snapshot. This test drives the real `ee` binary against an initialized
//! workspace so the CLI envelope, fsqlite workspace, frankensearch-backed search
//! path, and mesh status renderer stay aligned.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn run_ee(workspace: &Path, args: &[&str]) -> Result<Output, String> {
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

fn ensure_success(output: &Output, label: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label}: ee exited {:?}\nstderr:\n{}\nstdout:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ))
    }
}

fn stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout:\n{stdout}"))
}

fn run_json(workspace: &Path, label: &str, args: &[&str]) -> Result<Value, String> {
    let output = run_ee(workspace, args)?;
    ensure_success(&output, label)?;
    stdout_json(&output, label)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("mesh_auto_status_disabled_e2e.snap")
}

fn pretty(value: &Value) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to render JSON: {error}"))?;
    text.push('\n');
    Ok(text)
}

fn data_object<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{label}: missing object data field"))
}

fn required_string(value: &Value, pointer: &str, label: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{label}: missing string at {pointer}"))
}

fn scrub_workspace_strings(value: &mut Value, workspace: &Path) {
    let workspace_path = workspace.display().to_string();
    let database_path = workspace.join(".ee").join("ee.db").display().to_string();
    match value {
        Value::String(text) => {
            *text = text
                .replace(&database_path, "[WORKSPACE]/.ee/ee.db")
                .replace(&workspace_path, "[WORKSPACE]");
        }
        Value::Array(items) => {
            for item in items {
                scrub_workspace_strings(item, workspace);
            }
        }
        Value::Object(fields) => {
            for item in fields.values_mut() {
                scrub_workspace_strings(item, workspace);
            }
        }
        _ => {}
    }
}

fn canonical_mesh_status(value: &Value, workspace: &Path) -> Result<Value, String> {
    let data = data_object(value, "mesh status")?;
    let workspace_id = required_string(value, "/data/workspaceId", "mesh status")?;
    if !workspace_id.starts_with("wsp_") {
        return Err(format!(
            "mesh status: workspaceId should be redaction-safe wsp_* value, got {workspace_id:?}"
        ));
    }

    let mut projected = json!({
        "schema": "ee.mesh.auto_status_cli_golden.v1",
        "response": {
            "schema": value.get("schema").cloned().unwrap_or(Value::Null),
            "success": value.get("success").cloned().unwrap_or(Value::Null)
        },
        "meshStatus": {
            "schema": data.get("schema").cloned().unwrap_or(Value::Null),
            "command": data.get("command").cloned().unwrap_or(Value::Null),
            "workspaceId": "[WORKSPACE_ID]",
            "workspacePath": data.get("workspacePath").cloned().unwrap_or(Value::Null),
            "databasePath": data.get("databasePath").cloned().unwrap_or(Value::Null),
            "initialized": data.get("initialized").cloned().unwrap_or(Value::Null),
            "meshEnabled": data.get("meshEnabled").cloned().unwrap_or(Value::Null),
            "mode": data.get("mode").cloned().unwrap_or(Value::Null),
            "posture": data.get("posture").cloned().unwrap_or(Value::Null),
            "storage": data.get("storage").cloned().unwrap_or(Value::Null),
            "selectiveSync": data.get("selectiveSync").cloned().unwrap_or(Value::Null),
            "autoEnrollment": data.get("autoEnrollment").cloned().unwrap_or(Value::Null),
            "repairCommands": data.get("repairCommands").cloned().unwrap_or(Value::Null),
            "degraded": data.get("degraded").cloned().unwrap_or(Value::Null)
        }
    });
    scrub_workspace_strings(&mut projected, workspace);
    Ok(projected)
}

fn assert_golden(actual: &Value) -> TestResult {
    let actual_text = pretty(actual)?;
    let path = golden_path();
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    if actual_text == expected {
        Ok(())
    } else {
        Err(format!(
            "mesh auto-status golden drifted\nGolden file: {}\n\nexpected:\n{}\nactual:\n{}",
            path.display(),
            expected,
            actual_text
        ))
    }
}

#[test]
fn mesh_status_auto_enrollment_disabled_matches_cli_golden() -> TestResult {
    let tempdir = tempfile::Builder::new()
        .prefix("ee-mesh-auto-status-golden-")
        .tempdir_in("/tmp")
        .map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path();

    run_json(workspace, "init", &["init", "--json"])?;
    run_json(
        workspace,
        "remember",
        &[
            "remember",
            "Mesh auto status golden must stay read-only and avoid peer materialization.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
    )?;
    let search = run_json(
        workspace,
        "search",
        &["search", "mesh auto status golden", "--json"],
    )?;
    let hits = search
        .pointer("/data/results")
        .or_else(|| search.pointer("/data/hits"))
        .and_then(Value::as_array)
        .ok_or_else(|| "search: missing result array".to_owned())?;
    if hits.is_empty() {
        return Err("search: real workspace search returned no hits".to_owned());
    }

    let first = run_json(workspace, "mesh status", &["mesh", "status", "--json"])?;
    let second = run_json(
        workspace,
        "mesh status repeat",
        &["mesh", "status", "--json"],
    )?;
    let first_projection = canonical_mesh_status(&first, workspace)?;
    let second_projection = canonical_mesh_status(&second, workspace)?;
    if first_projection != second_projection {
        return Err(format!(
            "mesh status canonical output is not stable\nfirst:\n{}\nsecond:\n{}",
            pretty(&first_projection)?,
            pretty(&second_projection)?
        ));
    }

    assert_golden(&first_projection)
}
