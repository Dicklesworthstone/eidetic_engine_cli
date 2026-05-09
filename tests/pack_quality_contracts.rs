use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use insta::assert_json_snapshot;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    tempfile::Builder::new()
        .prefix(&format!("{prefix}-"))
        .tempdir()
        .map(tempfile::TempDir::keep)
        .map_err(|error| format!("failed to create {prefix} artifact directory: {error}"))
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    let exe =
        std::env::var("CARGO_BIN_EXE_ee").unwrap_or_else(|_| env!("CARGO_BIN_EXE_ee").to_string());
    Command::new(&exe)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee: {error}"))
}

fn scrub_pack_quality_report(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                scrub_pack_quality_report(child);
                if key == "fixtureDir" && child.is_string() {
                    *child = Value::String("[FIXTURE_DIR]".to_string());
                }
                if key == "stdout" && child.is_string() {
                    if let Some(s) = child.as_str() {
                        if s.contains("target/ee-e2e") {
                            *child = Value::String(
                                s.replace(|c: char| c.is_ascii_hexdigit(), "X").to_string(),
                            );
                        }
                    }
                }
                if key == "selectedIds" || key == "actualSelectedIds" {
                    if let Value::Array(arr) = child {
                        arr.sort_by(|a, b| {
                            let a_str = a.as_str().unwrap_or("");
                            let b_str = b.as_str().unwrap_or("");
                            a_str.cmp(b_str)
                        });
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_pack_quality_report(item);
            }
        }
        _ => {}
    }
}

#[test]
fn pack_quality_report_schema_is_registered() -> TestResult {
    let output = run_ee(&["--json", "version"])?;
    let stdout = String::from_utf8(output.stdout).map_err(|e| format!("stdout not UTF-8: {e}"))?;
    let value: Value =
        serde_json::from_str(&stdout).map_err(|e| format!("version output not JSON: {e}"))?;

    let schemas = value
        .pointer("/data/schemas")
        .and_then(Value::as_array)
        .ok_or("missing /data/schemas array")?;

    let schema_names: BTreeSet<&str> = schemas
        .iter()
        .filter_map(|s| s.get("name").and_then(Value::as_str))
        .collect();

    if !schema_names.contains("pack_quality_report") {
        return Err(format!(
            "pack_quality_report not in registered schemas: {:?}",
            schema_names
        ));
    }

    Ok(())
}

#[test]
fn pack_quality_report_has_stable_field_names() -> TestResult {
    let output = run_ee(&[
        "--json",
        "eval",
        "run",
        "release_failure",
        "--pack-quality",
        "--scenario",
        "usr_pre_task_brief",
    ])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("eval run --pack-quality failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout).map_err(|e| format!("stdout not UTF-8: {e}"))?;
    let stderr = String::from_utf8(output.stderr).map_err(|e| format!("stderr not UTF-8: {e}"))?;

    if !stderr.is_empty() {
        return Err(format!(
            "pack-quality JSON mode must keep diagnostics off stdout, got stderr: {stderr}"
        ));
    }

    let mut value: Value =
        serde_json::from_str(&stdout).map_err(|e| format!("pack-quality output not JSON: {e}"))?;

    let schema = value
        .pointer("/schema")
        .and_then(Value::as_str)
        .ok_or("missing /schema")?;
    if schema != "ee.response.v1" {
        return Err(format!("expected ee.response.v1, got {schema}"));
    }

    let report_schema = value
        .pointer("/data/report/schema")
        .and_then(Value::as_str)
        .ok_or("missing /data/report/schema")?;
    if report_schema != "ee.eval.pack_quality_report.v1" {
        return Err(format!(
            "expected ee.eval.pack_quality_report.v1, got {report_schema}"
        ));
    }

    let required_fields = [
        "/data/command",
        "/data/mode",
        "/data/report/fixture_id",
        "/data/report/aggregate_verdict",
        "/data/report/cases_total",
        "/data/report/cases_within",
        "/data/report/cases_drift",
        "/data/report/cases_regression",
        "/data/report/cases_inconclusive",
        "/data/report/comparisons",
    ];

    for field in &required_fields {
        if value.pointer(field).is_none() {
            return Err(format!("missing required field: {field}"));
        }
    }

    scrub_pack_quality_report(&mut value);
    assert_json_snapshot!("pack_quality_report_stable_fields", value);

    Ok(())
}

#[test]
fn pack_quality_report_arrays_are_deterministically_sorted() -> TestResult {
    let output1 = run_ee(&[
        "--json",
        "eval",
        "run",
        "release_failure",
        "--pack-quality",
        "--scenario",
        "usr_pre_task_brief",
    ])?;
    let output2 = run_ee(&[
        "--json",
        "eval",
        "run",
        "release_failure",
        "--pack-quality",
        "--scenario",
        "usr_pre_task_brief",
    ])?;

    let stdout1 =
        String::from_utf8(output1.stdout).map_err(|e| format!("stdout1 not UTF-8: {e}"))?;
    let stdout2 =
        String::from_utf8(output2.stdout).map_err(|e| format!("stdout2 not UTF-8: {e}"))?;

    let mut v1: Value =
        serde_json::from_str(&stdout1).map_err(|e| format!("output1 not JSON: {e}"))?;
    let mut v2: Value =
        serde_json::from_str(&stdout2).map_err(|e| format!("output2 not JSON: {e}"))?;

    scrub_pack_quality_report(&mut v1);
    scrub_pack_quality_report(&mut v2);

    if v1 != v2 {
        return Err(
            "pack-quality reports differ between identical runs - ordering is not deterministic"
                .to_string(),
        );
    }

    Ok(())
}

#[test]
fn pack_quality_lexical_only_branch_is_reported() -> TestResult {
    let output = run_ee(&[
        "--json",
        "eval",
        "run",
        "release_failure",
        "--pack-quality",
        "--scenario",
        "usr_pre_task_brief",
    ])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("eval run --pack-quality failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout).map_err(|e| format!("stdout not UTF-8: {e}"))?;
    let value: Value =
        serde_json::from_str(&stdout).map_err(|e| format!("output not JSON: {e}"))?;

    let degraded_branches = value
        .pointer("/data/degradedBranches")
        .and_then(Value::as_array)
        .ok_or("missing /data/degradedBranches")?;

    let has_lexical_only = degraded_branches.iter().any(|branch| {
        branch
            .get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| code == "semantic_disabled")
    });

    if !has_lexical_only {
        return Err(format!(
            "expected semantic_disabled degraded branch, got: {:?}",
            degraded_branches
        ));
    }

    Ok(())
}

#[test]
fn pack_quality_artifact_paths_are_reported() -> TestResult {
    let output = run_ee(&[
        "--json",
        "eval",
        "run",
        "release_failure",
        "--pack-quality",
        "--scenario",
        "usr_pre_task_brief",
    ])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("eval run --pack-quality failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout).map_err(|e| format!("stdout not UTF-8: {e}"))?;
    let value: Value =
        serde_json::from_str(&stdout).map_err(|e| format!("output not JSON: {e}"))?;

    let artifact_paths = value
        .pointer("/data/artifactPaths")
        .and_then(Value::as_array)
        .ok_or("missing /data/artifactPaths")?;

    if artifact_paths.is_empty() {
        return Err("artifactPaths should not be empty for pack-quality run".to_string());
    }

    let first = &artifact_paths[0];
    if first.get("stdout").is_none() {
        return Err("artifact path entry missing stdout field".to_string());
    }

    Ok(())
}

#[test]
fn pack_quality_no_diagnostics_on_json_stdout() -> TestResult {
    let output = run_ee(&[
        "--json",
        "eval",
        "run",
        "release_failure",
        "--pack-quality",
        "--scenario",
        "usr_pre_task_brief",
    ])?;

    let stdout = String::from_utf8(output.stdout).map_err(|e| format!("stdout not UTF-8: {e}"))?;
    let stderr = String::from_utf8(output.stderr).map_err(|e| format!("stderr not UTF-8: {e}"))?;

    if !stderr.is_empty() {
        return Err(format!(
            "JSON mode must not emit diagnostics to stderr: {stderr}"
        ));
    }

    if stdout.contains("WARN") || stdout.contains("ERROR") || stdout.contains("DEBUG") {
        return Err(format!("stdout contains diagnostic markers: {stdout}"));
    }

    if stdout.contains('\x1b') {
        return Err("stdout contains ANSI escape codes".to_string());
    }

    Ok(())
}

#[test]
fn pack_quality_verdict_values_are_stable() -> TestResult {
    let output = run_ee(&["--json", "eval", "run", "release_failure", "--pack-quality"])?;

    let stdout = String::from_utf8(output.stdout).map_err(|e| format!("stdout not UTF-8: {e}"))?;
    let value: Value =
        serde_json::from_str(&stdout).map_err(|e| format!("output not JSON: {e}"))?;

    let verdict = value
        .pointer("/data/report/aggregate_verdict")
        .and_then(Value::as_str)
        .ok_or("missing aggregate_verdict")?;

    let valid_verdicts = ["within", "drift", "regression", "inconclusive"];
    if !valid_verdicts.contains(&verdict) {
        return Err(format!(
            "unexpected verdict {verdict}, expected one of: {:?}",
            valid_verdicts
        ));
    }

    Ok(())
}
