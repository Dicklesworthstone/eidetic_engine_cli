//! J11.5 contract tests for the focused failure-mode impact runner.
//!
//! The runner is intentionally read-only: these tests invoke it with path
//! inputs and inspect the JSON command plan without running the emitted gates.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, process::Command};

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

fn run_impact(paths: &[&str]) -> Result<Value, String> {
    let mut command = Command::new("bash");
    command
        .current_dir(repo_root())
        .arg("scripts/failure_mode_impact.sh")
        .arg("--changed");
    for path in paths {
        command.arg(path);
    }
    command.arg("--json");

    let output = command
        .output()
        .map_err(|error| format!("failed to spawn impact runner: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "impact runner failed with {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("impact runner stdout was not JSON: {error}"))
}

fn golden_report(name: &str) -> Result<Value, String> {
    let path = format!(
        "{}/tests/fixtures/failure_mode_impact/{name}.json",
        repo_root()
    );
    let text = fs::read_to_string(&path).map_err(|error| format!("read {path}: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {path}: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn command_ids(report: &Value) -> Result<Vec<String>, String> {
    report["data"]["commands"]
        .as_array()
        .ok_or_else(|| "commands must be an array".to_string())?
        .iter()
        .map(|command| {
            command["id"]
                .as_str()
                .ok_or_else(|| "command id must be a string".to_string())
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()
}

fn fixture_codes(report: &Value) -> Result<Vec<String>, String> {
    report["data"]["fixtureCodes"]
        .as_array()
        .ok_or_else(|| "fixtureCodes must be an array".to_string())?
        .iter()
        .map(|code| {
            code.as_str()
                .ok_or_else(|| "fixture code must be a string".to_string())
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()
}

#[test]
fn fixture_only_change_gets_complete_focused_j6_plan() -> TestResult {
    let report = run_impact(&["tests/fixtures/failure_modes/weak_query_recall.json"])?;

    ensure(
        report == golden_report("complete_fixture")?,
        "complete fixture report must match golden JSON",
    )?;
    ensure(
        report["schema"] == "ee.failure_mode_impact.v1",
        "schema pin",
    )?;
    ensure(
        report["data"]["impactStatus"] == "complete",
        "fixture-only changes should have complete focused impact",
    )?;
    ensure(
        fixture_codes(&report)? == vec!["weak_query_recall".to_string()],
        "fixture code mapping",
    )?;
    let ids = command_ids(&report)?;
    ensure(
        ids.contains(&"focused_j6".to_string()),
        "focused J6 command",
    )?;
    ensure(
        !ids.contains(&"full_j6_catalog".to_string()),
        "complete focused impact should not require full catalog in the command plan",
    )
}

#[test]
fn generated_doc_change_is_partial_and_preserves_full_catalog_caveat() -> TestResult {
    let report = run_impact(&["docs/degraded_codes.md"])?;

    ensure(
        report == golden_report("partial_docs")?,
        "partial docs report must match golden JSON",
    )?;
    ensure(
        report["data"]["impactStatus"] == "partial",
        "generated docs are partial from path-only evidence",
    )?;
    ensure(
        report["data"]["signals"]["docsChanged"] == true,
        "docs changed signal",
    )?;
    let ids = command_ids(&report)?;
    ensure(
        ids.contains(&"regenerate_degraded_codes_doc".to_string()),
        "doc regeneration command",
    )?;
    ensure(
        ids.contains(&"full_j6_catalog".to_string()),
        "partial impact should recommend full J6",
    )
}

#[test]
fn source_path_with_known_codes_gets_partial_focused_plan() -> TestResult {
    let report = run_impact(&["src/core/index.rs"])?;

    ensure(
        report["data"]["impactStatus"] == "partial",
        "source-code path mapping should be partial",
    )?;
    ensure(
        fixture_codes(&report)?
            .iter()
            .any(|code| code == "index_publish_lock_contention"),
        "source mapping should detect index_publish_lock_contention",
    )?;
    let ids = command_ids(&report)?;
    ensure(
        ids.contains(&"focused_j6".to_string()),
        "focused J6 command",
    )?;
    ensure(
        ids.contains(&"full_j6_catalog".to_string()),
        "source impact should keep full catalog caveat",
    )
}

#[test]
fn driver_change_is_ambiguous_and_requires_full_catalog() -> TestResult {
    let report = run_impact(&["scripts/e2e_overhaul/failure_modes.sh"])?;

    ensure(
        report == golden_report("ambiguous_driver")?,
        "ambiguous driver report must match golden JSON",
    )?;
    ensure(
        report["data"]["impactStatus"] == "ambiguous",
        "driver changes can affect any fixture",
    )?;
    ensure(
        report["data"]["signals"]["driverChanged"] == true,
        "driver changed signal",
    )?;
    let ids = command_ids(&report)?;
    ensure(
        ids.contains(&"full_j6_catalog".to_string()),
        "ambiguous impact should require full catalog",
    )
}
