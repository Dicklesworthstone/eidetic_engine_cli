//! Native `go test -json` observations, not diagnostics scraped from stdout.
//!
//! Packages and subtests may interleave. Validate their complete lifecycle,
//! retain failures across repeated runs, and never interpret an Output string
//! as another protocol event. Package/test commitments are transient and
//! domain-separated; raw output is never part of a persisted fingerprint.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::{
    CanonicalDiagnostic, CompilerOutput, GoTestCoverage, MAX_DIAGNOSTICS_PER_FAILURE,
    MAX_SCANNED_OUTPUT_BYTES,
};

// test2json emits lifecycle and output events, not one record per diagnostic.
// A normal 100-test report already exceeds the compiler's 256-record budget.
// Bound this producer independently while keeping the shared 64 KiB byte cap,
// complete-stream validation, and four-diagnostic display cap unchanged.
const MAX_GO_RECORDS: usize = 1024;

#[derive(Clone, Copy, Eq, PartialEq)]
enum TestState {
    Running,
    Paused,
    Passed,
    Failed,
    Skipped,
    // Go can terminate benchmark runs at the package boundary rather than
    // emitting an individual pass. This state alone does not assert success.
    BenchmarkFinished,
}

impl TestState {
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed | Self::Skipped | Self::BenchmarkFinished
        )
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

/// BuildEvent.ImportPath is a build identity, NOT TestEvent.Package. Only
/// a package's explicit FailedBuild field can associate those namespaces.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct BuildEvent {
    import_path: String,
    action: String,
    output: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Record {
    Test(Event),
    Build(BuildEvent),
}

fn identity(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

/// Go's benchmark entry-point spelling, including sub-benchmarks and CPU
/// suffixes. Do not normalize a name: proof must bind the exact source subject.
/// In particular, TestCache/BenchmarkInner is an ordinary test, not a benchmark.
fn benchmark_name(name: &str) -> bool {
    name.strip_prefix("Benchmark")
        .is_some_and(|suffix| suffix.chars().next().is_none_or(|ch| !ch.is_lowercase()))
}

/// Modern test2json streams can emit benchmark run markers without individual
/// terminal markers. A package terminal closes those lifecycles, but only a
/// package pass supplies positive coverage. Ordinary tests still require their
/// own pass/fail/skip events, and printed timing lines never supply authority.
fn finish_benchmarks(
    name: &str,
    package: &mut Package,
    coverage: &mut GoTestCoverage,
    skipped: &BTreeSet<[u8; 32]>,
    passed: bool,
) {
    for (test, state) in &mut package.tests {
        if *state != TestState::Running || !benchmark_name(test) {
            continue;
        }
        *state = TestState::BenchmarkFinished;
        let subject = commitment(name, Some(test));
        if passed && !skipped.contains(&subject) {
            coverage.passed_tests.insert(subject);
        }
    }
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
        None if build => format!("go build failure: {package}"),
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
    // Bound even family probing: direct callers must not deserialize an
    // oversized first record before discovering that this is a Go stream.
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    if !text.trim_start().starts_with('{') {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Family {
        #[serde(default, rename = "Action", deserialize_with = "present")]
        action: bool,
        #[serde(default, rename = "Package", deserialize_with = "present")]
        package: bool,
        #[serde(default, rename = "ImportPath", deserialize_with = "present")]
        import_path: bool,
    }
    let family = serde_json::Deserializer::from_str(text)
        .into_iter::<Family>()
        .next()
        .ok_or(())?
        .map_err(|_| ())?;
    if !family.action && !family.package && !family.import_path {
        return Ok(None);
    }
    let mut packages = BTreeMap::<String, Package>::new();
    let mut coverage = GoTestCoverage::default();
    let mut output = CompilerOutput::default();
    let mut keys = BTreeSet::new();
    let mut skipped_tests = BTreeSet::new();
    let mut build_failures = BTreeSet::new();
    let mut linked_builds = BTreeSet::new();
    let mut records = serde_json::Deserializer::from_str(text).into_iter::<Record>();
    let mut consumed = 0;
    let mut count = 0;
    while let Some(event) = records.next() {
        let event = event.map_err(|_| ())?;
        if count == MAX_GO_RECORDS {
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
        let event = match event {
            Record::Test(event) => event,
            Record::Build(event) => {
                if !identity(&event.import_path) {
                    return Err(());
                }
                match event.action.as_str() {
                    "build-output" if event.output.is_some() => {}
                    "build-fail" if event.output.is_none() => {
                        build_failures.insert(commitment(&event.import_path, None));
                        output.failed = true;
                        retain_diagnostic(
                            &mut output,
                            &mut keys,
                            diagnostic(&event.import_path, None, true),
                        );
                    }
                    _ => return Err(()),
                }
                continue;
            }
        };
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
            || (event.action != "output"
                && (event.output.is_some() || !event.output_type.is_empty()))
        {
            return Err(());
        }
        if event.action == "start" {
            if !event.test.is_empty()
                || packages.insert(event.package, Package::default()).is_some()
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
                if package.tests.get(name).is_some_and(|state| {
                    !state.terminal() && !(*state == TestState::Running && benchmark_name(name))
                }) {
                    return Err(());
                }
                // Benchmark calibration/count runs need not have intervening
                // terminal events. A repeated ordinary test run is still bad.
                package.tests.insert(name.to_owned(), TestState::Running);
                None
            }
            "pause" | "cont" => {
                let name = test.ok_or(())?;
                if benchmark_name(name) {
                    return Err(());
                }
                let state = package.tests.get_mut(name).ok_or(())?;
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
            "pass" | "fail" | "skip" | "bench" => {
                let failed = event.action == "fail";
                let state = match event.action.as_str() {
                    "pass" | "bench" => TestState::Passed,
                    "fail" => TestState::Failed,
                    _ => TestState::Skipped,
                };
                if let Some(name) = test {
                    let benchmark = benchmark_name(name);
                    if event.action == "bench" && !benchmark {
                        return Err(());
                    }
                    // Legacy benchmark streams have no run marker. Repeated
                    // bench/fail/skip terminals represent separate iterations;
                    // failure and skipped coverage remain sticky across them.
                    match package.tests.get(name).copied() {
                        Some(TestState::Running) => {}
                        Some(previous)
                            if benchmark && previous.terminal() && event.action != "pass" => {}
                        None if benchmark && event.action != "pass" => {}
                        _ => return Err(()),
                    }
                    package.tests.insert(name.to_owned(), state);
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
                    if event.action == "bench" {
                        return Err(());
                    }
                    if matches!(event.action.as_str(), "pass" | "fail") {
                        finish_benchmarks(
                            &event.package,
                            package,
                            &mut coverage,
                            &skipped_tests,
                            event.action == "pass",
                        );
                    }
                    // A package may fail while tests are still running after
                    // a panic. Preserve the failure but not complete coverage.
                    if event.action != "fail"
                        && (package.failed || package.tests.values().any(|state| !state.terminal()))
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
                        if !event.failed_build.is_empty() {
                            linked_builds.insert(commitment(&event.failed_build, None));
                        }
                    } else if event.action == "pass" {
                        coverage.passed_packages.insert(subject);
                    }
                }
                package.failed |= failed;
                output.failed |= failed;
                // A package's aggregate fail is not a duplicate test class.
                (failed && (test.is_some() || !package.observed_test_failure)).then(|| {
                    if event.failed_build.is_empty() {
                        diagnostic(&event.package, test, false)
                    } else {
                        diagnostic(&event.failed_build, None, true)
                    }
                })
            }
            // Unknown producer actions cannot supply completion authority.
            _ => return Err(()),
        };
        if let Some(diagnostic) = failed_diagnostic {
            retain_diagnostic(&mut output, &mut keys, diagnostic);
        }
    }
    coverage.complete = !packages.is_empty()
        && build_failures.is_subset(&linked_builds)
        && packages.values().all(|package| {
            package.terminal && package.tests.values().all(|state| state.terminal())
        });
    // A package pass with no executed subjects (including cached summary-only
    // reports) cannot prove that an earlier failed test or benchmark passed.
    output.allows_completion =
        coverage.complete && !output.failed && !coverage.passed_tests.is_empty();
    output.go_test_coverage = Some(coverage);
    Ok(Some(output))
}

fn retain_diagnostic(
    output: &mut CompilerOutput,
    keys: &mut BTreeSet<String>,
    diagnostic: CanonicalDiagnostic,
) {
    if keys.insert(diagnostic.layered_key().key)
        && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
    {
        output.diagnostics.push(diagnostic);
    }
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
        assert!(
            decoded.diagnostics[0]
                .message_template
                .starts_with("go test failure:")
        );
        assert!(!format!("{:?}", decoded.diagnostics).contains("private_output_sentinel"));
        assert_eq!(
            structured_error_diagnostics(&stream(&events)),
            decoded.diagnostics
        );
        let optimistic = ToolOutput {
            text: stream(&events),
            is_error: Some(false),
            exit_code: Some(0),
        };
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
        assert_ne!(
            commitment("a", Some("TestCache")),
            commitment("b", Some("TestCache"))
        );
        assert_ne!(
            commitment("a", Some("TestCache")),
            commitment("a", Some("testCache"))
        );
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
        *events.last_mut().expect("terminal") =
            json!({"Action":"pass","Package":"example.org/cache"});
        assert!(inspect(&stream(&events)).is_err());
    }

    #[test]
    fn incomplete_empty_and_skipped_reports_never_supply_test_proof() {
        for events in [
            vec![json!({"Action":"start","Package":"a"})],
            vec![
                json!({"Action":"start","Package":"a"}),
                json!({"Action":"skip","Package":"a"}),
            ],
            vec![
                json!({"Action":"start","Package":"a"}),
                json!({"Action":"pass","Package":"a"}),
            ],
        ] {
            let decoded = parsed(&events);
            assert!(!decoded.allows_completion);
            assert!(
                decoded
                    .go_test_coverage
                    .expect("coverage")
                    .passed_tests
                    .is_empty()
            );
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
            "{\"Action\":\"start\",\"Package\":\"a\"} {\"Action\":\"skip\",\"Package\":\"a\"}"
                .to_owned(),
        ] {
            assert!(inspect(&bad).is_err(), "{bad}");
        }
        for event in [
            json!({"Action":"run","Package":"a","Test":""}),
            json!({"Action":"cont","Package":"a","Test":"missing"}),
            json!({"Action":"pass","Package":"a","Test":"missing"}),
            json!({"Action":"output","Package":"a"}),
            json!({"Action":"bench","Package":"a","Test":"TestNotABenchmark"}),
            json!({"Action":"fail","Package":"a","Elapsed":-1}),
            json!({"Action":"pass","Package":"a","FailedBuild":"b"}),
            json!({"Action":"output","Package":"a","Output":"ok","OutputType":"unknown"}),
        ] {
            assert!(
                inspect(&stream(&[
                    json!({"Action":"start","Package":"a"}),
                    event.clone()
                ]))
                .is_err(),
                "{event}"
            );
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
        while events.len() < MAX_GO_RECORDS {
            events.insert(1, json!({"Action":"output","Package":"a","Output":""}));
        }
        assert!(inspect(&stream(&events)).is_ok());
        events.insert(1, json!({"Action":"output","Package":"a","Output":""}));
        assert!(inspect(&stream(&events)).is_err());
        assert!(
            inspect(&format!(
                "{}{}",
                stream(&failed()),
                " ".repeat(MAX_SCANNED_OUTPUT_BYTES)
            ))
            .is_err()
        );
    }

    fn build_failure() -> Vec<Value> {
        vec![
            json!({"Action":"build-output","ImportPath":"example.org/cache [example.org/cache.test]","Output":"PRIVATE_BUILD_LOG_SENTINEL"}),
            json!({"Action":"build-fail","ImportPath":"example.org/cache [example.org/cache.test]"}),
            json!({"Action":"start","Package":"example.org/cache"}),
            json!({"Action":"fail","Package":"example.org/cache","FailedBuild":"example.org/cache [example.org/cache.test]"}),
        ]
    }

    fn passing(package: &str) -> Vec<Value> {
        vec![
            json!({"Action":"start","Package":package}),
            json!({"Action":"run","Package":package,"Test":"TestRoundTrip"}),
            json!({"Action":"pass","Package":package,"Test":"TestRoundTrip"}),
            json!({"Action":"pass","Package":package}),
        ]
    }

    #[test]
    fn build_events_keep_failure_identity_without_persisting_build_output() {
        let decoded = parsed(&build_failure());
        assert!(decoded.failed);
        assert!(!decoded.allows_completion);
        assert_eq!(decoded.diagnostics.len(), 1);
        assert!(
            decoded.diagnostics[0]
                .message_template
                .starts_with("go build failure:")
        );
        assert!(!format!("{:?}", decoded.diagnostics).contains("private_build_log_sentinel"));
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert!(coverage.complete);
        assert_eq!(coverage.failed_packages.len(), 1);
        assert!(coverage.failed_tests.is_empty());
        let standalone = parsed(&[json!({"Action":"build-fail","ImportPath":"example.org/cache"})]);
        assert!(standalone.failed);
        assert_eq!(standalone.diagnostics.len(), 1);
        assert!(!standalone.go_test_coverage.expect("coverage").complete);
    }

    #[test]
    fn build_identity_needs_an_explicit_link_not_a_similar_package_name() {
        for link in [
            "example.org/cache",
            "example.org/other [example.org/cache.test]",
        ] {
            let mut events = build_failure();
            events.last_mut().expect("package failure")["FailedBuild"] = json!(link);
            let decoded = parsed(&events);
            assert!(decoded.failed);
            assert!(!decoded.go_test_coverage.expect("coverage").complete);
        }
        // A clean unrelated package cannot stand in for the failed build.
        let mut events = build_failure()[..2].to_vec();
        events.extend(passing("example.org/other"));
        let decoded = parsed(&events);
        assert!(decoded.failed);
        assert!(!decoded.allows_completion);
        assert!(!decoded.go_test_coverage.expect("coverage").complete);
    }

    #[test]
    fn build_output_can_interleave_but_never_supplies_success_by_itself() {
        let warning = json!({"Action":"build-output","ImportPath":"example.org/dependency","Output":"a compiler warning"});
        let decoded = parsed(std::slice::from_ref(&warning));
        assert!(!decoded.failed);
        assert!(!decoded.allows_completion);
        let mut events = passing("example.org/cache");
        events.insert(2, warning);
        let output = ToolOutput {
            text: stream(&events),
            ..ToolOutput::default()
        };
        assert!(parsed(&events).allows_completion);
        assert!(!output.succeeded());
        assert!(
            ToolOutput {
                exit_code: Some(0),
                ..output
            }
            .succeeded()
        );
    }

    #[test]
    fn malformed_or_cross_namespace_build_records_reject_the_whole_stream() {
        for text in [
            r#"{"Action":"build-fail","ImportPath":"a","Package":"a"}"#,
            r#"{"Action":"build-fail","ImportPath":"a","ImportPath":"b"}"#,
            r#"{"Action":"build-output","ImportPath":"a"}"#,
            r#"{"Action":"build-fail","ImportPath":"a","Output":"failed"}"#,
            r#"{"Action":"build-pass","ImportPath":"a"}"#,
            r#"{"Action":"build-fail","ImportPath":null}"#,
            r#"{"Action":"build-fail","ImportPath":" "}"#,
            r#"{"Action":null,"Package":"a"}"#,
        ] {
            assert!(inspect(text).is_err(), "{text}");
            let complete_prefix = format!("{}\n{text}", stream(&passing("a")));
            assert!(inspect(&complete_prefix).is_err());
        }
    }

    #[test]
    fn modern_benchmark_runs_finish_at_the_package_boundary() {
        // Event shape reproduced with Go 1.23.2: benchmarks have run events
        // and timing output but no individual pass/bench terminal.
        let events = [
            json!({"Action":"start","Package":"a"}),
            json!({"Action":"run","Package":"a","Test":"TestCache"}),
            json!({"Action":"pass","Package":"a","Test":"TestCache"}),
            json!({"Action":"run","Package":"a","Test":"BenchmarkCache"}),
            json!({"Action":"run","Package":"a","Test":"BenchmarkCache"}),
            json!({"Action":"output","Package":"a","Test":"BenchmarkCache","Output":"PRIVATE_BENCHMARK_OUTPUT"}),
            json!({"Action":"run","Package":"a","Test":"BenchmarkNested"}),
            json!({"Action":"run","Package":"a","Test":"BenchmarkNested/small"}),
            json!({"Action":"pass","Package":"a"}),
        ];
        let decoded = parsed(&events);
        assert!(!decoded.failed);
        assert!(decoded.allows_completion);
        assert!(decoded.diagnostics.is_empty());
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert!(coverage.complete);
        let expected: BTreeSet<_> = [
            "TestCache",
            "BenchmarkCache",
            "BenchmarkNested",
            "BenchmarkNested/small",
        ]
        .into_iter()
        .map(|name| commitment("a", Some(name)))
        .collect();
        assert_eq!(coverage.passed_tests, expected);
        let output = ToolOutput {
            text: stream(&events),
            ..ToolOutput::default()
        };
        assert!(
            !output.succeeded(),
            "still needs independent process completion"
        );
        assert!(
            ToolOutput {
                exit_code: Some(0),
                ..output
            }
            .succeeded()
        );
    }

    #[test]
    fn legacy_benchmark_terminals_need_no_run_and_preserve_exact_identity() {
        let events = [
            json!({"Action":"start","Package":"a"}),
            json!({"Action":"output","Package":"a","Test":"BenchmarkCache-8","Output":"benchmark log"}),
            json!({"Action":"bench","Package":"a","Test":"BenchmarkCache-8"}),
            json!({"Action":"bench","Package":"a","Test":"BenchmarkCache-8"}),
            json!({"Action":"pass","Package":"a"}),
        ];
        let decoded = parsed(&events);
        assert!(decoded.allows_completion);
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert_eq!(coverage.passed_tests.len(), 1);
        assert!(
            coverage
                .passed_tests
                .contains(&commitment("a", Some("BenchmarkCache-8")))
        );
        assert!(
            !coverage
                .passed_tests
                .contains(&commitment("a", Some("BenchmarkCache")))
        );
        assert!(
            !coverage
                .passed_tests
                .contains(&commitment("a", Some("TestCache")))
        );
        assert!(
            !coverage
                .passed_tests
                .contains(&commitment("b", Some("BenchmarkCache-8")))
        );
    }

    #[test]
    fn benchmark_failures_survive_successful_iterations_and_other_benchmarks() {
        for with_run in [false, true] {
            let mut events = vec![json!({"Action":"start","Package":"a"})];
            if with_run {
                events.push(json!({"Action":"run","Package":"a","Test":"BenchmarkCache"}));
            }
            events.extend([
                json!({"Action":"fail","Package":"a","Test":"BenchmarkCache"}),
                json!({"Action":"bench","Package":"a","Test":"BenchmarkCache"}),
                json!({"Action":"run","Package":"a","Test":"BenchmarkOther"}),
                json!({"Action":"fail","Package":"a"}),
            ]);
            let decoded = parsed(&events);
            assert!(decoded.failed);
            assert!(!decoded.allows_completion);
            assert_eq!(decoded.diagnostics.len(), 1);
            assert_eq!(
                structured_error_diagnostics(&stream(&events)),
                decoded.diagnostics
            );
            let coverage = decoded.go_test_coverage.expect("coverage");
            assert!(coverage.complete);
            assert!(
                coverage
                    .failed_tests
                    .contains(&commitment("a", Some("BenchmarkCache")))
            );
            assert!(
                !coverage
                    .passed_tests
                    .contains(&commitment("a", Some("BenchmarkOther")))
            );
            *events.last_mut().expect("terminal") = json!({"Action":"pass","Package":"a"});
            assert!(
                inspect(&stream(&events)).is_err(),
                "failure cannot become a clean pass"
            );
        }
    }

    #[test]
    fn benchmark_output_skips_and_incomplete_runs_do_not_manufacture_proof() {
        for middle in [
            vec![
                json!({"Action":"output","Package":"a","Test":"BenchmarkCache","Output":"PASS BenchmarkCache"}),
            ],
            vec![json!({"Action":"skip","Package":"a","Test":"BenchmarkCache"})],
            vec![
                json!({"Action":"skip","Package":"a","Test":"BenchmarkCache"}),
                json!({"Action":"run","Package":"a","Test":"BenchmarkCache"}),
            ],
        ] {
            let mut events = vec![json!({"Action":"start","Package":"a"})];
            events.extend(middle);
            events.push(json!({"Action":"pass","Package":"a"}));
            let decoded = parsed(&events);
            assert!(!decoded.allows_completion);
            assert!(
                decoded
                    .go_test_coverage
                    .expect("coverage")
                    .passed_tests
                    .is_empty()
            );
        }
        let incomplete = parsed(&[
            json!({"Action":"start","Package":"a"}),
            json!({"Action":"run","Package":"a","Test":"BenchmarkCache"}),
        ]);
        assert!(!incomplete.allows_completion);
        assert!(!incomplete.go_test_coverage.expect("coverage").complete);
        for name in ["TestCache", "TestCache/BenchmarkInner", "Benchmarker"] {
            let events = [
                json!({"Action":"start","Package":"a"}),
                json!({"Action":"run","Package":"a","Test":name}),
                json!({"Action":"pass","Package":"a"}),
            ];
            assert!(
                inspect(&stream(&events)).is_err(),
                "not a benchmark: {name}"
            );
        }
        for action in ["pause", "cont"] {
            let events = [
                json!({"Action":"start","Package":"a"}),
                json!({"Action":"run","Package":"a","Test":"BenchmarkCache"}),
                json!({"Action":action,"Package":"a","Test":"BenchmarkCache"}),
            ];
            assert!(inspect(&stream(&events)).is_err());
        }
        assert!(
            inspect(&stream(&[
                json!({"Action":"start","Package":"a"}),
                json!({"Action":"bench","Package":"a"}),
            ]))
            .is_err()
        );
    }

    #[test]
    fn go_subject_names_are_redacted_before_diagnostic_keying() {
        let credential = format!("ghp_{}", "Q".repeat(36));
        let secret_test = format!("api_key={credential}");
        let decoded = parsed(&[
            json!({"Action":"start","Package":"example.org/cache"}),
            json!({"Action":"run","Package":"example.org/cache","Test":secret_test}),
            json!({"Action":"fail","Package":"example.org/cache","Test":secret_test}),
            json!({"Action":"fail","Package":"example.org/cache"}),
        ]);
        assert_eq!(decoded.diagnostics.len(), 1);
        let rendered = format!("{:?}", decoded.diagnostics);
        assert!(!rendered.contains(&credential));
        assert!(!rendered.contains(&credential.to_ascii_lowercase()));
    }

    #[test]
    fn real_store_go_build_repair_rejects_wrong_package_then_records_cited_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::core::cass_error_recall::record_session_error_recall;
        use crate::core::error_diagnosis::error_recall_report;
        use crate::db::{
            CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
            EvidenceProducerKind,
        };

        const WS: &str = "wsp_01234567890123456789012345";
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        db.insert_workspace(
            WS,
            &CreateWorkspaceInput {
                path: "/tmp/cass-go-build-recall".to_owned(),
                name: None,
            },
        )?;
        let session =
            crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(0x60_2000)).to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: WS.to_owned(),
                cass_session_id: "/sessions/go-build.jsonl".to_owned(),
                source_path: None,
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 0,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"go-build").to_hex()),
                metadata_json: None,
            },
        )?;
        let append = |number: u32,
                      kind: &str,
                      role: &str,
                      record: Value|
         -> Result<String, crate::db::DbError> {
            let id = crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(
                0x60_2000 + u128::from(number),
            ))
            .to_string();
            let excerpt = record.to_string();
            db.insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: WS.to_owned(),
                    session_id: session.clone(),
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("go-build:{number}"),
                    span_kind: kind.to_owned(),
                    start_line: number,
                    end_line: number,
                    start_byte: None,
                    end_byte: None,
                    role: Some(role.to_owned()),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                    excerpt,
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )?;
            Ok(id)
        };
        let call = |number, id| {
            append(
                number,
                "tool_call",
                "assistant",
                json!({
                    "type":"assistant","message":{"role":"assistant","content":[{
                        "type":"tool_use","id":id,"name":"Bash","input":{"command":"go test -json ./..."}
                    }]}
                }),
            )
        };
        let result = |number, id, output: String, is_error| {
            append(
                number,
                "tool_result",
                "user",
                json!({
                    "type":"user","message":{"role":"user","content":[{
                        "type":"tool_result","tool_use_id":id,"content":output,"is_error":is_error
                    }]}
                }),
            )
        };
        call(1, "failure")?;
        let failing = build_failure();
        let diagnostic = parsed(&failing).diagnostics.remove(0);
        let failure_id = result(2, "failure", stream(&failing), true)?;
        let repair_id = append(
            3,
            "message",
            "assistant",
            json!({
                "type":"assistant","message":{"role":"assistant","content":[{
                    "type":"text","text":"Fixed the cache build failure by adding the missing Cache type in cache.go."
                }]}
            }),
        )?;
        call(4, "wrong-retry")?;
        let wrong = result(
            5,
            "wrong-retry",
            stream(&passing("example.org/other")),
            false,
        )?;
        let report = record_session_error_recall(&db, WS, &session)?;
        assert_eq!(report.failures_seen, 1);
        assert_eq!(report.resolved_failures, 0);
        assert_eq!(report.incident_cards_recorded, 0);
        assert!(
            error_recall_report(&db, WS, &diagnostic)?
                .helpful_repairs
                .is_empty()
        );

        call(6, "valid-retry")?;
        let proof = result(
            7,
            "valid-retry",
            stream(&passing("example.org/cache")),
            false,
        )?;
        let report = record_session_error_recall(&db, WS, &session)?;
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.incident_cards_recorded, 1);
        let recall = error_recall_report(&db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.contains(&repair_id));
        assert_eq!(recall.proof_links, vec![proof]);
        assert!(!recall.proof_links.contains(&wrong));
        let card = crate::core::incident_card::incident_card_id(WS, &failure_id);
        assert!(db.get_search_admitted_evidence_span(&card, WS)?.is_some());
        assert_eq!(
            record_session_error_recall(&db, WS, &session)?.incident_cards_recorded,
            0
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "cass_error_recall_go_intake_tests.rs"]
mod intake_tests;
