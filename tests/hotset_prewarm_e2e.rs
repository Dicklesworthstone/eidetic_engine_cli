//! Real-binary e2e coverage for hotset prewarm event logging.
//!
//! The shell harness keeps a static validation path available, while this
//! integration test runs it with Cargo's test-built `ee` binary under RCH.

#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn hotset_prewarm_script_emits_event_log_and_preserves_semantics() -> TestResult {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = repo.join("scripts").join("e2e_hotset_prewarm.sh");
    let output = Command::new("bash")
        .arg(&script)
        .env("EE_BINARY", env!("CARGO_BIN_EXE_ee"))
        .env("EE_E2E_KEEP_WORKSPACE", "1")
        .output()
        .map_err(|error| format!("failed to run {}: {error}", script.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        format!(
            "hotset prewarm script failed: status={:?}; stdout={stdout}; stderr={stderr}",
            output.status.code()
        ),
    )?;

    let event_log = stderr
        .lines()
        .find_map(|line| line.strip_prefix("hotset prewarm e2e passed; events="))
        .ok_or_else(|| format!("script stderr did not report event log path: {stderr}"))?;
    let event_text = fs::read_to_string(event_log)
        .map_err(|error| format!("failed to read event log {event_log}: {error}"))?;
    let events = event_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;

    ensure(
        events.len() >= 9,
        format!("expected at least 9 events: {event_text}"),
    )?;
    ensure(
        events
            .iter()
            .all(|event| event["schema"].as_str() == Some("ee.test_event.v1")),
        "all events must use ee.test_event.v1",
    )?;
    ensure(
        events
            .iter()
            .any(|event| event["kind"].as_str() == Some("cache_prewarm")),
        "event log must include cache_prewarm command",
    )?;
    ensure(
        events
            .iter()
            .any(|event| event["kind"].as_str() == Some("hotset_prewarm_summary")),
        "event log must include final semantic summary",
    )?;
    ensure(
        events.iter().all(|event| {
            event["sourceSnapshotHash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("sha256:"))
                && event["manifestHash"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("sha256:"))
        }),
        "events must carry source snapshot and manifest hashes",
    )?;
    let summary = events
        .iter()
        .find(|event| event["kind"].as_str() == Some("hotset_prewarm_summary"))
        .ok_or_else(|| "summary event missing".to_owned())?;
    ensure(
        summary
            .pointer("/warmedItemCounts/admitted")
            .and_then(Value::as_u64)
            .is_some_and(|count| count >= 4),
        format!("summary must report admitted warmed items: {summary}"),
    )?;
    ensure(
        summary
            .pointer("/sampleSummary/p50Ms")
            .and_then(Value::as_u64)
            .is_some(),
        format!("summary must report p50 sample latency: {summary}"),
    )?;
    ensure(
        summary["firstFailureDiagnosis"].is_null(),
        format!("successful summary must not report first failure: {summary}"),
    )?;
    ensure(
        !event_text.contains("Hotset prewarm release workflow rule"),
        "event log must not leak raw remembered content",
    )
}
