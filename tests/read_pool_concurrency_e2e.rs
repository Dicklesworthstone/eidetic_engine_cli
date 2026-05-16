#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("ee-{name}-{}-{stamp}", std::process::id()))
}

fn env_or_default(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn read_pool_script_source() -> Result<String, String> {
    fs::read_to_string(
        repo_root()
            .join("scripts")
            .join("e2e_overhaul")
            .join("read_pool_concurrency.sh"),
    )
    .map_err(|error| format!("read read-pool e2e script: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn strict_speedup_gate_requires_in_process_harness() -> TestResult {
    let script = read_pool_script_source()?;
    for required in [
        "READ_POOL_HARNESS_MODE=\"process_fanout_cli\"",
        "process_fanout_cli cannot prove process-local read-pool speedup",
        "read_pool_strict_speedup_requires_in_process_harness",
        "\"harness_mode\" \"$READ_POOL_HARNESS_MODE\"",
    ] {
        ensure(
            script.contains(required),
            format!("read-pool harness strict-gate contract missing: {required}"),
        )?;
    }
    Ok(())
}

#[test]
fn read_pool_concurrency_script_smoke_emits_valid_events() -> TestResult {
    let run_dir = unique_temp_dir("read-pool-concurrency");
    fs::create_dir_all(&run_dir)
        .map_err(|error| format!("create run dir {}: {error}", run_dir.display()))?;
    let event_log = run_dir.join("read_pool_concurrency.jsonl");

    let output = Command::new("bash")
        .arg(
            repo_root()
                .join("scripts")
                .join("e2e_overhaul")
                .join("read_pool_concurrency.sh"),
        )
        .current_dir(repo_root())
        .env("EE_BINARY", ee_binary())
        .env("EE_TEST_LOG_PATH", &event_log)
        .env("EE_E2E_TMPDIR", &run_dir)
        .env(
            "EE_READ_POOL_CONTEXTS",
            env_or_default("EE_READ_POOL_CONTEXTS", "2"),
        )
        .env(
            "EE_READ_POOL_WRITERS",
            env_or_default("EE_READ_POOL_WRITERS", "1"),
        )
        .env(
            "EE_READ_POOL_CORPUS_SIZE",
            env_or_default("EE_READ_POOL_CORPUS_SIZE", "24"),
        )
        .env(
            "EE_READ_POOL_ENFORCE_SPEEDUP",
            env_or_default("EE_READ_POOL_ENFORCE_SPEEDUP", "0"),
        )
        .output()
        .map_err(|error| format!("run read-pool e2e: {error}"))?;

    let event_log_excerpt = fs::read_to_string(&event_log)
        .map(|events| {
            events
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|error| format!("unavailable: {error}"));

    ensure(
        output.status.success(),
        format!(
            "read-pool e2e failed: stdout={} stderr={} event_log_tail={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            event_log_excerpt
        ),
    )?;

    let events = fs::read_to_string(&event_log)
        .map_err(|error| format!("read event log {}: {error}", event_log.display()))?;
    let mut phases = std::collections::BTreeSet::new();
    let mut assert_failures = Vec::new();
    let mut read_pool_events = 0usize;
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value =
            serde_json::from_str(line).map_err(|error| format!("parse event: {error}; {line}"))?;
        ensure(
            event["schema"] == "ee.test_event.v1",
            format!("bad event schema: {event}"),
        )?;
        if event["kind"] == "read_pool_concurrency_e2e" {
            read_pool_events = read_pool_events.saturating_add(1);
            let fields = event
                .get("fields")
                .ok_or_else(|| format!("read-pool event missing fields: {event}"))?;
            ensure(
                fields["bead_id"] == "bd-2caru.5",
                format!("bad bead id: {event}"),
            )?;
            ensure(
                fields["surface"] == "read_pool",
                format!("bad surface: {event}"),
            )?;
            if let Some(phase) = fields["phase"].as_str() {
                phases.insert(phase.to_owned());
            }
        }
        if event["kind"] == "assert_fail" {
            assert_failures.push(event.to_string());
        }
    }

    if env_or_default("EE_READ_POOL_STRICT_ASSERTS", "0") == "1" {
        ensure(
            assert_failures.is_empty(),
            format!("script emitted assert_fail events: {assert_failures:?}"),
        )?;
    }
    ensure(read_pool_events > 0, "script emitted no read-pool events")?;
    for required in ["setup", "config", "context", "perf", "summary"] {
        ensure(
            phases.contains(required),
            format!("missing event phase {required}; phases={phases:?}"),
        )?;
    }
    Ok(())
}
