//! Real-binary trust freshness pin tests.
//!
//! These tests retain their temporary workspaces so a failing central verify
//! leaves enough evidence for follow-up without relying on cleanup.

use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn workspace_dir() -> Result<String, String> {
    let mut root = std::env::var("EE_E2E_TMPDIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| {
            if Path::new("/private/tmp").is_dir() {
                "/private/tmp".to_string()
            } else {
                "/tmp".to_string()
            }
        });
    if root.starts_with("/Volumes/") {
        root = if Path::new("/private/tmp").is_dir() {
            "/private/tmp".to_string()
        } else {
            "/tmp".to_string()
        };
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX epoch: {error}"))?
        .as_nanos();
    let path = format!(
        "{}/ee-trust-freshness-e2e-{}-{nanos}",
        root.trim_end_matches('/'),
        std::process::id()
    );
    fs::create_dir_all(&path)
        .map_err(|error| format!("failed to create retained workspace {path}: {error}"))?;
    Ok(path)
}

fn log_event(kind: &str, fields: Value) {
    eprintln!(
        "{}",
        json!({
            "schema": "ee.test_event.v1",
            "test": "trust_freshness_e2e",
            "kind": kind,
            "fields": fields,
        })
    );
}

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    log_event(
        "command_start",
        json!({
            "args": args,
            "workspace": workspace,
        }),
    );
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))?;
    log_event(
        "command_end",
        json!({
            "args": args,
            "exitCode": output.status.code(),
            "stdoutBytes": output.stdout.len(),
            "stderrBytes": output.stderr.len(),
            "elapsedMs": started.elapsed().as_millis(),
        }),
    );
    Ok(output)
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    let message = message.into();
    log_event(
        "assertion",
        json!({
            "message": message,
            "passed": condition,
        }),
    );
    if condition { Ok(()) } else { Err(message) }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    ensure(
        actual == expected,
        format!("{context}: expected {expected:?}, got {actual:?}"),
    )
}

fn stdout_json(output: &Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn assert_success_json(output: &Output, label: &str) -> Result<Value, String> {
    ensure_equal(&output.status.code(), &Some(0), &format!("{label} exit"))?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "{label} stderr must be empty in JSON mode: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let value = stdout_json(output, label)?;
    ensure_equal(
        &value["schema"],
        &json!("ee.response.v2"),
        &format!("{label} response schema"),
    )?;
    ensure_equal(&value["success"], &json!(true), &format!("{label} success"))?;
    Ok(value)
}

fn memory_id(value: &Value, label: &str) -> Result<String, String> {
    value
        .pointer("/data/memory_id")
        .or_else(|| value.pointer("/data/memoryId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{label}: missing memory id"))
}

fn has_degraded_code(value: &Value, code: &str) -> bool {
    value
        .pointer("/data/degraded")
        .and_then(Value::as_array)
        .is_some_and(|degraded| {
            degraded
                .iter()
                .any(|entry| entry.get("code").and_then(Value::as_str) == Some(code))
        })
}

fn provenance_degraded_count(value: &Value) -> usize {
    value
        .pointer("/data/degraded")
        .and_then(Value::as_array)
        .map(|degraded| {
            degraded
                .iter()
                .filter(|entry| {
                    entry
                        .get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| code.starts_with("why_provenance_freshness_"))
                })
                .count()
        })
        .unwrap_or(0)
}

fn write_probe(path: &Path, marker: &str) -> Result<(), String> {
    fs::write(
        path,
        format!("pub fn trust_probe() -> &'static str {{\n    \"{marker}\"\n}}\n"),
    )
    .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[test]
fn why_verify_and_drift_pin_trust_freshness_transitions() -> TestResult {
    let workspace = workspace_dir()?;
    log_event("workspace", json!({ "path": workspace }));
    fs::create_dir_all(Path::new(&workspace).join("src"))
        .map_err(|error| format!("failed to create src dir: {error}"))?;
    let source = Path::new(&workspace).join("src/trust_probe.rs");
    let moved = Path::new(&workspace).join("src/trust_probe_moved.rs");
    write_probe(&source, "trusted-freshness-rust-v1")?;

    let init = run_ee(&workspace, &["init", "--json"])?;
    let _init_json = assert_success_json(&init, "init")?;

    let remember = run_ee(
        &workspace,
        &[
            "remember",
            "trusted-freshness-rust-v1",
            "--level",
            "episodic",
            "--kind",
            "fact",
            "--source",
            "file://src/trust_probe.rs#L2-L2",
            "--json",
        ],
    )?;
    let remember_json = assert_success_json(&remember, "remember file provenance")?;
    let file_memory_id = memory_id(&remember_json, "remember file provenance")?;
    log_event(
        "memory_created",
        json!({
            "memoryId": file_memory_id,
            "provenance": "file://src/trust_probe.rs#L2-L2",
        }),
    );

    let cass = run_ee(
        &workspace,
        &[
            "remember",
            "cass-backed trust freshness rust pointer",
            "--level",
            "episodic",
            "--kind",
            "fact",
            "--source",
            "cass-session://trust-freshness-rust-fixture#L1-L2",
            "--json",
        ],
    )?;
    let cass_json = assert_success_json(&cass, "remember cass provenance")?;
    let cass_memory_id = memory_id(&cass_json, "remember cass provenance")?;

    let why_cass = run_ee(&workspace, &["why", &cass_memory_id, "--json"])?;
    let why_cass_json = assert_success_json(&why_cass, "why cass provenance")?;
    ensure(
        has_degraded_code(&why_cass_json, "why_provenance_freshness_unverifiable"),
        "cass provenance is unverifiable while cass verifier is absent",
    )?;
    ensure(
        !has_degraded_code(&why_cass_json, "why_provenance_freshness_missing"),
        "cass provenance must not be misclassified as missing",
    )?;

    let why_present = run_ee(&workspace, &["why", &file_memory_id, "--json"])?;
    let why_present_json = assert_success_json(&why_present, "why present provenance")?;
    ensure_equal(
        &provenance_degraded_count(&why_present_json),
        &0,
        "present provenance degraded count",
    )?;

    fs::rename(&source, &moved).map_err(|error| {
        format!(
            "failed to move {} to {}: {error}",
            source.display(),
            moved.display()
        )
    })?;
    log_event(
        "transition",
        json!({ "memoryId": file_memory_id, "state": "moved" }),
    );
    let why_moved = run_ee(&workspace, &["why", &file_memory_id, "--json"])?;
    let why_moved_json = assert_success_json(&why_moved, "why moved provenance")?;
    ensure(
        has_degraded_code(&why_moved_json, "why_provenance_freshness_moved"),
        "moved provenance reports moved degradation",
    )?;

    fs::rename(&moved, &source).map_err(|error| {
        format!(
            "failed to restore {} to {}: {error}",
            moved.display(),
            source.display()
        )
    })?;
    log_event(
        "transition",
        json!({ "memoryId": file_memory_id, "state": "restored" }),
    );
    let why_restored = run_ee(&workspace, &["why", &file_memory_id, "--json"])?;
    let why_restored_json = assert_success_json(&why_restored, "why restored provenance")?;
    ensure_equal(
        &provenance_degraded_count(&why_restored_json),
        &0,
        "restored provenance degraded count",
    )?;

    write_probe(&source, "trusted-freshness-rust-v2")?;
    log_event(
        "transition",
        json!({ "memoryId": file_memory_id, "state": "content_changed" }),
    );
    let why_missing = run_ee(&workspace, &["why", &file_memory_id, "--json"])?;
    let why_missing_json = assert_success_json(&why_missing, "why changed provenance")?;
    ensure(
        has_degraded_code(&why_missing_json, "why_provenance_freshness_missing"),
        "changed provenance reports missing/mismatched degradation",
    )?;

    let verify = run_ee(&workspace, &["verify", "provenance", "--json"])?;
    let verify_json = assert_success_json(&verify, "verify provenance")?;
    ensure(
        verify_json
            .pointer("/data/referents")
            .and_then(Value::as_array)
            .is_some_and(|referents| {
                referents.iter().any(|referent| {
                    referent.get("memoryId").and_then(Value::as_str)
                        == Some(file_memory_id.as_str())
                        && matches!(
                            referent.get("status").and_then(Value::as_str),
                            Some("evidence_drift" | "evidence_missing")
                        )
                })
            }),
        "verify provenance classifies changed file evidence",
    )?;
    ensure(
        verify_json
            .pointer("/data/referents")
            .and_then(Value::as_array)
            .is_some_and(|referents| {
                referents.iter().any(|referent| {
                    referent.get("memoryId").and_then(Value::as_str)
                        == Some(cass_memory_id.as_str())
                        && referent.get("status").and_then(Value::as_str) == Some("unverifiable")
                })
            }),
        "verify provenance keeps cass evidence unverifiable",
    )?;
    ensure(
        verify_json
            .pointer("/data/auditCount")
            .and_then(Value::as_u64)
            .is_some_and(|count| count >= 1),
        "verify provenance records audit evidence",
    )?;
    ensure(
        verify_json
            .pointer("/data/mutationCount")
            .and_then(Value::as_u64)
            .is_some_and(|count| count >= 1),
        "verify provenance records trust mutation evidence",
    )?;

    let drift = run_ee(&workspace, &["memory", "drift", &file_memory_id, "--json"])?;
    ensure_equal(&drift.status.code(), &Some(0), "memory drift exit")?;
    ensure(
        drift.stderr.is_empty(),
        format!(
            "memory drift stderr must be empty in JSON mode: {}",
            String::from_utf8_lossy(&drift.stderr)
        ),
    )?;
    let drift_json = stdout_json(&drift, "memory drift")?;
    ensure_equal(
        &drift_json["schema"],
        &json!("ee.memory_drift.report.v1"),
        "memory drift schema",
    )?;
    ensure(
        drift_json
            .pointer("/items")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("memoryId").and_then(Value::as_str) == Some(file_memory_id.as_str())
                        && matches!(
                            item.get("driftStatus").and_then(Value::as_str),
                            Some("changed" | "missing_source" | "unverifiable")
                        )
                })
            }),
        "memory drift reports affected provenance state",
    )?;

    Ok(())
}
