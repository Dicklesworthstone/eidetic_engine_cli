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
        || super::structured_error_reports_failure(&output.text)
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
                || runner_report_failed(&line)
        })
}

/// A shell pipeline can exit zero after its test runner failed. Recognize
/// complete runner reports rather than searching arbitrary prose for `failed`.
/// This is a veto only: a passing summary never supplies completion authority.
fn runner_report_failed(line: &str) -> bool {
    let mut body = line.trim();
    if body == "fail" || body.starts_with("fail\t") {
        return true;
    }
    if let Some(rest) = body.strip_prefix("not ok ")
        && rest.split_whitespace().next().is_some_and(decimal_count)
    {
        return true;
    }
    if let Some(fields) = body.strip_prefix("failed (").and_then(|s| s.strip_suffix(')')) {
        return !fields.is_empty()
            && fields.split(',').all(|field| {
                field.trim().split_once('=').is_some_and(|(name, count)| {
                    matches!(name.trim(), "failures" | "errors" | "skipped")
                        && decimal_count(count.trim())
                })
            });
    }
    if body.starts_with("===") {
        if !body.ends_with("===") {
            return false;
        }
        body = body.trim_matches('=').trim();
    }
    for header in ["test suites", "test files", "snapshots", "tests"] {
        if let Some(rest) = body.strip_prefix(header) {
            if let Some(rest) = rest.strip_prefix(':') {
                body = rest.trim();
                break;
            }
            if rest.starts_with(char::is_whitespace) {
                body = rest.trim();
                break;
            }
        }
    }
    if let Some((counts, elapsed)) = body.rsplit_once(" in ") {
        if !runner_duration(elapsed) {
            return false;
        }
        body = counts.trim_end();
    }
    if let Some((counts, total)) = body.rsplit_once('(') {
        if !total.strip_suffix(')').is_some_and(|total| decimal_count(total.trim())) {
            return false;
        }
        body = counts.trim_end();
    }
    let mut failure = false;
    for field in body.split([',', ';', '|']) {
        let mut words = field.split_whitespace();
        let Some(count) = words.next().filter(|count| decimal_count(count)) else {
            return false;
        };
        let Some(outcome) = words.next() else {
            return false;
        };
        let failed = match outcome {
            "failed" | "failing" | "fail" | "failure" | "failures" | "error" | "errors" => true,
            "passed" | "passing" | "pass" | "skipped" | "pending" | "todo" | "total"
            | "ignored" | "deselected" | "warning" | "warnings" => false,
            _ => return false,
        };
        if words.next().is_some() {
            return false;
        }
        // Counts are decimal strings, not machine integers. Huge failure
        // counters must not overflow into an absent or zero observation.
        failure |= failed && count.bytes().any(|byte| byte != b'0');
    }
    failure
}

fn decimal_count(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn runner_duration(text: &str) -> bool {
    let number = ["milliseconds", "seconds", "ms", "s"]
        .iter()
        .find_map(|unit| text.trim().strip_suffix(*unit).map(str::trim));
    let Some(number) = number else {
        return false;
    };
    let mut parts = number.split('.');
    parts.next().is_some_and(decimal_count)
        && parts.next().is_none_or(decimal_count)
        && parts.next().is_none()
}

pub(super) fn succeeded(output: &ToolOutput) -> bool {
    // Diagnostic extraction may inspect a prefix, but that prefix can never
    // certify success when a failure or final status could follow the bound.
    if output.text.len() > MAX_SCANNED_OUTPUT_BYTES
        || output.failed()
        || !super::structured_output_allows_completion(&output.text)
    {
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
    fn native_error_envelopes_veto_optimistic_process_completion() {
        for text in [
            r#"{"schema":"ee.error.v2","error":{"code":"migration_required","message":"migrate first"}}"#,
            r#"{"schema":"ee.error.v2","error":{"code":false}}"#,
            r#"{"schema":"ee.rch.verify.v1","success":false,"degraded_codes":["rch_verify_topology_blocked"]}"#,
            r#"{"schema":"ee.rch.verify.v1","success":false,"degraded_codes":[]}"#,
        ] {
            let result = ToolOutput {
                is_error: Some(false),
                exit_code: Some(0),
                ..output(text)
            };
            assert!(failed(&result), "{text}");
            assert!(!succeeded(&result), "{text}");
        }
        assert!(succeeded(&ToolOutput {
            is_error: Some(false),
            exit_code: Some(0),
            ..output(r#"{"schema":"ee.response.v2","success":true,"data":{}}"#)
        }));
    }

    #[test]
    fn native_abstentions_and_ambiguous_status_never_certify_a_repair() {
        for text in [
            r#"{"schema":"ee.rch.verify.v1","success":null,"exit_code":null,"verdict":"abstained","abstention_reason":"no_execution_attempted"}"#,
            r#"{"schema":"ee.rch.verify.v1","exit_code":0}"#,
            r#"{"schema":"ee.rch.verify.v1","success":"true","exit_code":0}"#,
            r#"{"schema":"ee.rch.verify.v1","success":false,"success":true,"exit_code":0}"#,
            r#"{"schema":"ee.rch.verify.v1","schema":"example","success":true,"exit_code":0}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"verdict":"abstained"}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"abstention_reason":"no_execution_attempted"}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"timed_out":true}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":null}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":1}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"degraded_codes":["rch_verify_topology_blocked"]}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"degraded_codes":["rch_verify_remote_marker_missing"]}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"known_blocker":{"schema":"ee.rch.known_blocker.v1","blocker_kind":"capacity_or_timeout"}}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"verdict":"passed","degraded_codes":["rch_verify_proof_broker_reuse_existing"]}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"status":"proof_broker_reuse"}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"verification_attribution":"not_run_proof_broker_reuse"}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"degraded_codes":["rch_verify_proof_broker_wait_for_inflight"]}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0"#,
            r#"{"schema":"ee.response.v2","success":false,"data":{}}"#,
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
        for text in [
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0}"#,
            r#"{"schema":"ee.rch.verify.v1","success":true,"exit_code":0,"verdict":"passed","timed_out":false,"abstention_reason":null,"known_blocker":null}"#,
            r#"{"example":{"schema":"ee.rch.verify.v1","success":false}}"#,
        ] {
            assert!(
                succeeded(&ToolOutput {
                    is_error: Some(false),
                    exit_code: Some(0),
                    ..output(text)
                }),
                "{text}"
            );
        }
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

    #[test]
    fn cross_language_failures_override_successful_tool_and_shell_status() {
        for text in [
            "================ 1 failed, 21 passed in 0.03s ================",
            "1 error, 21 passed in 125ms",
            "Ran 22 tests in 0.003s\nFAILED (failures=1, errors=0)",
            "Test Suites: 1 failed, 2 passed, 3 total",
            "Tests: 1 failed, 21 passed, 22 total",
            "Test Files 1 failed | 2 passed (3)",
            "Tests 1 failed | 21 passed (22)",
            "Snapshots: 1 failed, 2 passed, 3 total",
            "21 passing (20ms)\n1 failing",
            "21 pass\n1 fail",
            "FAIL\texample.test/cache\t0.031s",
            "TAP version 13\nnot ok 2 - cache reload\n1..2",
            "999999999999999999999999999 failed, 1 passed",
        ] {
            let result = ToolOutput {
                is_error: Some(false),
                exit_code: Some(0),
                ..output(text)
            };
            assert!(failed(&result), "lost runner failure: {text}");
            assert!(!succeeded(&result), "false repair proof: {text}");
            assert_eq!(result.text, text, "classification rewrote evidence");
        }
    }

    #[test]
    fn successful_runner_reports_still_require_explicit_completion() {
        for text in [
            "================ 22 passed in 0.03s ================",
            "0 failed, 22 passed in 125ms",
            "000 errors, 22 passed",
            "Ran 22 tests in 0.003s\nOK",
            "Test Suites: 0 failed, 3 passed, 3 total",
            "Test Files 3 passed (3)",
            "Tests 000 failed | 22 passed (22)",
            "Snapshots: 3 passed, 3 total",
            "21 pass\n0 fail",
            "TAP version 13\nok 1 - reload\n1..1",
        ] {
            assert!(!failed(&output(text)), "false failure: {text}");
            assert!(!succeeded(&output(text)), "stdout granted authority: {text}");
            assert!(succeeded(&ToolOutput {
                exit_code: Some(0),
                ..output(text)
            }), "lost valid completed run: {text}");
        }
    }

    #[test]
    fn report_grammar_does_not_reinterpret_prose_or_examples() {
        for text in [
            "the previous run had 1 failed test",
            "1 failed in src/other.rs",
            "1 failed, then cargo test passed",
            "Tests: 1 failed (quoted example)",
            "Tests: 1 failed | run repair.sh",
            "testsuite: 1 failed",
            "Test Files 1 failed (3) trailing qualification",
            "\"1 failed\"",
            "not okay 2",
            "not ok cache reload",
            "failed (fix=1)",
            "=== 1 failed",
            "-1 failed",
            "1e9 failed",
            "1 failed in unknowns",
        ] {
            assert!(!runner_report_failed(&text.to_ascii_lowercase()), "{text}");
        }
    }
}
