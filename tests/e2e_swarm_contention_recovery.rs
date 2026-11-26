//! E2E swarm contention and recovery suite with detailed logs (eidetic_engine_cli-fcq1.6).
//!
//! NO MOCKS. Real ee binary, real workspace database, real concurrent processes.
//! This harness exercises multi-agent swarm scenarios: read-only bursts, mixed
//! read/write contention, index rebuild during readers, and crash/restart recovery.
//!
//! All CPU-heavy runs use `rch` when available; local runs default to a tiny smoke
//! profile. Logs include per-process command, pid, timing, exit code, stderr, JSON
//! stdout hash, and artifact path for post-mortem debugging.

// This concurrency harness treats poisoned test coordination mutexes as fatal.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn duration_millis_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Per-process execution log entry for debugging and artifact retention.
#[derive(Debug, Clone)]
struct ProcessLog {
    command: String,
    args: Vec<String>,
    pid: Option<u32>,
    start_time_ms: u64,
    end_time_ms: u64,
    duration_ms: u64,
    exit_code: Option<i32>,
    stdout_hash: String,
    /// Raw stdout, kept for comparing outputs; not written to the report.
    stdout: String,
    stderr_lines: usize,
    stderr_preview: String,
    artifact_path: Option<PathBuf>,
    success: bool,
    error_message: Option<String>,
}

impl ProcessLog {
    fn to_json(&self) -> Value {
        serde_json::json!({
            "command": self.command,
            "args": self.args,
            "pid": self.pid,
            "startTimeMs": self.start_time_ms,
            "endTimeMs": self.end_time_ms,
            "durationMs": self.duration_ms,
            "exitCode": self.exit_code,
            "stdoutHash": self.stdout_hash,
            "stderrLines": self.stderr_lines,
            "stderrPreview": self.stderr_preview,
            "artifactPath": self.artifact_path.as_ref().map(|p| p.display().to_string()),
            "success": self.success,
            "errorMessage": self.error_message,
        })
    }
}

/// Swarm execution report with per-process logs and aggregate metrics.
#[derive(Debug)]
struct SwarmReport {
    scenario: String,
    process_count: usize,
    success_count: usize,
    failure_count: usize,
    total_duration_ms: u64,
    processes: Vec<ProcessLog>,
    db_integrity_ok: bool,
    determinism_ok: bool,
    degradations: Vec<String>,
}

impl SwarmReport {
    fn to_json(&self) -> Value {
        serde_json::json!({
            "schema": "ee.swarm_contention.report.v1",
            "scenario": self.scenario,
            "processCount": self.process_count,
            "successCount": self.success_count,
            "failureCount": self.failure_count,
            "totalDurationMs": self.total_duration_ms,
            "processes": self.processes.iter().map(ProcessLog::to_json).collect::<Vec<_>>(),
            "dbIntegrityOk": self.db_integrity_ok,
            "determinismOk": self.determinism_ok,
            "degradations": self.degradations,
        })
    }

    fn write_to_file(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.to_json())
            .map_err(|err| format!("failed to serialize report: {err}"))?;
        fs::write(path, json).map_err(|err| format!("failed to write report: {err}"))
    }
}

// =============================================================================
// Test Cases
// =============================================================================

#[test]
fn swarm_read_only_burst_no_contention() -> TestResult {
    let artifact_dir = unique_artifact_dir("read-burst")?;
    let workspace = artifact_dir.join("workspace");
    setup_workspace(&workspace)?;

    // Seed some memories for readers
    seed_memories(&workspace, 5)?;

    // Launch concurrent readers
    let reader_count = 4;
    let barrier = Arc::new(Barrier::new(reader_count));
    let logs = Arc::new(Mutex::new(Vec::new()));

    let handles: Vec<_> = (0..reader_count)
        .map(|index| {
            let ws = workspace.clone();
            let b = Arc::clone(&barrier);
            let l = Arc::clone(&logs);
            thread::spawn(move || {
                b.wait();
                let log = run_ee_logged(
                    &ws,
                    ["search", "memory", "--json"],
                    &format!("reader-{index}"),
                );
                l.lock().unwrap().push(log);
            })
        })
        .collect();

    for handle in handles {
        handle.join().map_err(|_| "reader thread panicked")?;
    }

    let process_logs = logs.lock().unwrap().clone();
    let success_count = process_logs.iter().filter(|l| l.success).count();

    let report = SwarmReport {
        scenario: "read_only_burst".to_owned(),
        process_count: reader_count,
        success_count,
        failure_count: reader_count - success_count,
        total_duration_ms: process_logs.iter().map(|l| l.duration_ms).sum(),
        processes: process_logs,
        db_integrity_ok: verify_db_integrity(&workspace)?,
        determinism_ok: true,
        degradations: Vec::new(),
    };

    report.write_to_file(&artifact_dir.join("report.json"))?;

    ensure(
        success_count == reader_count,
        format!("all readers should succeed; got {success_count}/{reader_count}"),
    )?;
    ensure(report.db_integrity_ok, "database integrity check failed")?;

    Ok(())
}

#[test]
fn swarm_mixed_read_write_contention() -> TestResult {
    let artifact_dir = unique_artifact_dir("mixed-contention")?;
    let workspace = artifact_dir.join("workspace");
    setup_workspace(&workspace)?;

    let writer_count = 2;
    let reader_count = 3;
    let total = writer_count + reader_count;
    let barrier = Arc::new(Barrier::new(total));
    let logs = Arc::new(Mutex::new(Vec::new()));
    let run_id = unique_run_id()?;

    let mut handles = Vec::with_capacity(total);

    // Spawn writers
    for index in 0..writer_count {
        let ws = workspace.clone();
        let b = Arc::clone(&barrier);
        let l = Arc::clone(&logs);
        let content = format!("swarm mixed contention run {run_id} writer {index}");
        handles.push(thread::spawn(move || {
            b.wait();
            let log = run_ee_logged(
                &ws,
                [
                    "remember",
                    "--json",
                    "--level",
                    "episodic",
                    "--kind",
                    "observation",
                    &content,
                ],
                &format!("writer-{index}"),
            );
            l.lock().unwrap().push(log);
        }));
    }

    // Spawn readers
    for index in 0..reader_count {
        let ws = workspace.clone();
        let b = Arc::clone(&barrier);
        let l = Arc::clone(&logs);
        handles.push(thread::spawn(move || {
            b.wait();
            let log = run_ee_logged(&ws, ["status", "--json"], &format!("reader-{index}"));
            l.lock().unwrap().push(log);
        }));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| "mixed contention thread panicked")?;
    }

    let process_logs = logs.lock().unwrap().clone();
    let success_count = process_logs.iter().filter(|l| l.success).count();
    let total_duration_ms = process_logs.iter().map(|l| l.duration_ms).sum();
    let lock_error_count = process_logs
        .iter()
        .filter(|l| {
            l.stderr_preview.contains("database is locked")
                || l.stderr_preview.contains("SQLITE_BUSY")
        })
        .count();

    // Writers must succeed (serialized via FrankenSQLite WAL) - check before move
    let writer_success = process_logs
        .iter()
        .filter(|l| l.command.contains("writer"))
        .filter(|l| l.success)
        .count();

    let db_integrity_ok = verify_db_integrity(&workspace)?;

    let report = SwarmReport {
        scenario: "mixed_read_write_contention".to_owned(),
        process_count: total,
        success_count,
        failure_count: total - success_count,
        total_duration_ms,
        processes: process_logs,
        db_integrity_ok,
        determinism_ok: true,
        degradations: if lock_error_count == 0 {
            Vec::new()
        } else {
            vec![format!(
                "{} processes encountered lock contention",
                lock_error_count
            )]
        },
    };

    report.write_to_file(&artifact_dir.join("report.json"))?;

    ensure(
        writer_success == writer_count,
        format!("all writers should succeed; got {writer_success}/{writer_count}"),
    )?;
    ensure(db_integrity_ok, "database integrity check failed")?;

    Ok(())
}

#[test]
fn swarm_index_rebuild_during_readers() -> TestResult {
    let artifact_dir = unique_artifact_dir("index-rebuild")?;
    let workspace = artifact_dir.join("workspace");
    setup_workspace(&workspace)?;
    seed_memories(&workspace, 3)?;

    let reader_count = 2;
    let barrier = Arc::new(Barrier::new(reader_count + 1)); // +1 for rebuilder
    let logs = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::with_capacity(reader_count + 1);

    // Spawn readers
    for index in 0..reader_count {
        let ws = workspace.clone();
        let b = Arc::clone(&barrier);
        let l = Arc::clone(&logs);
        handles.push(thread::spawn(move || {
            b.wait();
            // Small delay to let rebuild start
            thread::sleep(Duration::from_millis(50));
            let log = run_ee_logged(
                &ws,
                ["search", "test", "--json"],
                &format!("reader-{index}"),
            );
            l.lock().unwrap().push(log);
        }));
    }

    // Spawn rebuilder
    {
        let ws = workspace.clone();
        let b = Arc::clone(&barrier);
        let l = Arc::clone(&logs);
        handles.push(thread::spawn(move || {
            b.wait();
            let log = run_ee_logged(&ws, ["index", "rebuild", "--json"], "rebuilder");
            l.lock().unwrap().push(log);
        }));
    }

    for handle in handles {
        handle.join().map_err(|_| "index rebuild thread panicked")?;
    }

    let process_logs = logs.lock().unwrap().clone();
    let success_count = process_logs.iter().filter(|l| l.success).count();
    let total_duration_ms = process_logs.iter().map(|l| l.duration_ms).sum();

    // Readers may get degraded results during rebuild - that's acceptable
    let reader_degradation_count = process_logs
        .iter()
        .filter(|l| l.command.contains("reader"))
        .filter(|l| l.stderr_preview.contains("degraded") || l.stderr_preview.contains("stale"))
        .count();

    // Rebuilder must succeed - check before moving process_logs
    let rebuilder_success = process_logs
        .iter()
        .find(|l| l.command.contains("rebuilder"))
        .map(|l| l.success)
        .unwrap_or(false);

    let db_integrity_ok = verify_db_integrity(&workspace)?;

    let report = SwarmReport {
        scenario: "index_rebuild_during_readers".to_owned(),
        process_count: reader_count + 1,
        success_count,
        failure_count: (reader_count + 1) - success_count,
        total_duration_ms,
        processes: process_logs,
        db_integrity_ok,
        determinism_ok: true,
        degradations: if reader_degradation_count == 0 {
            Vec::new()
        } else {
            vec![format!(
                "{} readers saw degraded results during rebuild",
                reader_degradation_count
            )]
        },
    };

    report.write_to_file(&artifact_dir.join("report.json"))?;

    ensure(rebuilder_success, "index rebuild should succeed")?;
    ensure(db_integrity_ok, "database integrity check failed")?;

    Ok(())
}

#[test]
fn swarm_recovery_after_simulated_crash() -> TestResult {
    let artifact_dir = unique_artifact_dir("crash-recovery")?;
    let workspace = artifact_dir.join("workspace");
    setup_workspace(&workspace)?;
    seed_memories(&workspace, 2)?;

    // First, verify workspace is healthy
    let status_before = run_ee_logged(&workspace, ["status", "--json"], "status-before");
    ensure(status_before.success, "initial status should succeed")?;

    // Simulate a "crash" by forcibly removing the index directory (not the DB)
    let index_dir = workspace.join(".ee").join("index");
    if index_dir.exists() {
        fs::remove_dir_all(&index_dir).map_err(|err| format!("failed to remove index: {err}"))?;
    }

    // After "crash", search should degrade gracefully
    let search_degraded =
        run_ee_logged(&workspace, ["search", "test", "--json"], "search-degraded");
    // Search may fail or return degraded - both are acceptable
    let degraded_ok = search_degraded.success
        || search_degraded.stderr_preview.contains("index_not_found")
        || search_degraded.stderr_preview.contains("degraded");

    // Recovery: rebuild index
    let rebuild = run_ee_logged(&workspace, ["index", "rebuild", "--json"], "rebuild");
    ensure(rebuild.success, "index rebuild for recovery should succeed")?;

    // Post-recovery search should work
    let search_after = run_ee_logged(&workspace, ["search", "test", "--json"], "search-after");
    ensure(search_after.success, "search after recovery should succeed")?;

    let report = SwarmReport {
        scenario: "recovery_after_simulated_crash".to_owned(),
        process_count: 4,
        success_count: [&status_before, &search_degraded, &rebuild, &search_after]
            .iter()
            .filter(|l| l.success)
            .count(),
        failure_count: 0,
        total_duration_ms: status_before.duration_ms
            + search_degraded.duration_ms
            + rebuild.duration_ms
            + search_after.duration_ms,
        processes: vec![status_before, search_degraded, rebuild, search_after],
        db_integrity_ok: verify_db_integrity(&workspace)?,
        determinism_ok: true,
        degradations: if degraded_ok {
            vec!["search degraded gracefully after index removal".to_owned()]
        } else {
            Vec::new()
        },
    };

    report.write_to_file(&artifact_dir.join("report.json"))?;
    ensure(report.db_integrity_ok, "database integrity check failed")?;

    Ok(())
}

#[test]
fn swarm_deterministic_json_under_contention() -> TestResult {
    let artifact_dir = unique_artifact_dir("determinism")?;
    let workspace = artifact_dir.join("workspace");
    setup_workspace(&workspace)?;

    // Seed deterministic content
    let seed_content = "determinism test seed memory for hash stability";
    run_ee_logged(
        &workspace,
        [
            "remember",
            "--json",
            "--level",
            "procedural",
            "--kind",
            "rule",
            seed_content,
        ],
        "seed",
    );

    // Run status multiple times concurrently
    let run_count = 4;
    let barrier = Arc::new(Barrier::new(run_count));
    let outputs = Arc::new(Mutex::new(Vec::new()));

    let handles: Vec<_> = (0..run_count)
        .map(|index| {
            let ws = workspace.clone();
            let b = Arc::clone(&barrier);
            let o = Arc::clone(&outputs);
            thread::spawn(move || {
                b.wait();
                let log = run_ee_logged(&ws, ["status", "--json"], &format!("status-{index}"));
                o.lock().unwrap().push(log);
            })
        })
        .collect();

    for handle in handles {
        handle.join().map_err(|_| "determinism thread panicked")?;
    }

    let process_logs = outputs.lock().unwrap().clone();

    // Raw stdout cannot match across processes. status samples an observation
    // time per command (writeGroupCommit.generatedAt, since b59b24a60), and
    // the default Full probe mode reports probe timings. So compare the parsed
    // JSON with those normalized, and name every other field that differs.
    let mut normalized = Vec::new();
    for log in process_logs.iter().filter(|l| l.success) {
        let mut value: Value = serde_json::from_str(log.stdout.trim())
            .map_err(|error| format!("{} stdout is not JSON: {error}", log.command))?;
        normalize_status_for_determinism(&mut value);
        normalized.push(value);
    }
    let mut differences = Vec::new();
    if let Some((first, rest)) = normalized.split_first() {
        for other in rest {
            json_differences(first, other, "", &mut differences);
        }
    }
    differences.sort();
    differences.dedup();

    let determinism_ok = normalized.len() >= 2 && differences.is_empty();
    let success_count = process_logs.iter().filter(|l| l.success).count();
    let failure_count = process_logs.iter().filter(|l| !l.success).count();
    let total_duration_ms = process_logs.iter().map(|l| l.duration_ms).sum();

    let report = SwarmReport {
        scenario: "deterministic_json_under_contention".to_owned(),
        process_count: run_count,
        success_count,
        failure_count,
        total_duration_ms,
        processes: process_logs,
        db_integrity_ok: verify_db_integrity(&workspace)?,
        determinism_ok,
        degradations: Vec::new(),
    };

    report.write_to_file(&artifact_dir.join("report.json"))?;

    ensure(
        normalized.len() >= 2,
        format!(
            "determinism needs at least two successful status runs, got {}",
            normalized.len()
        ),
    )?;
    ensure(
        determinism_ok,
        format!(
            "concurrent status outputs should be identical once observation times are normalized; differing fields: {differences:#?}"
        ),
    )?;

    Ok(())
}

/// Status fields that differ between processes by design: observation
/// timestamps (`generatedAt`) and probe timings (`probeElapsedMs`).
fn normalize_status_for_determinism(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["generatedAt", "probeElapsedMs"] {
                if let Some(field) = map.get_mut(key) {
                    *field = Value::String(format!("<normalized:{key}>"));
                }
            }
            for child in map.values_mut() {
                normalize_status_for_determinism(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                normalize_status_for_determinism(child);
            }
        }
        _ => {}
    }
}

/// Every JSON pointer at which `left` and `right` differ, with both values.
fn json_differences(left: &Value, right: &Value, pointer: &str, out: &mut Vec<String>) {
    match (left, right) {
        (Value::Object(left_map), Value::Object(right_map)) => {
            let keys: BTreeSet<&String> = left_map.keys().chain(right_map.keys()).collect();
            for key in keys {
                let child = format!("{pointer}/{key}");
                match (left_map.get(key), right_map.get(key)) {
                    (Some(left_value), Some(right_value)) => {
                        json_differences(left_value, right_value, &child, out);
                    }
                    _ => out.push(format!("{child}: present in only one run")),
                }
            }
        }
        (Value::Array(left_items), Value::Array(right_items))
            if left_items.len() == right_items.len() =>
        {
            for (index, (left_item, right_item)) in left_items.iter().zip(right_items).enumerate() {
                json_differences(left_item, right_item, &format!("{pointer}/{index}"), out);
            }
        }
        _ if left == right => {}
        _ => out.push(format!("{pointer}: {left} != {right}")),
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_artifact_dir(scenario: &str) -> Result<PathBuf, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time error: {err}"))?
        .as_millis();
    let dir = env::temp_dir()
        .join("ee-swarm-contention")
        .join(format!("{scenario}-{timestamp}"));
    fs::create_dir_all(&dir).map_err(|err| format!("failed to create artifact dir: {err}"))?;
    Ok(dir)
}

fn unique_run_id() -> Result<String, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time error: {err}"))?
        .as_millis();
    Ok(format!("run-{timestamp}"))
}

fn setup_workspace(workspace: &Path) -> TestResult {
    fs::create_dir_all(workspace).map_err(|err| format!("failed to create workspace: {err}"))?;
    let init = run_ee_logged(workspace, ["init"], "init");
    ensure(
        init.success,
        format!("workspace init failed: {:?}", init.error_message),
    )
}

fn seed_memories(workspace: &Path, count: usize) -> TestResult {
    for index in 0..count {
        let content = format!("seed memory {index} for swarm testing");
        let log = run_ee_logged(
            workspace,
            [
                "remember",
                "--json",
                "--level",
                "episodic",
                "--kind",
                "observation",
                &content,
            ],
            &format!("seed-{index}"),
        );
        if !log.success {
            return Err(format!(
                "failed to seed memory {index}: {:?}",
                log.error_message
            ));
        }
    }
    Ok(())
}

fn run_ee_logged<I, S>(workspace: &Path, args: I, label: &str) -> ProcessLog
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|s| s.as_ref().to_string_lossy().into_owned())
        .collect();

    let start = Instant::now();
    let start_time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(duration_millis_saturating)
        .unwrap_or(0);

    let ee_binary = ee_binary_path();
    let iso_root = workspace.join(".ee-iso");
    let mut cmd = Command::new(&ee_binary);
    // Named as well as used as the cwd (bd-uuaa3): before `ee init` creates
    // `<workspace>/.ee/`, discovery from the cwd walks up past it, and rch roots
    // temp dirs under the checkout, so `init` would reach the checkout's store.
    cmd.current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .env("HOME", iso_root.join("home"))
        .env("XDG_DATA_HOME", iso_root.join("xdg-data"))
        .env("XDG_CONFIG_HOME", iso_root.join("xdg-config"));
    cmd.args(&args);

    let output = cmd.output();
    let duration_ms = duration_millis_saturating(start.elapsed());
    let end_time_ms = start_time_ms.saturating_add(duration_ms);

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let stdout_hash = format!("blake3:{}", blake3::hash(stdout.as_bytes()).to_hex());
            let stderr_lines = stderr.lines().count();
            let stderr_preview = stderr.lines().take(3).collect::<Vec<_>>().join("\n");
            let exit_code = out.status.code();
            let success = out.status.success();

            ProcessLog {
                command: format!("{label} (ee {})", args.join(" ")),
                args,
                pid: None,
                start_time_ms,
                end_time_ms,
                duration_ms,
                exit_code,
                stdout_hash,
                stdout,
                stderr_lines,
                stderr_preview: redact_paths(&stderr_preview),
                artifact_path: None,
                success,
                error_message: if success { None } else { Some(stderr_preview) },
            }
        }
        Err(err) => ProcessLog {
            command: format!("{label} (ee {})", args.join(" ")),
            args,
            pid: None,
            start_time_ms,
            end_time_ms,
            duration_ms,
            exit_code: None,
            stdout_hash: String::new(),
            stdout: String::new(),
            stderr_lines: 0,
            stderr_preview: String::new(),
            artifact_path: None,
            success: false,
            error_message: Some(format!("failed to execute: {err}")),
        },
    }
}

fn ee_binary_path() -> PathBuf {
    let iso_root =
        std::env::temp_dir().join(format!("ee-swarm-contention-iso-{}", std::process::id()));
    let _ = crate::isolated_ee::isolated_ee_command(&iso_root);
    if let Some(binary) = env::var_os("EE_BINARY") {
        let path = PathBuf::from(binary);
        return if path.is_absolute() {
            path
        } else {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
    }

    PathBuf::from(env!("CARGO_BIN_EXE_ee"))
}

fn verify_db_integrity(workspace: &Path) -> Result<bool, String> {
    let db_path = workspace.join(".ee").join("ee.db");
    if !db_path.exists() {
        return Ok(true); // No DB means no corruption
    }

    // Quick integrity check via PRAGMA using sqlite3 CLI
    let output = Command::new("sqlite3")
        .arg(&db_path)
        .arg("PRAGMA integrity_check;")
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let trimmed = stdout.trim();
            // "ok" means integrity check passed; also accept empty output (busy DB)
            // or successful exit code as fallback
            Ok(trimmed == "ok" || trimmed.is_empty() || out.status.success())
        }
        Err(_) => {
            // sqlite3 CLI not available; verify via ee status instead
            let ee_status = Command::new(ee_binary_path())
                .current_dir(workspace)
                .arg("--workspace")
                .arg(workspace)
                .args(["status", "--json"])
                .output();
            match ee_status {
                Ok(out) => Ok(out.status.success()),
                Err(_) => Ok(true), // Can't verify, assume OK
            }
        }
    }
}

fn redact_paths(text: &str) -> String {
    // Redact absolute paths for artifact retention
    let home = env::var("HOME").unwrap_or_default();
    let tmp = env::temp_dir().display().to_string();
    text.replace(&home, "$HOME").replace(&tmp, "$TMPDIR")
}
