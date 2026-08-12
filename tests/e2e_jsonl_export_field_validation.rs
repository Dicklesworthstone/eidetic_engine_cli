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

fn run_ee_text(workspace: &Path, args: &[&str]) -> Result<(i32, String, String), String> {
    let mut full_args = vec!["--workspace", path_arg(workspace)?];
    full_args.extend(args);

    let output = Command::new(ee_bin())
        .args(&full_args)
        .env_remove("EE_WORKSPACE")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("spawn ee: {error}"))?;

    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
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

fn write_valid_jsonl(path: &Path) -> TestResult {
    let jsonl = [
        r#"{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-04-30T00:00:00Z","workspace_id":"wsp_01234567890123456789012345","workspace_path":"/source","export_scope":"memories","redaction_level":"none","record_count":3,"ee_version":"0.1.0","hostname":null,"export_id":"exp-001","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}"#,
        r#"{"schema":"ee.export.memory.v1","memory_id":"mem_01234567890123456789012345","workspace_id":"wsp_01234567890123456789012345","level":"procedural","kind":"rule","content":"Run cargo fmt --check before release.","importance":0.8,"confidence":0.9,"utility":0.7,"created_at":"2026-04-30T00:00:00Z","updated_at":null,"expires_at":null,"source_agent":"MistySalmon","provenance_uri":"ee-export://fixture","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}"#,
        r#"{"schema":"ee.export.tag.v1","memory_id":"mem_01234567890123456789012345","tag":"Release","created_at":"2026-04-30T00:00:00Z"}"#,
        r#"{"schema":"ee.export.footer.v1","export_id":"exp-001","completed_at":"2026-04-30T00:01:00Z","total_records":4,"memory_count":1,"link_count":0,"tag_count":1,"audit_count":0,"checksum":null,"success":true,"error_message":null}"#,
    ]
    .join("\n");
    fs::write(path, jsonl).map_err(|error| format!("write valid JSONL: {error}"))
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
        &Some(&json!("ee.response.v2")),
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

#[test]
fn import_jsonl_public_response_exposes_and_retries_index_publication_failure() -> TestResult {
    let root = unique_artifact_dir("publication-retry")?;
    let workspace = root.join("workspace with spaces");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let (exit_code, parsed, _stderr) = run_ee(&workspace, &["init"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "init exit code")?;
    ensure(
        parsed.pointer("/success") == Some(&json!(true)),
        format!("init must succeed: {parsed}"),
    )?;

    let index_dir = workspace.join(".ee/index");
    if index_dir.exists() {
        fs::rename(&index_dir, workspace.join(".ee/index-before-import"))
            .map_err(|error| format!("preserve initialized index: {error}"))?;
    }
    let blocked_target = workspace.join(".ee/index-publish-blocker");
    fs::create_dir_all(&blocked_target).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&blocked_target, &index_dir)
        .map_err(|error| format!("install index publication blocker: {error}"))?;

    let source = root.join("source.jsonl");
    write_valid_jsonl(&source)?;
    let (exit_code, failed, stderr) = run_ee(
        &workspace,
        &["import", "jsonl", "--source", path_arg(&source)?],
    )?;
    ensure_equal(
        &exit_code,
        &EXIT_SUCCESS,
        "durable import with derived publication failure exit code",
    )?;
    ensure(
        stderr.is_empty(),
        format!("JSON import must keep stderr empty: {stderr}"),
    )?;
    ensure_equal(
        &failed.pointer("/data/memoriesImported"),
        &Some(&json!(1)),
        "source memory remains durable",
    )?;
    let degradation = failed
        .pointer("/degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.get("code").and_then(Value::as_str) == Some("import_index_publish_failed")
            })
        })
        .ok_or_else(|| format!("response omitted publication degradation: {failed}"))?;
    let workspace_text = path_arg(&workspace)?;
    let expected_repair = format!(
        "ee index rebuild --workspace '{}'",
        workspace_text.replace('\'', "'\\''")
    );
    ensure_equal(
        &degradation.get("severity"),
        &Some(&json!("warning")),
        "publication degradation severity",
    )?;
    ensure_equal(
        &degradation.get("repair"),
        &Some(&json!(expected_repair)),
        "publication degradation exact repair",
    )?;
    let issue = failed
        .pointer("/data/issues")
        .and_then(Value::as_array)
        .and_then(|issues| {
            issues.iter().find(|issue| {
                issue.get("code").and_then(Value::as_str) == Some("import_index_publish_failed")
            })
        })
        .ok_or_else(|| format!("data.issues omitted publication failure: {failed}"))?;
    ensure_equal(
        &issue.get("repair"),
        &degradation.get("repair"),
        "data issue and response degradation share one repair",
    )?;

    let (exit_code, human, stderr) = run_ee_text(
        &workspace,
        &["import", "jsonl", "--source", path_arg(&source)?],
    )?;
    ensure_equal(
        &exit_code,
        &EXIT_SUCCESS,
        "human publication failure exit code",
    )?;
    ensure(
        stderr.is_empty(),
        format!("human import must keep stderr empty: {stderr}"),
    )?;
    ensure(
        human.contains("[warning] import_index_publish_failed")
            && human.contains(&format!("Repair: {expected_repair}")),
        format!("human output omitted publication failure or repair: {human}"),
    )?;

    let (exit_code, toon, stderr) = run_ee_text(
        &workspace,
        &[
            "--format",
            "toon",
            "import",
            "jsonl",
            "--source",
            path_arg(&source)?,
        ],
    )?;
    ensure_equal(
        &exit_code,
        &EXIT_SUCCESS,
        "TOON publication failure exit code",
    )?;
    ensure(
        stderr.is_empty(),
        format!("TOON import must keep stderr empty: {stderr}"),
    )?;
    ensure(
        toon.ends_with('\n'),
        format!("TOON output must end with one record newline: {toon:?}"),
    )?;
    let decoded = toon::try_decode(toon.trim_end_matches('\n'), None)
        .map_err(|error| format!("decode TOON import response: {error}"))?;
    let decoded = Value::from(decoded);
    ensure_equal(
        &decoded.pointer("/schema"),
        &Some(&json!("ee.response.v2")),
        "TOON response envelope schema",
    )?;
    let toon_degradation = decoded
        .pointer("/degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.get("code").and_then(Value::as_str) == Some("import_index_publish_failed")
            })
        })
        .ok_or_else(|| format!("TOON response omitted publication degradation: {decoded}"))?;
    ensure_equal(
        &toon_degradation.get("repair"),
        &Some(&json!(expected_repair)),
        "TOON publication degradation exact repair",
    )?;

    // Keep the failed symlink as evidence. Once the canonical path is free,
    // the same public import must retry its existing deterministic job.
    fs::rename(&index_dir, workspace.join(".ee/index-failed-link"))
        .map_err(|error| format!("preserve failed index symlink: {error}"))?;
    let (exit_code, retried, stderr) = run_ee(
        &workspace,
        &["import", "jsonl", "--source", path_arg(&source)?],
    )?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "reimport retry exit code")?;
    ensure(
        stderr.is_empty(),
        format!("reimport retry must keep stderr empty: {stderr}"),
    )?;
    ensure_equal(
        &retried.pointer("/data/memoriesImported"),
        &Some(&json!(0)),
        "retry does not duplicate the source memory",
    )?;
    ensure_equal(
        &retried.pointer("/data/memoriesSkippedDuplicate"),
        &Some(&json!(1)),
        "retry recognizes the durable source memory",
    )?;
    ensure_equal(
        &retried.pointer("/degraded"),
        &Some(&json!([])),
        "successful retry clears publication degradation",
    )?;

    let (exit_code, status, stderr) = run_ee(&workspace, &["index", "status"])?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "index status exit code")?;
    ensure(
        stderr.is_empty(),
        format!("index status must keep stderr empty: {stderr}"),
    )?;
    ensure_equal(
        &status.pointer("/data/health"),
        &Some(&json!("ready")),
        "retry makes the index ready",
    )?;
    let db_generation = status
        .pointer("/data/dbGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("ready status omitted database generation: {status}"))?;
    let index_generation = status
        .pointer("/data/indexGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("ready status omitted index generation: {status}"))?;
    ensure_equal(
        &index_generation,
        &db_generation,
        "retry publishes the committed database generation",
    )?;

    let (exit_code, search, stderr) = run_ee(
        &workspace,
        &[
            "search",
            "cargo fmt release",
            "--source-mode",
            "lexical-only",
            "--strict-source-mode",
            "--relevance-floor",
            "0",
        ],
    )?;
    ensure_equal(&exit_code, &EXIT_SUCCESS, "post-retry search exit code")?;
    ensure(
        stderr.is_empty(),
        format!("post-retry search must keep stderr empty: {stderr}"),
    )?;
    let results = search
        .pointer("/data/results")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("search response omitted results: {search}"))?;
    ensure(
        results.iter().any(|result| {
            result.get("docId").and_then(Value::as_str) == Some("mem_01234567890123456789012345")
        }),
        format!("retried imported memory is not searchable: {search}"),
    )?;

    // Simulate loss of a derived index after the logical job completed. The
    // next identical import must re-arm that completed job instead of claiming
    // success while the index remains absent.
    fs::rename(
        &index_dir,
        workspace.join(".ee/index-completed-but-missing"),
    )
    .map_err(|error| format!("preserve completed index: {error}"))?;
    let (exit_code, recovered, stderr) = run_ee(
        &workspace,
        &["import", "jsonl", "--source", path_arg(&source)?],
    )?;
    ensure_equal(
        &exit_code,
        &EXIT_SUCCESS,
        "completed-job index recovery exit code",
    )?;
    ensure(
        stderr.is_empty(),
        format!("completed-job recovery must keep stderr empty: {stderr}"),
    )?;
    ensure_equal(
        &recovered.pointer("/data/memoriesImported"),
        &Some(&json!(0)),
        "completed-job recovery remains idempotent",
    )?;
    ensure_equal(
        &recovered.pointer("/degraded"),
        &Some(&json!([])),
        "completed-job recovery restores the index without degradation",
    )?;
    let (exit_code, status, stderr) = run_ee(&workspace, &["index", "status"])?;
    ensure_equal(
        &exit_code,
        &EXIT_SUCCESS,
        "recovered index status exit code",
    )?;
    ensure(
        stderr.is_empty(),
        format!("recovered index status must keep stderr empty: {stderr}"),
    )?;
    let db_generation = status
        .pointer("/data/dbGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("recovered status omitted database generation: {status}"))?;
    let index_generation = status
        .pointer("/data/indexGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("recovered status omitted index generation: {status}"))?;
    ensure(
        status.pointer("/data/health") == Some(&json!("ready"))
            && index_generation == db_generation,
        format!("completed-job recovery did not restore ready equality: {status}"),
    )
}
