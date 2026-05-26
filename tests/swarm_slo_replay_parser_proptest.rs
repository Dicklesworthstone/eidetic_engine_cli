//! bd-zyco8: structure-aware parser checks for the swarm SLO replay driver.
//!
//! The replay driver is a shell entry point with an embedded Python JSONL
//! parser, so this proptest target exercises the real script boundary rather
//! than a reimplemented Rust parser.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestCaseError};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
struct ReplayLimits {
    max_input_bytes: usize,
    max_line_bytes: usize,
    max_rows: usize,
}

impl Default for ReplayLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024,
            max_line_bytes: 4 * 1024,
            max_rows: 32,
        }
    }
}

#[derive(Clone, Debug)]
struct InvalidTraceCase {
    trace: String,
    limits: ReplayLimits,
    expected_stderr: &'static str,
}

#[derive(Clone, Debug)]
struct ReplayRun {
    success: bool,
    stdout: String,
    stderr: String,
    replay: String,
    summary: String,
}

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("swarm-slo-replay.sh")
}

fn run_replay(trace: &str, limits: ReplayLimits) -> Result<ReplayRun, TestCaseError> {
    let tempdir = tempfile::tempdir_in("/tmp")
        .map_err(|error| TestCaseError::fail(format!("create tempdir: {error}")))?;
    let input = tempdir.path().join("trace.jsonl");
    let output = tempdir.path().join("replayed.jsonl");
    let summary = tempdir.path().join("summary.json");
    fs::write(&input, trace).map_err(|error| {
        TestCaseError::fail(format!(
            "write replay proptest input {}: {error}",
            input.display()
        ))
    })?;

    let output_result = Command::new(script_path())
        .arg("--input")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--summary")
        .arg(&summary)
        .arg("--verify-determinism")
        .env(
            "EE_SWARM_SLO_REPLAY_MAX_INPUT_BYTES",
            limits.max_input_bytes.to_string(),
        )
        .env(
            "EE_SWARM_SLO_REPLAY_MAX_LINE_BYTES",
            limits.max_line_bytes.to_string(),
        )
        .env("EE_SWARM_SLO_REPLAY_MAX_ROWS", limits.max_rows.to_string())
        .output()
        .map_err(|error| TestCaseError::fail(format!("run swarm-slo-replay.sh: {error}")))?;

    let replay = fs::read_to_string(&output).unwrap_or_default();
    let summary = fs::read_to_string(&summary).unwrap_or_default();
    Ok(ReplayRun {
        success: output_result.status.success(),
        stdout: String::from_utf8_lossy(&output_result.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output_result.stderr).into_owned(),
        replay,
        summary,
    })
}

fn jsonl(value: Value) -> String {
    format!("{value}\n")
}

fn invalid_trace_strategy() -> impl Strategy<Value = InvalidTraceCase> {
    prop_oneof![
        "[A-Za-z0-9 _-]{0,64}".prop_map(|tail| InvalidTraceCase {
            trace: format!("{{\"schema\":\"ee.test_event.v1\",{tail}\n"),
            limits: ReplayLimits::default(),
            expected_stderr: "invalid JSONL",
        }),
        Just(InvalidTraceCase {
            trace: "[\"not\", \"an\", \"object\"]\n".to_owned(),
            limits: ReplayLimits::default(),
            expected_stderr: "not a JSON object",
        }),
        "[A-Za-z0-9 _-]{0,48}".prop_map(|phase| InvalidTraceCase {
            trace: jsonl(json!({"eventIndex": 0, "phase": phase})),
            limits: ReplayLimits::default(),
            expected_stderr: "missing schema",
        }),
        "[A-Za-z0-9 _-]{64,160}".prop_map(|value| InvalidTraceCase {
            trace: jsonl(json!({"schema": "ee.test_event.v1", "oversized": value})),
            limits: ReplayLimits {
                max_line_bytes: 64,
                ..ReplayLimits::default()
            },
            expected_stderr: "exceeds max bytes",
        }),
        (2usize..=8).prop_map(|rows| InvalidTraceCase {
            trace: std::iter::repeat(json!({"schema": "ee.test_event.v1"}).to_string())
                .take(rows)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
            limits: ReplayLimits {
                max_rows: 1,
                ..ReplayLimits::default()
            },
            expected_stderr: "exceeds max rows",
        }),
        "[A-Za-z0-9 _-]{16,96}".prop_map(|value| InvalidTraceCase {
            trace: jsonl(json!({"schema": "ee.test_event.v1", "payload": value})),
            limits: ReplayLimits {
                max_input_bytes: 16,
                ..ReplayLimits::default()
            },
            expected_stderr: "input exceeds max bytes",
        }),
    ]
}

fn unexpected_fields_trace_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(("[a-z]{1,12}", "[A-Za-z0-9 _-]{0,64}"), 0..10).prop_map(|fields| {
        let mut object = serde_json::Map::new();
        object.insert("schema".to_owned(), json!("ee.test_event.v1"));
        object.insert("eventIndex".to_owned(), json!(0));
        object.insert("phase".to_owned(), json!("fuzz"));
        object.insert("kind".to_owned(), json!("note"));
        for (index, (key, value)) in fields.into_iter().enumerate() {
            object.insert(format!("unexpected_{index}_{key}"), json!(value));
        }
        jsonl(Value::Object(object))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn malformed_or_oversized_replay_inputs_fail_loud_without_tracebacks(case in invalid_trace_strategy()) {
        let run = run_replay(&case.trace, case.limits)?;

        prop_assert!(
            !run.success,
            "invalid replay trace unexpectedly succeeded; stdout={} stderr={}",
            run.stdout,
            run.stderr,
        );
        prop_assert!(
            run.stderr.contains("swarm-slo-replay:"),
            "failure must be a stable replay parser error, got stderr={}",
            run.stderr,
        );
        prop_assert!(
            run.stderr.contains(case.expected_stderr),
            "stderr must contain expected marker `{}`; got {}",
            case.expected_stderr,
            run.stderr,
        );
        prop_assert!(
            !run.stderr.contains("Traceback"),
            "parser errors must not expose Python tracebacks: {}",
            run.stderr,
        );
        prop_assert!(
            run.replay.is_empty(),
            "invalid parser input must not emit replay rows: {}",
            run.replay,
        );
    }

    #[test]
    fn replay_parser_accepts_unexpected_fields_without_dropping_original_rows(trace in unexpected_fields_trace_strategy()) {
        let run = run_replay(&trace, ReplayLimits::default())?;

        prop_assert!(
            run.success,
            "valid replay trace with unexpected fields failed; stdout={} stderr={}",
            run.stdout,
            run.stderr,
        );
        prop_assert_eq!(run.replay, trace);
        let summary: Value = serde_json::from_str(&run.summary)
            .map_err(|error| TestCaseError::fail(format!("summary must be JSON: {error}; {}", run.summary)))?;
        prop_assert_eq!(summary["eventCount"].as_u64(), Some(1));
        prop_assert_eq!(summary["schema"].as_str(), Some("ee.swarm_slo.replay.v1"));
    }
}
