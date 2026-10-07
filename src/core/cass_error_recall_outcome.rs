//! Positive completion evidence for CASS-derived repair attribution.
//!
//! Absence of a recognized diagnostic is not proof that a command succeeded.
//! Keep unknown, malformed, truncated and still-running outputs out of the
//! helpful-repair store; any explicit failure overrides optimistic summaries.

use serde_json::Value;

use super::{MAX_SCANNED_OUTPUT_BYTES, ToolOutput, bounded, content_text};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ExitReport {
    #[default]
    Absent,
    Success,
    Rejected,
}

/// Inspect every status trailer, not just the first nonempty output line.
/// A malformed status is not equivalent to an absent status, and a later zero
/// must never erase an earlier nonzero result from a multi-command tool call.
fn exit_report(text: &str) -> ExitReport {
    let mut report = ExitReport::Absent;
    for line in text.lines() {
        let lower = line.trim().to_ascii_lowercase();
        let status = [
            "process exited with code",
            "process exited with status",
            "exit code",
            "exit status",
        ]
        .iter()
        .find_map(|prefix| {
            let rest = lower.strip_prefix(*prefix)?;
            if !rest.is_empty() && !rest.starts_with(char::is_whitespace) && !rest.starts_with(':')
            {
                return None;
            }
            Some(rest.trim().trim_start_matches(':').trim())
        });
        let Some(status) = status else {
            continue;
        };
        if status.parse::<i64>() != Ok(0) {
            return ExitReport::Rejected;
        }
        report = ExitReport::Success;
    }
    report
}

pub(super) fn failed(output: &ToolOutput) -> bool {
    output.is_error == Some(true)
        || output.exit_code.is_some_and(|code| code != 0)
        || exit_report(bounded(&output.text)) == ExitReport::Rejected
        || bounded(&output.text).lines().any(|line| {
            let line = line.trim().to_ascii_lowercase();
            line.starts_with("error:")
                || line.starts_with("error[")
                || line.starts_with("test result: failed")
                || line.contains("panicked at")
                || (line.starts_with("test ") && line.ends_with("failed"))
                || (line.starts_with("test result:")
                    && line.split(';').any(|counter| {
                        let Some(count) = counter.trim().strip_suffix(" failed") else {
                            return false;
                        };
                        count.is_empty() || count.bytes().any(|byte| byte != b'0')
                    }))
        })
}

pub(super) fn succeeded(output: &ToolOutput) -> bool {
    // Diagnostic extraction may inspect a prefix, but that prefix can never
    // certify success when a failure or final status could follow the bound.
    if output.text.len() > MAX_SCANNED_OUTPUT_BYTES || output.failed() {
        return false;
    }
    let lower = output.text.to_ascii_lowercase();
    if [
        "process running with session id",
        "command running in background",
        "command is running in the background",
        "command timed out",
        "execution timed out",
        "output truncated",
        "output has been truncated",
        "truncated output",
    ]
    .iter()
    .any(|marker| lower.contains(*marker))
    {
        return false;
    }
    // Claude's explicit is_error=false is a tool-level success observation,
    // including quiet commands. Codex instead carries an exit status, either
    // in metadata or in the execution wrapper. Bare stdout proves neither.
    output.is_error == Some(false)
        || output.exit_code == Some(0)
        || exit_report(&output.text) == ExitReport::Success
}

/// Decode both object-valued and JSON-string-valued Codex execution wrappers.
/// Unknown output shapes are not empty successful commands. Malformed or
/// contradictory status fields veto proof even if the text contains a zero.
pub(super) fn codex_output(raw: &Value) -> Option<ToolOutput> {
    match raw {
        Value::Object(_) => structured_output(raw),
        Value::String(text) => {
            if let Ok(structured) = serde_json::from_str::<Value>(text)
                && structured.is_object()
                && (structured.get("output").is_some() || structured.get("metadata").is_some())
            {
                return structured_output(&structured);
            }
            Some(ToolOutput {
                text: text.clone(),
                ..ToolOutput::default()
            })
        }
        _ => Some(ToolOutput {
            text: content_text(raw)?,
            ..ToolOutput::default()
        }),
    }
}

fn structured_output(raw: &Value) -> Option<ToolOutput> {
    let text = content_text(raw.get("output")?)?;
    let mut invalid = false;
    let mut exit_code = None;
    let metadata = raw.get("metadata");
    if metadata.is_some_and(|value| !value.is_object()) {
        invalid = true;
    }
    for value in [
        raw.get("exit_code"),
        metadata.and_then(|metadata| metadata.get("exit_code")),
    ]
    .into_iter()
    .flatten()
    {
        match value.as_i64() {
            Some(code) => {
                if exit_code.is_some_and(|previous| previous != code) {
                    invalid = true;
                }
                exit_code = Some(code);
            }
            None => invalid = true,
        }
    }
    // Do not derive Codex completion from a generic success/error boolean:
    // execution wrappers must supply their process exit, not merely no error.
    if let Some(value) = raw.get("is_error") {
        invalid |= value.as_bool() != Some(false);
    }
    Some(ToolOutput {
        text,
        is_error: invalid.then_some(true),
        exit_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output(text: &str) -> ToolOutput {
        ToolOutput {
            text: text.to_owned(),
            ..ToolOutput::default()
        }
    }

    #[test]
    fn stdout_without_a_completion_observation_is_not_proof() {
        for text in [
            "",
            "all green",
            "Finished dev profile",
            "test result: ok. 21 passed; 0 failed",
        ] {
            assert!(!succeeded(&output(text)), "{text:?}");
        }
        assert!(succeeded(&ToolOutput {
            is_error: Some(false),
            ..output("")
        }));
        assert!(succeeded(&ToolOutput {
            exit_code: Some(0),
            ..output("")
        }));
    }

    #[test]
    fn execution_status_can_follow_headers_or_a_test_summary() {
        for text in [
            "Exit code 0\nFinished dev profile",
            "Chunk ID: 123\nWall time: 0.3 seconds\nProcess exited with code 0\nFinal output:\nok",
            "test result: ok. 21 passed; 0 failed\nProcess exited with status 0",
            "Final output:\nok\nexit status: 0",
        ] {
            assert!(succeeded(&output(text)), "{text}");
        }
    }

    #[test]
    fn failed_or_malformed_status_overrides_every_positive_signal() {
        for status in [
            "101",
            "-9",
            "",
            "unknown",
            "0 (pending)",
            "99999999999999999999999",
        ] {
            let text =
                format!("test result: ok. 21 passed; 0 failed\nProcess exited with code {status}");
            let result = ToolOutput {
                is_error: Some(false),
                exit_code: Some(0),
                ..output(&text)
            };
            assert!(failed(&result), "{text}");
            assert!(!succeeded(&result));
        }
        assert!(!succeeded(&output("Exit code 1\nExit code 0")));
        assert!(!succeeded(&output("Exit code 0\nExit code 1")));
        assert_eq!(exit_report("exit codebook"), ExitReport::Absent);
    }

    #[test]
    fn counters_and_errors_veto_optimistic_completion() {
        for text in [
            "error: linking with cc failed",
            "error[E0277]: missing bound",
            "thread 'worker' panicked at src/lib.rs",
            "test store::round_trip ... FAILED",
            "test result: FAILED. 21 passed; 1 failed",
            "test result: ok. 21 passed; 100000000000000000000000 failed",
            "test result: ok. 21 passed; 1 failed",
        ] {
            assert!(
                !succeeded(&ToolOutput {
                    exit_code: Some(0),
                    ..output(text)
                }),
                "{text}"
            );
        }
        assert!(succeeded(&ToolOutput {
            exit_code: Some(0),
            ..output("test result: ok. 21 passed; 0 failed")
        }));
    }

    #[test]
    fn partial_background_or_oversized_output_never_proves_a_fix() {
        for text in [
            "Process running with session ID 42",
            "Command running in background with ID: abc",
            "Command timed out after 10000 milliseconds",
            "Warning: output truncated\nExit code 0",
            "Output has been truncated\nExit code 0",
        ] {
            assert!(
                !succeeded(&ToolOutput {
                    is_error: Some(false),
                    exit_code: Some(0),
                    ..output(text)
                }),
                "{text}"
            );
        }
        let exact = "x".repeat(MAX_SCANNED_OUTPUT_BYTES);
        assert!(succeeded(&ToolOutput {
            exit_code: Some(0),
            ..output(&exact)
        }));
        let hidden_failure = format!("{exact}\nExit code 101");
        assert!(!succeeded(&ToolOutput {
            exit_code: Some(0),
            ..output(&hidden_failure)
        }));
    }

    #[test]
    fn codex_object_and_string_wrappers_preserve_output_and_exit() {
        let body = json!({"output": "Finished dev profile", "metadata": {"exit_code": 0}});
        for raw in [body.clone(), Value::String(body.to_string())] {
            let decoded = codex_output(&raw).expect("known wrapper");
            assert_eq!(decoded.text, "Finished dev profile");
            assert_eq!(decoded.exit_code, Some(0));
            assert!(succeeded(&decoded));
        }
        let failed = json!({"output": "linker failed", "exit_code": 1});
        let decoded = codex_output(&failed).expect("failure wrapper");
        assert_eq!(decoded.exit_code, Some(1));
        assert!(!succeeded(&decoded));
    }

    #[test]
    fn malformed_or_conflicting_wrapper_status_never_becomes_success() {
        for raw in [
            json!({"output": "Exit code 0", "metadata": {"exit_code": "0"}}),
            json!({"output": "Exit code 0", "metadata": {"exit_code": null}}),
            json!({"output": "Exit code 0", "metadata": false}),
            json!({"output": "Exit code 0", "exit_code": 1, "metadata": {"exit_code": 0}}),
            json!({"output": "Exit code 0", "exit_code": 0, "is_error": true}),
            json!({"output": "Exit code 0", "exit_code": 0, "is_error": "false"}),
        ] {
            let decoded = codex_output(&raw).expect("text is still diagnostic evidence");
            assert!(!succeeded(&decoded), "{raw}");
        }
        for raw in [
            json!({}),
            json!({"metadata": {"exit_code": 0}}),
            json!({"output": {}, "exit_code": 0}),
            json!({"output": [{"type": "image", "text": "hidden"}], "exit_code": 0}),
        ] {
            assert!(codex_output(&raw).is_none(), "{raw}");
        }
        let no_status = codex_output(&json!({"output": "all green"})).expect("stdout");
        assert!(!succeeded(&no_status));
    }
}
