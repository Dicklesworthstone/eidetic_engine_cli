use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn run_ee(workspace: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_json(workspace: &Path, args: &[&str]) -> Result<Value, String> {
    let output = run_ee(workspace, args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed with status {:?}; stderr: {stderr}; stdout: {stdout}",
            args.join(" "),
            output.status.code()
        ));
    }
    if !stderr.is_empty() {
        return Err(format!(
            "ee {} must keep JSON diagnostics out of stderr, got {stderr:?}",
            args.join(" ")
        ));
    }
    serde_json::from_str(&stdout)
        .map_err(|error| format!("ee {} stdout must be JSON: {error}", args.join(" ")))
}

fn scrub_for_fixture(value: &mut Value, workspace: &Path) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                scrub_for_fixture(child, workspace);
                if key.to_ascii_lowercase().contains("fingerprint") && child.is_string() {
                    *child = Value::String("[HASH]".to_string());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_for_fixture(item, workspace);
            }
        }
        Value::String(text) => {
            let workspace_text = workspace.to_string_lossy();
            *text = text.replace(workspace_text.as_ref(), "[WORKSPACE]");
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Result<&'a Value, String> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .ok_or_else(|| format!("missing JSON field {}", path.join(".")))?;
    }
    Ok(current)
}

fn ensure_string(value: &Value, path: &[&str], expected: &str) -> TestResult {
    let actual = value_at(value, path)?
        .as_str()
        .ok_or_else(|| format!("JSON field {} should be a string", path.join(".")))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "JSON field {} should be {expected:?}, got {actual:?}",
            path.join(".")
        ))
    }
}

fn ensure_bool(value: &Value, path: &[&str], expected: bool) -> TestResult {
    let actual = value_at(value, path)?
        .as_bool()
        .ok_or_else(|| format!("JSON field {} should be a bool", path.join(".")))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "JSON field {} should be {expected}, got {actual}",
            path.join(".")
        ))
    }
}

#[test]
fn status_supports_default_v1_and_explicit_v0_response_envelopes() -> TestResult {
    let workspace = tempfile::Builder::new()
        .prefix("ee-schema-compat-")
        .tempdir()
        .map_err(|error| format!("failed to create schema compat workspace: {error}"))?;
    let workspace_path = workspace.path();

    let v1 = run_json(workspace_path, &["--workspace", ".", "--json", "status"])?;
    ensure_string(&v1, &["schema"], "ee.response.v1")?;
    ensure_bool(&v1, &["success"], true)?;
    if v1.get("data").is_none() || v1.get("result").is_some() {
        return Err("v1 response should use data and not result".to_string());
    }

    let v0 = run_json(
        workspace_path,
        &[
            "--workspace",
            ".",
            "--schema-version",
            "v0",
            "--json",
            "status",
        ],
    )?;
    ensure_string(&v0, &["schema"], "ee.response.v0")?;
    ensure_bool(&v0, &["ok"], true)?;
    if v0.get("result").is_none() || v0.get("data").is_some() {
        return Err("v0 response should use result and not data".to_string());
    }
    ensure_string(&v0, &["result", "command"], "status")?;

    let legacy = run_json(
        workspace_path,
        &["--workspace", ".", "--legacy-schema", "--json", "status"],
    )?;
    ensure_string(&legacy, &["schema"], "ee.response.v0")?;

    Ok(())
}

#[test]
fn context_supports_explicit_v0_response_envelope() -> TestResult {
    let workspace = tempfile::Builder::new()
        .prefix("ee-schema-context-compat-")
        .tempdir()
        .map_err(|error| format!("failed to create schema compat workspace: {error}"))?;
    let workspace_path = workspace.path();

    run_json(workspace_path, &["--workspace", ".", "--json", "init"])?;
    run_json(
        workspace_path,
        &[
            "--workspace",
            ".",
            "--json",
            "remember",
            "Run cargo fmt before release.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "release,formatting",
        ],
    )?;
    run_json(
        workspace_path,
        &["--workspace", ".", "--json", "index", "rebuild"],
    )?;

    let context = run_json(
        workspace_path,
        &[
            "--workspace",
            ".",
            "--schema-version",
            "v0",
            "--json",
            "context",
            "format before release",
            "--profile",
            "compact",
            "--candidate-pool",
            "5",
            "--max-tokens",
            "512",
        ],
    )?;

    ensure_string(&context, &["schema"], "ee.response.v0")?;
    ensure_bool(&context, &["ok"], true)?;
    if context.get("result").is_none() || context.get("data").is_some() {
        return Err("context v0 response should use result and not data".to_string());
    }
    ensure_string(&context, &["result", "command"], "context")?;
    ensure_string(
        &context,
        &["result", "request", "query"],
        "format before release",
    )?;

    Ok(())
}

#[test]
fn status_v0_golden_fixture_is_byte_stable_after_scrubbing() -> TestResult {
    let workspace = tempfile::Builder::new()
        .prefix("ee-schema-compat-golden-")
        .tempdir()
        .map_err(|error| format!("failed to create schema compat workspace: {error}"))?;
    let workspace_path = workspace.path();
    let mut value = run_json(
        workspace_path,
        &[
            "--workspace",
            ".",
            "--schema-version",
            "v0",
            "--fields",
            "minimal",
            "--json",
            "status",
        ],
    )?;
    scrub_for_fixture(&mut value, workspace_path);

    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&value)
            .map_err(|error| format!("failed to render v0 status fixture: {error}"))?
    );
    let expected_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden/schema_v0/status_minimal.json.golden");
    let expected = fs::read_to_string(&expected_path)
        .map_err(|error| format!("failed to read {}: {error}", expected_path.display()))?;
    if actual != expected {
        return Err(format!(
            "schema v0 status fixture drifted; update {} only for an intentional contract change",
            expected_path.display()
        ));
    }

    Ok(())
}
