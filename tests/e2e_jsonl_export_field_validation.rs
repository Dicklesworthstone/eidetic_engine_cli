//! E2E test validating JSONL import rejects records with missing required fields.
//!
//! Verifies that `ee import jsonl` surfaces proper error envelopes when
//! records fail ExportRecordBuildError validation (missing memory_id, blank
//! content, etc.), rather than silently skipping malformed records.
//!
//! The database has CHECK constraints that prevent corrupt data from being
//! written, so we test the validation path via import (where malformed JSONL
//! can be constructed) rather than export (where the DB guarantees validity).

#![cfg(unix)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
        .as_nanos();
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("jsonl-field-validation")
        .join(format!("{}-{}-{nanos}", name, std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn path_arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn run_ee(workspace: &Path, args: &[&str]) -> Result<(i32, Value, String), String> {
    let mut full_args = vec!["--workspace", path_arg(workspace)?, "--json"];
    full_args.extend(args);

    let output = Command::new(ee_bin())
        .args(&full_args)
        .env_remove("EE_WORKSPACE")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("spawn ee: {error}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let parsed: Value = serde_json::from_str(&stdout).map_err(|error| {
        format!("parse stdout JSON: {error}\nstdout: {stdout}\nstderr: {stderr}")
    })?;

    Ok((exit_code, parsed, stderr.into_owned()))
}

/// Create a JSONL file with a valid header but a memory record missing its ID.
fn write_jsonl_with_blank_memory_id(path: &Path) -> TestResult {
    let header = json!({
        "schema": "ee.export.v1",
        "format_version": 1,
        "export_timestamp": "2026-01-01T00:00:00Z",
        "source_workspace_id": "ws_test00000000000000000000",
        "import_source": "native",
        "scope": "full",
        "trust_level": "verified",
        "record_count": 1
    });
    let memory = json!({
        "record_type": "memory",
        "memory_id": "",  // BLANK - should trigger validation error
        "level": "episodic",
        "kind": "fact",
        "content": "test content",
        "confidence": 0.8,
        "utility": 0.5,
        "importance": 0.5,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "trust_class": "agent_assertion"
    });
    let jsonl = format!("{}\n{}\n", header, memory);
    fs::write(path, jsonl).map_err(|e| format!("write jsonl: {e}"))
}

/// Create a JSONL file with a valid header but a memory record with blank content.
fn write_jsonl_with_blank_content(path: &Path) -> TestResult {
    let header = json!({
        "schema": "ee.export.v1",
        "format_version": 1,
        "export_timestamp": "2026-01-01T00:00:00Z",
        "source_workspace_id": "ws_test00000000000000000000",
        "import_source": "native",
        "scope": "full",
        "trust_level": "verified",
        "record_count": 1
    });
    let memory = json!({
        "record_type": "memory",
        "memory_id": "mem_test00000000000000000000",
        "level": "episodic",
        "kind": "fact",
        "content": "",  // BLANK - should trigger validation error
        "confidence": 0.8,
        "utility": 0.5,
        "importance": 0.5,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "trust_class": "agent_assertion"
    });
    let jsonl = format!("{}\n{}\n", header, memory);
    fs::write(path, jsonl).map_err(|e| format!("write jsonl: {e}"))
}

#[test]
fn import_jsonl_rejects_blank_memory_id_with_issue_code() -> TestResult {
    let root = unique_artifact_dir("blank-memory-id")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    // 1. Initialize workspace
    let (exit_code, parsed, _stderr) = run_ee(&workspace, &["init"])?;
    ensure_equal(&exit_code, &0, "init exit code")?;
    ensure(
        parsed.pointer("/success") == Some(&json!(true)),
        format!("init must succeed: {parsed}"),
    )?;

    // 2. Create malformed JSONL with blank memory_id
    let jsonl_path = root.join("malformed.jsonl");
    write_jsonl_with_blank_memory_id(&jsonl_path)?;

    // 3. Attempt import - should report rejection with issue codes
    let (exit_code, parsed, stderr) = run_ee(
        &workspace,
        &[
            "import",
            "jsonl",
            "--source",
            path_arg(&jsonl_path)?,
            "--dry-run",
        ],
    )?;

    // 4. Assert proper response envelope
    ensure_equal(
        &exit_code,
        &EXIT_SUCCESS,
        "rejected import still returns parseable report",
    )?;
    ensure(
        stderr.is_empty(),
        format!("JSON mode must keep stderr empty, got: {stderr}"),
    )?;
    ensure_equal(
        &parsed.pointer("/schema"),
        &Some(&json!("ee.response.v1")),
        "response schema",
    )?;
    ensure_equal(
        &parsed.pointer("/data/status"),
        &Some(&json!("rejected")),
        "import status must be 'rejected'",
    )?;
    ensure_equal(
        &parsed.pointer("/data/memoriesImported"),
        &Some(&json!(0)),
        "no memories should be imported",
    )?;

    // 5. Assert issue codes are surfaced
    let issues = parsed
        .pointer("/data/issues")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("import must expose issues array: {parsed}"))?;
    ensure(
        issues
            .iter()
            .any(|issue| issue.get("severity").and_then(Value::as_str) == Some("error")),
        format!("import must report error-severity issue for blank memory_id: {issues:?}"),
    )?;

    Ok(())
}

#[test]
fn import_jsonl_rejects_blank_content_with_issue_code() -> TestResult {
    let root = unique_artifact_dir("blank-content")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    // 1. Initialize workspace
    let (exit_code, _, _) = run_ee(&workspace, &["init"])?;
    ensure_equal(&exit_code, &0, "init exit code")?;

    // 2. Create malformed JSONL with blank content
    let jsonl_path = root.join("malformed.jsonl");
    write_jsonl_with_blank_content(&jsonl_path)?;

    // 3. Attempt import
    let (exit_code, parsed, stderr) = run_ee(
        &workspace,
        &[
            "import",
            "jsonl",
            "--source",
            path_arg(&jsonl_path)?,
            "--dry-run",
        ],
    )?;

    // 4. Assert proper response envelope
    ensure_equal(
        &exit_code,
        &EXIT_SUCCESS,
        "rejected import still returns parseable report",
    )?;
    ensure(stderr.is_empty(), "stderr must be empty in JSON mode")?;
    ensure_equal(
        &parsed.pointer("/data/status"),
        &Some(&json!("rejected")),
        "import status must be 'rejected'",
    )?;

    // 5. Assert issue codes are surfaced
    let issues = parsed
        .pointer("/data/issues")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("import must expose issues array: {parsed}"))?;
    ensure(
        !issues.is_empty(),
        format!("import must report issues for blank content: {issues:?}"),
    )?;

    Ok(())
}
