use std::fs;
use std::path::Path;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const CANONICAL_WORKSPACE_ID: &str = "wsp_meshforeground0000000001";
const CANONICAL_WORKSPACE_PATH: &str = "/tmp/ee-mesh";
const GOLDEN_PATH: &str = "tests/fixtures/golden/mesh/foreground_status_disabled_cli_envelope.json";

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
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
            "{label} failed with exit {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout:\n{stdout}"))
}

fn replace_string_values(value: &mut serde_json::Value, from: &str, to: &str) {
    if from.is_empty() || from == to {
        return;
    }

    match value {
        serde_json::Value::String(text) => {
            if text.contains(from) {
                *text = text.replace(from, to);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                replace_string_values(item, from, to);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                replace_string_values(item, from, to);
            }
        }
        _ => {}
    }
}

fn canonicalize_mesh_status(
    mut value: serde_json::Value,
    workspace_path: &Path,
) -> serde_json::Value {
    let workspace = workspace_path.to_string_lossy();
    replace_string_values(&mut value, &workspace, CANONICAL_WORKSPACE_PATH);
    if let Ok(canonical) = workspace_path.canonicalize() {
        replace_string_values(
            &mut value,
            &canonical.to_string_lossy(),
            CANONICAL_WORKSPACE_PATH,
        );
    }

    if let Some(slot) = value.pointer_mut("/data/workspaceId") {
        *slot = serde_json::json!(CANONICAL_WORKSPACE_ID);
    }

    value
}

fn assert_matches_golden(actual: &serde_json::Value) -> TestResult {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_PATH);
    let expected_text = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let expected: serde_json::Value = serde_json::from_str(&expected_text)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;

    if *actual == expected {
        Ok(())
    } else {
        Err(format!(
            "mesh status disabled CLI golden drifted\nexpected:\n{}\nactual:\n{}",
            serde_json::to_string_pretty(&expected).unwrap_or_else(|_| expected.to_string()),
            serde_json::to_string_pretty(actual).unwrap_or_else(|_| actual.to_string())
        ))
    }
}

fn log_event(event: &str, data: serde_json::Value) {
    eprintln!(
        "{}",
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "suite": "mesh_status_golden_e2e",
            "test": "mesh_status_disabled_cli_envelope_matches_golden",
            "event": event,
            "data": data,
        })
    );
}

#[test]
fn mesh_status_disabled_cli_envelope_matches_golden() -> TestResult {
    let tempdir = tempfile::Builder::new()
        .prefix("ee-mesh-status-golden-")
        .tempdir_in("/tmp")
        .map_err(|error| format!("failed to create temp workspace: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&workspace, &["init", "--json"])?;
    ensure_success(&init, "ee init")?;

    let status = run_ee(&workspace, &["mesh", "status", "--json"])?;
    ensure_success(&status, "ee mesh status --json")?;
    if !status.stderr.is_empty() {
        return Err(format!(
            "ee mesh status --json should keep JSON mode stderr quiet, got:\n{}",
            String::from_utf8_lossy(&status.stderr)
        ));
    }

    let actual = stdout_json(&status, "ee mesh status --json")?;
    if actual.pointer("/data/meshEnabled") != Some(&serde_json::json!(false)) {
        return Err(format!("mesh status should remain disabled: {actual}"));
    }
    if actual.pointer("/data/autoEnrollment/tailscale/status")
        != Some(&serde_json::json!("not_probed"))
    {
        return Err("disabled mesh status must not probe local Tailscale".to_owned());
    }

    let canonical = canonicalize_mesh_status(actual, tempdir.path());
    log_event(
        "compare_golden",
        serde_json::json!({
            "goldenPath": GOLDEN_PATH,
            "responseSchema": canonical["schema"],
            "meshSchema": canonical["data"]["schema"],
            "discoverySchema": canonical["data"]["autoEnrollment"]["discovery"]["schema"],
        }),
    );
    assert_matches_golden(&canonical)
}
