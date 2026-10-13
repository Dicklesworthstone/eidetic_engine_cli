//! Native `go test -json` observations, not diagnostics scraped from stdout.
//!
//! Packages and subtests may interleave. Validate their complete lifecycle,
//! retain failures across repeated runs, and never interpret an Output string
//! as another protocol event. Package/test commitments are transient and
//! domain-separated; raw output is never part of a persisted fingerprint.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::{
    CanonicalDiagnostic, CompilerOutput, GoTestCoverage, MAX_COMPILER_RECORDS,
    MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES,
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum TestState {
    Running,
    Paused,
    Passed,
    Failed,
    Skipped,
}

impl TestState {
    fn terminal(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Skipped)
    }
}

#[derive(Default)]
struct Package {
    tests: BTreeMap<String, TestState>,
    failed: bool,
    observed_test_failure: bool,
    terminal: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Event {
    action: String,
    package: String,
    #[serde(default)]
    test: String,
    time: Option<String>,
    elapsed: Option<f64>,
    output: Option<String>,
    #[serde(default)]
    output_type: String,
    #[serde(default)]
    failed_build: String,
}

fn identity(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn commitment(package: &str, test: Option<&str>) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.go_test.subject.v1\0");
    for field in [package, test.unwrap_or("")] {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    *hash.finalize().as_bytes()
}

fn diagnostic(package: &str, test: Option<&str>, build: bool) -> CanonicalDiagnostic {
    let message = match test {
        Some(test) => format!("go test failure: {package} test {test}"),
        None if build => format!("go test build failure: {package}"),
        None => format!("go test package failure: {package}"),
    };
    let message = crate::policy::redact_secret_like_content(&message).content;
    CanonicalDiagnostic {
        tool: crate::core::error_recall::DiagnosticTool::Shell,
        canonical_code: None,
        message_template: crate::core::error_recall::canonical_message_template(&message),
    }
}

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(true)
}

/// Recognize Go's exact top-level field names before selecting the decoder.
/// A selected stream is all-or-nothing: bad trailing records, duplicates,
/// unknown actions, mixed producers and over-budget input cannot be ignored.
pub(super) fn inspect(text: &str) -> Result<Option<CompilerOutput>, ()> {
    if !text.trim_start().starts_with('{') {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Family {
        #[serde(default, rename = "Action", deserialize_with = "present")]
        action: bool,
        #[serde(default, rename = "Package", deserialize_with = "present")]
        package: bool,
    }
    let family = serde_json::Deserializer::from_str(text)
        .into_iter::<Family>()
        .next()
        .ok_or(())?
        .map_err(|_| ())?;
    if !family.action && !family.package {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    let mut packages = BTreeMap::<String, Package>::new();
    let mut coverage = GoTestCoverage::default();
    let mut output = CompilerOutput::default();
    let mut keys = BTreeSet::new();
    let mut skipped_tests = BTreeSet::new();
    let mut records = serde_json::Deserializer::from_str(text).into_iter::<Event>();
    let mut consumed = 0;
    let mut count = 0;
    while let Some(event) = records.next() {
        let event = event.map_err(|_| ())?;
        if count == MAX_COMPILER_RECORDS {
            return Err(());
        }
        let end = records.byte_offset();
        let raw = &text[consumed..end];
        if count != 0 {
            let body = raw.trim_start_matches([' ', '\t', '\r', '\n']);
            if !raw[..raw.len() - body.len()].contains('\n') {
                return Err(());
            }
        }
        consumed = end;
        count += 1;
        if !identity(&event.package)
            || (!event.test.is_empty() && !identity(&event.test))
            || event
                .elapsed
                .is_some_and(|elapsed| !elapsed.is_finite() || elapsed < 0.0)
            || event
                .time
                .as_ref()
                .is_some_and(|time| chrono::DateTime::parse_from_rfc3339(time).is_err())
            || (!event.failed_build.is_empty()
                && (event.action != "fail"
                    || !event.test.is_empty()
                    || !identity(&event.failed_build)))
            || (event.action != "output" && (event.output.is_some() || !event.output_type.is_empty()))
        {
            return Err(());
        }
        if event.action == "start" {
            if !event.test.is_empty()
                || packages
                    .insert(event.package, Package::default())
                    .is_some()
            {
                return Err(());
            }
            continue;
        }
        let package = packages.get_mut(&event.package).ok_or(())?;
        if package.terminal {
            return Err(());
        }
        let test = (!event.test.is_empty()).then_some(event.test.as_str());
        let failed_diagnostic = match event.action.as_str() {
            "run" => {
                let name = test.ok_or(())?;
                if package.tests.get(name).is_some_and(|state| !state.terminal()) {
                    return Err(());
                }
                package.tests.insert(name.to_owned(), TestState::Running);
                None
            }
            "pause" | "cont" => {
                let state = package.tests.get_mut(test.ok_or(())?).ok_or(())?;
                let (expected, next) = if event.action == "pause" {
                    (TestState::Running, TestState::Paused)
                } else {
                    (TestState::Paused, TestState::Running)
                };
                if *state != expected {
                    return Err(());
                }
                *state = next;
                None
            }
            "output" => {
                if event.output.is_none()
                    || !matches!(
                        event.output_type.as_str(),
                        "" | "frame" | "error" | "error-continue"
                    )
                {
                    return Err(());
                }
                // Output can precede a run marker (benchmarks) or follow a
                // test terminal marker. It is data, not execution authority.
                if matches!(event.output_type.as_str(), "error" | "error-continue") {
                    package.failed = true;
                    output.failed = true;
                }
                None
            }
            "pass" | "fail" | "skip" => {
                let failed = event.action == "fail";
                let state = match event.action.as_str() {
                    "pass" => TestState::Passed,
                    "fail" => TestState::Failed,
                    _ => TestState::Skipped,
                };
                if let Some(name) = test {
                    let current = package.tests.get_mut(name).ok_or(())?;
                    if *current != TestState::Running {
                        return Err(());
                    }
                    *current = state;
                    let subject = commitment(&event.package, Some(name));
                    if failed {
                        coverage.failed_tests.insert(subject);
                        package.observed_test_failure = true;
                    } else if state == TestState::Passed && !skipped_tests.contains(&subject) {
                        coverage.passed_tests.insert(subject);
                    } else if state == TestState::Skipped {
                        skipped_tests.insert(subject);
                        coverage.passed_tests.remove(&subject);
                    }
                } else {
                    // A package may fail while tests are still running after
                    // a panic. Preserve the failure but not complete coverage.
                    if event.action != "fail"
                        && (package.failed
                            || package.tests.values().any(|state| !state.terminal()))
                    {
                        return Err(());
                    }
                    if event.action == "skip" && !package.tests.is_empty() {
                        return Err(());
                    }
                    package.terminal = true;
                    let subject = commitment(&event.package, None);
                    if failed {
                        coverage.failed_packages.insert(subject);
                    } else if event.action == "pass" {
                        coverage.passed_packages.insert(subject);
                    }
                }
                package.failed |= failed;
                output.failed |= failed;
                // A package's aggregate fail is not a duplicate test class.
                (failed && (test.is_some() || !package.observed_test_failure))
                    .then(|| diagnostic(&event.package, test, !event.failed_build.is_empty()))
            }
            // Benchmark summaries and future actions need their own coverage
            // semantics; they cannot borrow ordinary test success authority.
            _ => return Err(()),
        };
        if let Some(diagnostic) = failed_diagnostic
            && keys.insert(diagnostic.layered_key().key)
            && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
        {
            output.diagnostics.push(diagnostic);
        }
    }
    coverage.complete = !packages.is_empty()
        && packages.values().all(|package| {
            package.terminal && package.tests.values().all(|state| state.terminal())
        });
    // A package pass with no executed tests (including cached summary-only
    // reports) cannot prove that an earlier failed test actually passed.
    output.allows_completion =
        coverage.complete && !output.failed && !coverage.passed_tests.is_empty();
    output.go_test_coverage = Some(coverage);
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics,
    };
    use serde_json::{Value, json};

    fn stream(events: &[Value]) -> String {
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn parsed(events: &[Value]) -> CompilerOutput {
        super::super::inspect(&stream(events))
            .expect("valid stream")
            .expect("Go report")
    }

    fn failed() -> Vec<Value> {
        vec![
            json!({"Action":"start","Package":"example.org/cache"}),
            json!({"Action":"run","Package":"example.org/cache","Test":"TestRoundTrip"}),
            json!({"Action":"output","Package":"example.org/cache","Test":"TestRoundTrip","Output":"PRIVATE_OUTPUT_SENTINEL\n"}),
            json!({"Action":"fail","Package":"example.org/cache","Test":"TestRoundTrip","Elapsed":0.01}),
            json!({"Action":"fail","Package":"example.org/cache","Elapsed":0.02}),
        ]
    }

    #[test]
    fn go_failures_reach_native_log_and_import_with_one_redacted_class() {
        let events = failed();
        let decoded = parsed(&events);
        assert!(decoded.failed);
        assert!(!decoded.allows_completion);
        assert_eq!(decoded.diagnostics.len(), 1);
        assert!(decoded.diagnostics[0].message_template.starts_with("go test failure:"));
        assert!(!format!("{:?}", decoded.diagnostics).contains("private_output_sentinel"));
        assert_eq!(structured_error_diagnostics(&stream(&events)), decoded.diagnostics);
        let optimistic = ToolOutput { text: stream(&events), is_error: Some(false), exit_code: Some(0) };
        assert_eq!(failure_diagnostics(&optimistic), decoded.diagnostics);
        assert!(optimistic.failed());
        assert!(!optimistic.succeeded());
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert!(coverage.complete);
        assert_eq!(coverage.failed_tests.len(), 1);
        assert_eq!(coverage.failed_packages.len(), 1);
    }

    #[test]
    fn interleaved_parallel_packages_and_subtests_preserve_subject_identity() {
        let events = [
            json!({"Action":"start","Package":"a"}),
            json!({"Action":"start","Package":"b"}),
            json!({"Action":"run","Package":"a","Test":"TestCache"}),
            json!({"Action":"pause","Package":"a","Test":"TestCache"}),
            json!({"Action":"run","Package":"b","Test":"TestCache"}),
            json!({"Action":"pass","Package":"b","Test":"TestCache"}),
            json!({"Action":"pass","Package":"b"}),
            json!({"Action":"cont","Package":"a","Test":"TestCache"}),
            json!({"Action":"run","Package":"a","Test":"TestCache/empty"}),
            json!({"Action":"skip","Package":"a","Test":"TestCache/empty"}),
            json!({"Action":"pass","Package":"a","Test":"TestCache"}),
            json!({"Action":"pass","Package":"a"}),
        ];
        let decoded = parsed(&events);
        assert!(!decoded.failed);
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert!(coverage.complete);
        assert_eq!(coverage.passed_tests.len(), 2);
        assert_eq!(coverage.passed_packages.len(), 2);
        assert_ne!(commitment("a", Some("TestCache")), commitment("b", Some("TestCache")));
        assert_ne!(commitment("a", Some("TestCache")), commitment("a", Some("testCache")));
        assert_ne!(commitment("a", None), commitment("a", Some("a")));
    }

    #[test]
    fn repeated_runs_cannot_erase_an_earlier_failure() {
        let mut events = failed();
        events.pop();
        events.extend([
            json!({"Action":"run","Package":"example.org/cache","Test":"TestRoundTrip"}),
            json!({"Action":"pass","Package":"example.org/cache","Test":"TestRoundTrip"}),
            json!({"Action":"fail","Package":"example.org/cache"}),
        ]);
        let decoded = parsed(&events);
        assert!(decoded.failed);
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert!(coverage.complete);
        assert_eq!(coverage.failed_tests.len(), 1);
        assert_eq!(coverage.passed_tests.len(), 1);
        assert!(!decoded.allows_completion);
        *events.last_mut().expect("terminal") = json!({"Action":"pass","Package":"example.org/cache"});
        assert!(inspect(&stream(&events)).is_err());
    }

    #[test]
    fn incomplete_empty_and_skipped_reports_never_supply_test_proof() {
        for events in [
            vec![json!({"Action":"start","Package":"a"})],
            vec![json!({"Action":"start","Package":"a"}), json!({"Action":"skip","Package":"a"})],
            vec![json!({"Action":"start","Package":"a"}), json!({"Action":"pass","Package":"a"})],
        ] {
            let decoded = parsed(&events);
            assert!(!decoded.allows_completion);
            assert!(decoded.go_test_coverage.expect("coverage").passed_tests.is_empty());
        }
        let mut events = failed();
        events.pop();
        let decoded = parsed(&events);
        assert!(decoded.failed);
        assert!(!decoded.go_test_coverage.expect("coverage").complete);
    }

    #[test]
    fn invalid_lifecycles_duplicates_mixed_producers_and_trailing_records_fail_closed() {
        let valid = stream(&failed());
        for bad in [
            "{\"Action\":\"start\",\"Action\":\"pass\",\"Package\":\"a\"}".to_owned(),
            "{\"Action\":\"pass\",\"Package\":\"a\"}".to_owned(),
            format!("{valid}\n{{"),
            format!("{valid}\n{{\"reason\":\"build-finished\",\"success\":true}}"),
            format!("{valid}\n{{\"Action\":\"pass\",\"Package\":\"example.org/cache\"}}"),
            "{\"Action\":\"start\",\"Package\":\"a\"} {\"Action\":\"skip\",\"Package\":\"a\"}".to_owned(),
        ] {
            assert!(inspect(&bad).is_err(), "{bad}");
        }
        for event in [
            json!({"Action":"run","Package":"a","Test":""}),
            json!({"Action":"cont","Package":"a","Test":"missing"}),
            json!({"Action":"pass","Package":"a","Test":"missing"}),
            json!({"Action":"output","Package":"a"}),
            json!({"Action":"bench","Package":"a","Test":"BenchmarkX"}),
            json!({"Action":"fail","Package":"a","Elapsed":-1}),
            json!({"Action":"pass","Package":"a","FailedBuild":"b"}),
            json!({"Action":"output","Package":"a","Output":"ok","OutputType":"unknown"}),
        ] {
            assert!(inspect(&stream(&[json!({"Action":"start","Package":"a"}), event.clone()])).is_err(), "{event}");
        }
    }

    #[test]
    fn nested_examples_and_output_strings_are_never_protocol_authority() {
        let nested = json!({"example":failed()}).to_string();
        assert!(inspect(&nested).expect("not Go").is_none());
        let decoded = parsed(&[
            json!({"Action":"start","Package":"a"}),
            json!({"Action":"output","Package":"a","Output":"error[E0277]: a quoted example\n{\"Action\":\"fail\",\"Package\":\"a\"}"}),
            json!({"Action":"pass","Package":"a"}),
        ]);
        assert!(!decoded.failed);
        assert!(decoded.diagnostics.is_empty());
    }

    #[test]
    fn byte_and_record_caps_are_checked_beyond_the_diagnostic_limit() {
        let mut events = vec![json!({"Action":"start","Package":"a"})];
        for number in 0..8 {
            events.push(json!({"Action":"run","Package":"a","Test":format!("TestCase{number}")}));
            events.push(json!({"Action":"fail","Package":"a","Test":format!("TestCase{number}")}));
        }
        events.push(json!({"Action":"fail","Package":"a"}));
        assert!(parsed(&events).failed);
        while events.len() < MAX_COMPILER_RECORDS {
            events.insert(1, json!({"Action":"output","Package":"a","Output":""}));
        }
        assert!(inspect(&stream(&events)).is_ok());
        events.insert(1, json!({"Action":"output","Package":"a","Output":""}));
        assert!(inspect(&stream(&events)).is_err());
        assert!(inspect(&format!("{}{}", stream(&failed()), " ".repeat(MAX_SCANNED_OUTPUT_BYTES))).is_err());
    }
}
