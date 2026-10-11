//! Bounded libtest JSON execution histories, not discovery or printed examples.
//!
//! The enclosing compiler decoder owns the byte/record limits and complete
//! JSONL validation. Here each executed test consumes one start, suite
//! counters must reconcile with observed events, and a successful run must
//! actually execute a test. Test stdout/message payloads are never fingerprints.
//! Event shapes follow rust-lang/rust 6f5da406, library/test/src/formatters/json.rs.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::core::error_recall::{CanonicalDiagnostic, from_cargo};

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    event: String,
    name: Option<String>,
    test_count: Option<u64>,
    passed: Option<u64>,
    failed: Option<u64>,
    ignored: Option<u64>,
    measured: Option<u64>,
    filtered_out: Option<u64>,
    exec_time: Option<f64>,
    // Decode, but never use these untrusted payloads as diagnostic text.
    stdout: Option<String>,
    message: Option<String>,
    reason: Option<String>,
}

impl Event {
    fn has_counters(&self) -> bool {
        self.passed.is_some()
            || self.failed.is_some()
            || self.ignored.is_some()
            || self.measured.is_some()
            || self.filtered_out.is_some()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TestState {
    Running,
    Finished,
}

struct Suite {
    expected: u64,
    tests: BTreeMap<String, TestState>,
    passed: u64,
    failed: u64,
    ignored: u64,
}

/// State is scoped to one invocation; separate suite binaries may reuse names.
#[derive(Default)]
pub(super) struct Harness {
    active: Option<Suite>,
    seen: bool,
    failed: bool,
    passed: u64,
}

impl Harness {
    pub(super) fn seen(&self) -> bool {
        self.seen
    }

    pub(super) fn failed(&self) -> bool {
        self.failed
    }

    pub(super) fn allows_completion(&self) -> bool {
        self.seen && self.active.is_none() && !self.failed && self.passed > 0
    }

    /// An incomplete but well-formed run can supply failure diagnostics, never
    /// completion. A malformed or contradictory event refuses the whole input.
    pub(super) fn observe(&mut self, raw: &str) -> Result<Option<CanonicalDiagnostic>, ()> {
        let event: Event = serde_json::from_str(raw).map_err(|_| ())?;
        if event.exec_time.is_some_and(|time| !time.is_finite() || time < 0.0) {
            return Err(());
        }
        match event.kind.as_str() {
            "suite" => self.suite_event(event),
            "test" => self.test_event(event),
            // Benchmarks, discovery and future report formats are not a
            // successful test execution proof for this supported contract.
            _ => Err(()),
        }
    }

    fn suite_event(&mut self, event: Event) -> Result<Option<CanonicalDiagnostic>, ()> {
        if event.name.is_some()
            || event.stdout.is_some()
            || event.message.is_some()
            || event.reason.is_some()
        {
            return Err(());
        }
        match event.event.as_str() {
            "started" => {
                if self.active.is_some() || event.has_counters() {
                    return Err(());
                }
                self.active = Some(Suite {
                    expected: event.test_count.ok_or(())?,
                    tests: BTreeMap::new(),
                    passed: 0,
                    failed: 0,
                    ignored: 0,
                });
                self.seen = true;
            }
            "ok" | "failed" => {
                if event.test_count.is_some() || event.measured != Some(0) {
                    return Err(());
                }
                let suite = self.active.as_ref().ok_or(())?;
                let passed = event.passed.ok_or(())?;
                let failed = event.failed.ok_or(())?;
                let ignored = event.ignored.ok_or(())?;
                let _filtered = event.filtered_out.ok_or(())?;
                let completed = passed.checked_add(failed).and_then(|n| n.checked_add(ignored));
                if (passed, failed, ignored) != (suite.passed, suite.failed, suite.ignored)
                    || suite.tests.values().any(|state| *state == TestState::Running)
                    || completed != Some(suite.tests.len() as u64)
                    || completed.is_none_or(|count| count > suite.expected)
                    || (event.event == "ok" && (failed != 0 || completed != Some(suite.expected)))
                    || (event.event == "failed" && failed == 0)
                {
                    return Err(());
                }
                // A failed --fail-fast suite may finish before every selected
                // test starts. It still cannot certify completion or a repair.
                self.failed |= failed != 0;
                self.passed = self.passed.saturating_add(passed);
                self.active = None;
            }
            _ => return Err(()),
        }
        Ok(None)
    }

    fn test_event(&mut self, event: Event) -> Result<Option<CanonicalDiagnostic>, ()> {
        if event.test_count.is_some() || event.has_counters() {
            return Err(());
        }
        let name = event.name.ok_or(())?;
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err(());
        }
        let suite = self.active.as_mut().ok_or(())?;
        match event.event.as_str() {
            "started" => {
                if event.stdout.is_some()
                    || event.message.is_some()
                    || event.reason.is_some()
                    || suite.tests.len() as u64 >= suite.expected
                    || suite.tests.insert(name, TestState::Running).is_some()
                {
                    return Err(());
                }
            }
            "timeout" => {
                // Libtest's timeout event is an in-progress notification, not
                // a terminal result. A later failed event carries the failure.
                if suite.tests.get(&name) != Some(&TestState::Running) {
                    return Err(());
                }
            }
            "ok" | "failed" | "ignored" => {
                let previous = suite.tests.get(&name).copied();
                // Some ignored tests are reported without being launched.
                if previous != Some(TestState::Running)
                    && !(event.event == "ignored"
                        && previous.is_none()
                        && (suite.tests.len() as u64) < suite.expected)
                {
                    return Err(());
                }
                if event.reason.is_some() && event.event != "failed" {
                    return Err(());
                }
                suite.tests.insert(name.clone(), TestState::Finished);
                match event.event.as_str() {
                    "ok" => suite.passed += 1,
                    "ignored" => suite.ignored += 1,
                    _ => {
                        suite.failed += 1;
                        self.failed = true;
                        // Match the existing plaintext failing-test key exactly.
                        // Redact the name too; parameterized names can carry data.
                        let line = format!("test {name} ... FAILED");
                        let line = crate::policy::redact_secret_like_content(&line).content;
                        return Ok(Some(from_cargo(None, &format!("test failed: {line}"))));
                    }
                }
            }
            _ => return Err(()),
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{ToolOutput, failure_diagnostics};
    use serde_json::{Value, json};

    fn start(count: u64) -> Value {
        json!({"type":"suite","event":"started","test_count":count})
    }

    fn test(name: &str, event: &str) -> Value {
        json!({"type":"test","event":event,"name":name})
    }

    fn end(passed: u64, failed: u64, ignored: u64) -> Value {
        json!({"type":"suite","event":if failed == 0 {"ok"} else {"failed"},
            "passed":passed,"failed":failed,"ignored":ignored,"measured":0,"filtered_out":0})
    }

    fn stream(events: &[Value]) -> String {
        events.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
    }

    fn inspect(events: &[Value]) -> super::super::CompilerOutput {
        super::super::inspect(&stream(events)).expect("valid stream").expect("libtest family")
    }

    #[test]
    fn failed_json_test_matches_plaintext_fingerprint_and_never_uses_stdout() {
        let mut failed = test("store::round_trip", "failed");
        failed["stdout"] = json!("PRIVATE_STDOUT_SENTINEL error[E0308]: printed example");
        let output = inspect(&[start(1), test("store::round_trip", "started"), failed, end(0,1,0)]);
        let plain = failure_diagnostics(&ToolOutput {
            text: "test store::round_trip ... FAILED\ntest result: FAILED. 0 passed; 1 failed".into(),
            is_error: Some(true), exit_code: Some(101),
        });
        assert_eq!(output.diagnostics, plain);
        assert!(output.failed);
        assert!(!output.allows_completion);
        assert!(!format!("{:?}", output.diagnostics).contains("PRIVATE_STDOUT_SENTINEL"));
    }

    #[test]
    fn interleaved_tests_and_multiple_suite_binaries_reconcile_by_name() {
        let output = inspect(&[
            start(3), test("a", "started"), test("b", "started"),
            test("b", "ok"), test("a", "timeout"), test("a", "ok"),
            test("ignored", "ignored"), end(2,0,1),
            start(1), test("a", "started"), test("a", "ok"), end(1,0,0),
        ]);
        assert!(output.allows_completion);
        assert!(!output.failed);
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn compile_success_is_followed_by_checked_test_execution_not_an_early_success() {
        let events = [json!({"reason":"compiler-artifact","fresh":true}),
            json!({"reason":"build-finished","success":true}),
            start(1), test("a", "started"), test("a", "ok"), end(1,0,0)];
        let output = inspect(&events);
        assert!(output.allows_completion);
        let mut wrapper = ToolOutput { text: stream(&events), is_error: None, exit_code: None };
        assert!(!wrapper.succeeded(), "suite completion is not process completion");
        wrapper.exit_code = Some(0);
        assert!(wrapper.succeeded());
        let mut failed = events;
        failed[4] = test("a", "failed");
        failed[5] = end(0,1,0);
        wrapper.text = stream(&failed);
        assert!(wrapper.failed());
        assert!(!wrapper.succeeded());
    }

    #[test]
    fn missing_duplicate_or_contradictory_events_never_certify_a_repair() {
        let cases = [
            vec![test("a","ok")],
            vec![start(1),test("a","ok"),end(1,0,0)],
            vec![start(1),test("a","started"),test("a","started")],
            vec![start(1),test("a","started"),test("a","ok"),test("a","ok")],
            vec![start(1),test("a","started"),test("b","ok")],
            vec![start(1),test("a","started"),test("a","failed"),end(1,0,0)],
            vec![start(2),test("a","started"),test("a","ok"),end(1,0,0)],
            vec![start(0),start(0)],
            vec![start(1),test("a","started"),test("a","ok"),end(u64::MAX,1,0)],
            vec![json!({"type":"suite","event":"discovery"})],
            vec![json!({"reason":"build-finished","success":false}),start(0)],
            vec![json!({"reason":"compiler-artifact"}),start(0)],
        ];
        for events in cases {
            assert!(super::super::inspect(&stream(&events)).is_err(), "{events:?}");
            assert!(!ToolOutput {text:stream(&events),is_error:Some(false),exit_code:Some(0)}.succeeded());
        }
    }

    #[test]
    fn partial_failed_and_empty_runs_remain_distinct_from_successful_execution() {
        for events in [vec![start(1)], vec![start(1),test("a","started")],
            vec![start(0),end(0,0,0)],
            vec![start(1),test("a","ignored"),end(0,0,1)]] {
            assert!(!inspect(&events).allows_completion);
        }
        let partial = inspect(&[start(2),test("a","started"),test("a","failed")]);
        assert!(partial.failed);
        assert_eq!(partial.diagnostics.len(),1);
        assert!(!partial.allows_completion);
        let fail_fast = inspect(&[start(20),test("a","started"),test("a","failed"),end(0,1,0)]);
        assert!(fail_fast.failed);
        assert!(!fail_fast.allows_completion);
    }

    #[test]
    fn a_later_good_suite_never_erases_failure_or_an_unfinished_suite() {
        let failed = [start(1),test("a","started"),test("a","failed"),end(0,1,0),
            start(1),test("a","started"),test("a","ok"),end(1,0,0)];
        assert!(inspect(&failed).failed);
        assert!(!inspect(&failed).allows_completion);
        let unfinished = [start(1),test("a","started"),test("a","ok"),end(1,0,0),start(1)];
        assert!(!inspect(&unfinished).allows_completion);
    }

    #[test]
    fn status_duplicates_conflicting_families_and_trailing_data_refuse_the_whole_stream() {
        let prefix = stream(&[start(1),test("a","started")]);
        for tail in [
            r#"{"type":"test","event":"failed","event":"ok","name":"a"}"#,
            r#"{"type":"test","event":"ok","name":"a","name":"b"}"#,
            r#"{"type":"test","event":"ok","name":"a","success":false}"#,
            r#"{"type":"test","event":"ok","name":"a","$message_type":"diagnostic"}"#,
            r#"{"type":"test","event":"ok","name":"a","reason":"compiler-message"}"#,
            r#"{"type":"test","event":"ok","name":"a","exec_time":-1}"#,
            r#"{"type":"suite","event":"ok","passed":1,"failed":0,"ignored":0,"measured":0,"filtered_out":0,"failed":1}"#,
            r#"{"reason":"build-finished","success":true}"#,
            "not JSON", "{",
        ] {
            assert!(super::super::inspect(&format!("{prefix}\n{tail}")).is_err(), "{tail}");
        }
        let valid = stream(&[start(1),test("a","started"),test("a","ok"),end(1,0,0)]);
        assert!(super::super::inspect(&format!("{valid}\n{{\"example\":true}}")).is_err());
        assert!(super::super::inspect(&format!("{valid}{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":0}}")).is_err());
    }

    #[test]
    fn diagnostic_caps_and_name_redaction_do_not_skip_final_validation() {
        let mut events = vec![start(8)];
        for number in 0..8 {
            let name = format!("suite::case_{number}");
            events.push(test(&name, "started"));
            events.push(test(&name, "failed"));
        }
        events.push(end(0, 8, 0));
        let result = inspect(&events);
        assert_eq!(result.diagnostics.len(), super::super::MAX_DIAGNOSTICS_PER_FAILURE);
        assert!(result.failed);
        events.push(json!({"type":"suite","event":"unknown"}));
        assert!(super::super::inspect(&stream(&events)).is_err());

        let name = "suite::auth api_key=sk-proj-ABCDEF1234567890ABCDEF1234567890";
        let result = inspect(&[start(1), test(name, "started"), test(name, "failed"), end(0, 1, 0)]);
        assert_eq!(result.diagnostics.len(), 1);
        assert!(!result.diagnostics[0].message_template.contains("abcdef1234567890"));
    }

}
