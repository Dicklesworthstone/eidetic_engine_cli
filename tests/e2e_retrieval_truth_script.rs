#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn retained_scratch_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "ee-retrieval-truth-script-{}-{nanos}",
        std::process::id()
    ))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<Value>(line).map_err(|error| {
                format!(
                    "{}:{} invalid JSON: {error}: {line}",
                    path.display(),
                    index + 1
                )
            })
        })
        .collect()
}

fn event_log_tail(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|events| {
            events
                .lines()
                .rev()
                .take(18)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|error| format!("unavailable: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn retrieval_truth_script_exercises_public_cli_and_structured_logging() -> TestResult {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ee_bin = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    let scratch = retained_scratch_dir();
    let log_dir = scratch.join("logs");
    let event_log = log_dir.join("events.jsonl");
    fs::create_dir_all(&log_dir)
        .map_err(|error| format!("create retained retrieval-truth log dir: {error}"))?;

    let output = Command::new(repo.join("scripts/e2e_retrieval_truth.sh"))
        .current_dir(&repo)
        .env("EE_BIN", &ee_bin)
        .env("EE_E2E_KEEP", "1")
        .env("EE_E2E_KEEP_ARTIFACTS", "1")
        .env("EE_E2E_TMPDIR", &scratch)
        .env("LOG_DIR", &log_dir)
        .env("EE_TEST_LOG_PATH", &event_log)
        .output()
        .map_err(|error| format!("run scripts/e2e_retrieval_truth.sh: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "scripts/e2e_retrieval_truth.sh failed with status {:?}\nstdout:\n{}\nstderr:\n{}\nevent_log_tail:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            event_log_tail(&event_log),
        ),
    )?;

    let summary = read_json(&log_dir.join("summary.json"))?;
    ensure(
        summary["schema"] == "ee.test_event.v1.summary",
        format!("bad summary schema: {summary}"),
    )?;
    ensure(
        summary["test"] == "retrieval_truth",
        format!("bad test name: {summary}"),
    )?;
    ensure(
        summary["verdict"] == "PASS",
        format!("bad verdict: {summary}"),
    )?;
    ensure(
        summary["fail"] == 0,
        format!("unexpected failures: {summary}"),
    )?;
    ensure(
        summary["pass"].as_u64().unwrap_or(0) >= 35,
        format!("retrieval-truth script should record detailed assertions: {summary}"),
    )?;

    let events = read_jsonl(&event_log)?;
    let mut command_end_count = 0usize;
    let mut assert_fail_count = 0usize;
    let mut saw_index_posture_note = false;
    let mut saw_doctor_note = false;
    let mut saw_score_note = false;
    let mut saw_ee_artifact_manifest = false;

    for event in &events {
        ensure(
            event["schema"] == "ee.test_event.v1",
            format!("unexpected event schema: {event}"),
        )?;
        match event["kind"].as_str().unwrap_or_default() {
            "command_end" => {
                command_end_count = command_end_count.saturating_add(1);
            }
            "assert_fail" => {
                assert_fail_count = assert_fail_count.saturating_add(1);
            }
            "note" => {
                let message = event
                    .pointer("/fields/message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                saw_index_posture_note |= message.contains("posture_index mode=");
                saw_doctor_note |= message.contains("doctor_embedding_message=");
                saw_score_note |= message.contains("top_search_score query=");
            }
            "artifact_manifest" => {
                let binary_path = event
                    .pointer("/fields/binary_path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let binary_hash_status = event
                    .pointer("/fields/binary_hash_status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                saw_ee_artifact_manifest |= binary_path == ee_bin.to_string_lossy().as_ref()
                    && binary_hash_status == "available";
            }
            _ => {}
        }
    }

    ensure(
        command_end_count >= 8,
        format!("expected at least 8 command_end events, got {command_end_count}"),
    )?;
    ensure(
        assert_fail_count == 0,
        format!("retrieval-truth script emitted {assert_fail_count} assert_fail events"),
    )?;
    ensure(
        saw_index_posture_note,
        "event log missing posture_index note",
    )?;
    ensure(
        saw_doctor_note,
        "event log missing doctor embedding trap note",
    )?;
    ensure(saw_score_note, "event log missing top search score note")?;
    ensure(
        saw_ee_artifact_manifest,
        "event log missing available ee binary artifact manifest",
    )
}
