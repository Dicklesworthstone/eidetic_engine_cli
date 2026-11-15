//! Native `go test` terminal reports with package-bound diagnostic identity.
//!
//! The testing package prints test frames before cmd/go supplies the package
//! footer. Buffer only bounded subject names until that footer binds them.
//! Quiet failures expose useful identities, but omit passing controls: only a
//! complete verbose lifecycle can certify a repair. JSON and terminal reports
//! share diagnostic keys and transient coverage commitments.
//!
//! Grammar: Go's src/testing/testing.go (chattyPrinter, T.report) and
//! src/cmd/go/internal/test/test.go (runTestActor, printExitStatus).

use std::collections::{BTreeMap, BTreeSet};

use super::{CompilerOutput, GoTestCoverage, MAX_SCANNED_OUTPUT_BYTES, go};

const MAX_GO_LINES: usize = 2048;

#[derive(Clone, Copy, Eq, PartialEq)]
enum State {
    Running,
    Paused,
    Passed,
    Failed,
    Skipped,
}

impl State {
    fn terminal(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Skipped)
    }
}

struct Test {
    state: State,
    failed: bool,
    skipped: bool,
}

#[derive(Default)]
struct Package {
    tests: BTreeMap<String, Test>,
    summary: Option<bool>,
    incomplete: bool,
    other_output: bool,
}

impl Package {
    fn empty(&self) -> bool {
        self.tests.is_empty() && self.summary.is_none() && !self.other_output
    }

    fn observe(&mut self, name: &str, state: State) -> Result<(), ()> {
        if !identity(name) || self.summary.is_some() {
            return Err(());
        }
        if state == State::Running {
            match self.tests.get_mut(name) {
                Some(test) if test.state.terminal() => test.state = State::Running,
                Some(_) => return Err(()),
                None => {
                    self.tests.insert(
                        name.to_owned(),
                        Test {
                            state,
                            failed: false,
                            skipped: false,
                        },
                    );
                }
            }
            return Ok(());
        }
        let Some(test) = self.tests.get_mut(name) else {
            // Without -v, Go prints failing test names but neither RUN nor
            // passing controls. Retain the failure without inventing coverage.
            if state != State::Failed {
                return Err(());
            }
            self.incomplete = true;
            self.tests.insert(
                name.to_owned(),
                Test {
                    state,
                    failed: true,
                    skipped: false,
                },
            );
            return Ok(());
        };
        if test.state != State::Running {
            // Repeated quiet failures are real, but still have no inventory.
            if state == State::Failed && test.failed && self.incomplete {
                return Ok(());
            }
            return Err(());
        }
        test.state = state;
        test.failed |= state == State::Failed;
        test.skipped |= state == State::Skipped;
        Ok(())
    }

    fn resume(&mut self, name: &str) -> Result<(), ()> {
        let test = self.tests.get_mut(name).ok_or(())?;
        if self.summary.is_some() || test.state != State::Paused {
            return Err(());
        }
        test.state = State::Running;
        Ok(())
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Status {
    Passed,
    Failed,
    BuildFailed,
    Empty,
}

struct Footer<'a> {
    package: &'a str,
    status: Status,
    cached: bool,
    no_tests: bool,
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
}

fn decimal(value: &str) -> bool {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    !whole.is_empty()
        && !fraction.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

fn duration(value: &str) -> bool {
    value.strip_suffix('s').is_some_and(decimal)
}

fn coverage_line(line: &str) -> bool {
    let Some(body) = line.strip_prefix("coverage: ") else {
        return false;
    };
    if body == "[no statements]" {
        return true;
    }
    let Some((percent, suffix)) = body.split_once("% of statements") else {
        return false;
    };
    decimal(percent)
        && percent.parse::<f64>().is_ok_and(|value| value <= 100.0)
        && (suffix.is_empty() || suffix.strip_prefix(" in ").is_some_and(|s| !s.is_empty()))
}

fn footer(line: &str) -> Result<Option<Footer<'_>>, ()> {
    let (status, rest) = if let Some(rest) = line.strip_prefix("ok  \t") {
        (Status::Passed, rest)
    } else if let Some(rest) = line.strip_prefix("FAIL\t") {
        (Status::Failed, rest)
    } else if let Some(rest) = line.strip_prefix("?   \t") {
        (Status::Empty, rest)
    } else {
        return Ok(None);
    };
    let Some((package, detail)) = rest.split_once('\t') else {
        if status == Status::Failed {
            for (suffix, status) in [
                (" [build failed]", Status::BuildFailed),
                (" [setup failed]", Status::Failed),
            ] {
                if let Some(package) = rest.strip_suffix(suffix)
                    && identity(package)
                {
                    return Ok(Some(Footer {
                        package,
                        status,
                        cached: false,
                        no_tests: true,
                    }));
                }
            }
        }
        return Err(());
    };
    if !identity(package) {
        return Err(());
    }
    if status == Status::Empty {
        return if detail == "[no test files]" {
            Ok(Some(Footer {
                package,
                status,
                cached: false,
                no_tests: true,
            }))
        } else {
            Err(())
        };
    }
    let (detail, no_tests) = [" [no tests to run]", " [no fuzz tests to fuzz]"]
        .into_iter()
        .find_map(|suffix| detail.strip_suffix(suffix).map(|rest| (rest, true)))
        .unwrap_or((detail, false));
    let (elapsed, coverage) = detail.split_once('\t').unwrap_or((detail, ""));
    if (!duration(elapsed) && elapsed != "(cached)")
        || (!coverage.is_empty() && !coverage_line(coverage))
        || (status != Status::Passed && (elapsed == "(cached)" || no_tests))
    {
        return Err(());
    }
    Ok(Some(Footer {
        package,
        status,
        cached: elapsed == "(cached)",
        no_tests,
    }))
}

fn terminal(line: &str) -> Result<Option<(&str, State)>, ()> {
    let line = line.trim_start_matches(' ');
    let (rest, state) = if let Some(rest) = line.strip_prefix("--- PASS: ") {
        (rest, State::Passed)
    } else if let Some(rest) = line.strip_prefix("--- FAIL: ") {
        (rest, State::Failed)
    } else if let Some(rest) = line.strip_prefix("--- SKIP: ") {
        (rest, State::Skipped)
    } else {
        return Ok(None);
    };
    let (name, elapsed) = rest.rsplit_once(" (").ok_or(())?;
    if !identity(name) || !elapsed.strip_suffix(')').is_some_and(duration) {
        return Err(());
    }
    Ok(Some((name, state)))
}

fn family(line: &str) -> bool {
    line.starts_with("=== RUN   ")
        || line.trim_start_matches(' ').starts_with("--- FAIL: ")
        || line.starts_with("FAIL\t")
        || line.starts_with("ok  \t")
        || line.starts_with("?   \t")
}

pub(super) fn inspect(text: &str) -> Result<Option<CompilerOutput>, ()> {
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        // Select the producer before refusing its size. Other plain reports
        // retain their existing bounded diagnostic fallback; their completion
        // path independently refuses oversized output. Probe only the bounded
        // prefix, line by line, so a cut terminal escape cannot hide an earlier
        // Go frame and no oversized buffer is normalized or deserialized.
        let recognized = super::super::bounded(text).lines().any(|line| {
            family(line)
                || super::super::outcome::terminal_text(line)
                    .is_ok_and(|visible| visible.lines().any(family))
        });
        return if recognized { Err(()) } else { Ok(None) };
    }
    let text = match super::super::outcome::terminal_text(text) {
        Ok(text) => text,
        Err(()) if text.lines().any(family) => return Err(()),
        Err(()) => return Ok(None),
    };
    if !text.lines().any(family) {
        return Ok(None);
    }
    let mut output = CompilerOutput::default();
    let mut coverage = GoTestCoverage::default();
    let mut keys = BTreeSet::new();
    let mut packages = BTreeSet::new();
    let mut package = Package::default();
    let mut complete = true;
    let mut final_failure = false;
    for (index, line) in text.lines().enumerate() {
        if index == MAX_GO_LINES {
            return Err(());
        }
        if line.trim().is_empty() {
            continue;
        }
        if final_failure {
            return Err(());
        }
        if let Some(footer) = footer(line)? {
            if !packages.insert(footer.package.to_owned()) {
                return Err(());
            }
            let failed = matches!(footer.status, Status::Failed | Status::BuildFailed);
            let named_failure = package.tests.values().any(|test| test.failed);
            if (!failed && (named_failure || package.summary == Some(false)))
                || (footer.status == Status::Empty && !package.empty())
            {
                return Err(());
            }
            // The package footer alone cannot establish an executed inventory.
            // Cached replays and skipped/empty runs carry no new verification.
            complete &= footer.status == Status::Empty
                || (!package.incomplete
                    && !footer.cached
                    && !footer.no_tests
                    && !package.tests.is_empty()
                    && package.summary == Some(!failed)
                    && package.tests.values().all(|test| test.state.terminal()));
            let subject = go::commitment(footer.package, None);
            if failed {
                coverage.failed_packages.insert(subject);
                output.failed = true;
            } else if footer.status == Status::Passed {
                coverage.passed_packages.insert(subject);
            }
            for (name, test) in &package.tests {
                let subject = go::commitment(footer.package, Some(name));
                if test.failed {
                    coverage.failed_tests.insert(subject);
                    go::retain_diagnostic(
                        &mut output,
                        &mut keys,
                        go::diagnostic(footer.package, Some(name), false),
                    );
                } else if test.state == State::Passed && !test.skipped {
                    coverage.passed_tests.insert(subject);
                }
            }
            if failed && !named_failure {
                go::retain_diagnostic(
                    &mut output,
                    &mut keys,
                    go::diagnostic(footer.package, None, footer.status == Status::BuildFailed),
                );
            }
            package = Package::default();
            continue;
        }
        if let Some(name) = line.strip_prefix("=== RUN   ") {
            package.observe(name, State::Running)?;
        } else if let Some(name) = line.strip_prefix("=== PAUSE ") {
            package.observe(name, State::Paused)?;
        } else if let Some(name) = line.strip_prefix("=== CONT  ") {
            package.resume(name)?;
        } else if let Some(name) = line.strip_prefix("=== NAME  ") {
            if package.summary.is_some()
                || !package
                    .tests
                    .get(name)
                    .is_some_and(|test| test.state == State::Running)
            {
                return Err(());
            }
        } else if let Some((name, state)) = terminal(line)? {
            package.observe(name, state)?;
        } else if matches!(line, "PASS" | "FAIL") {
            if line == "FAIL" && package.empty() && output.failed {
                // cmd/go prints one final FAIL after the package-list report.
                final_failure = true;
            } else if package.summary.replace(line == "PASS").is_some() {
                return Err(());
            }
        } else if coverage_line(line) {
            package.other_output = true;
        } else {
            if package.summary.is_some()
                || (!packages.is_empty() && package.empty())
                || line.trim_start().starts_with("=== ")
                || line.trim_start().starts_with("--- ")
            {
                return Err(());
            }
            // Test logs are data. A prelude without any RUN frame cannot
            // establish that the visible report contains the whole inventory.
            package.incomplete |= package.tests.is_empty();
            package.other_output = true;
        }
    }
    coverage.complete = complete && !packages.is_empty() && package.empty();
    output.allows_completion =
        coverage.complete && !output.failed && !coverage.passed_tests.is_empty();
    output.go_test_coverage = Some(coverage);
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics, symptom_line,
    };

    const FAILED: &str = "=== RUN   TestCache\n    cache_test.go:4: PRIVATE_LOG_SENTINEL\n--- FAIL: TestCache (0.01s)\n=== RUN   TestControl\n--- PASS: TestControl (0.00s)\nFAIL\nFAIL\texample.org/cache\t0.010s\nFAIL\n";
    const PASSED: &str = "=== RUN   TestCache\n--- PASS: TestCache (0.01s)\n=== RUN   TestControl\n--- PASS: TestControl (0.00s)\nPASS\nok  \texample.org/cache\t0.012s\n";

    fn parsed(text: &str) -> CompilerOutput {
        super::super::inspect(text)
            .expect("valid native report")
            .expect("Go terminal report")
    }

    fn successful(text: &str) -> ToolOutput {
        ToolOutput {
            text: text.to_owned(),
            is_error: Some(false),
            exit_code: Some(0),
        }
    }

    #[test]
    fn quiet_failures_share_json_and_interactive_diagnostics_without_invented_coverage() {
        let quiet = "--- FAIL: TestCache (0.01s)\n    cache_test.go:4: PRIVATE_LOG_SENTINEL\nFAIL\nFAIL\texample.org/cache\t0.010s\nFAIL\n";
        let decoded = parsed(quiet);
        assert!(decoded.failed);
        assert_eq!(
            decoded.diagnostics,
            vec![go::diagnostic(
                "example.org/cache",
                Some("TestCache"),
                false
            )]
        );
        assert_eq!(structured_error_diagnostics(quiet), decoded.diagnostics);
        let output = successful(quiet);
        assert_eq!(failure_diagnostics(&output), decoded.diagnostics);
        assert!(output.failed());
        assert!(!output.succeeded());
        assert!(!format!("{:?}", decoded.diagnostics).contains("private_log_sentinel"));
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert!(!coverage.complete);
        assert_eq!(coverage.failed_packages.len(), 1);
        assert_eq!(coverage.failed_tests.len(), 1);
        assert!(coverage.passed_tests.is_empty());
    }

    #[test]
    fn verbose_inventory_keeps_passing_controls_and_requires_independent_exit() {
        let failed = parsed(FAILED);
        assert!(failed.go_test_coverage.as_ref().expect("coverage").complete);
        let coverage = failed.go_test_coverage.expect("coverage");
        assert!(
            coverage
                .passed_tests
                .contains(&go::commitment("example.org/cache", Some("TestControl")))
        );
        assert!(successful(PASSED).succeeded());
        let logged_error = PASSED.replace(
            "=== RUN   TestCache\n",
            "=== RUN   TestCache\nerror: expected invalid input\n",
        );
        assert!(successful(&logged_error).succeeded());
        let printed_exit = PASSED.replace(
            "=== RUN   TestCache\n",
            "=== RUN   TestCache\nProcess exited with code 0\n",
        );
        let output = ToolOutput {
            text: printed_exit,
            is_error: None,
            exit_code: None,
        };
        assert!(!output.succeeded());
    }

    #[test]
    fn parallel_subtests_multiple_packages_and_coverage_keep_exact_subjects() {
        let text = "?   \texample.org/helpers\t[no test files]\n=== RUN   TestParent\n=== RUN   TestParent/child\n=== PAUSE TestParent/child\n=== CONT  TestParent/child\n=== NAME  TestParent\n    parent_test.go:8: running child\n--- PASS: TestParent (0.01s)\n    --- PASS: TestParent/child (0.00s)\nPASS\ncoverage: 80.0% of statements\nok  \texample.org/cache\t0.012s\tcoverage: 80.0% of statements\n=== RUN   TestParent\n--- PASS: TestParent (0.01s)\nPASS\nok  \texample.org/other\t0.011s\n";
        let decoded = parsed(text);
        assert!(decoded.allows_completion);
        let coverage = decoded.go_test_coverage.expect("coverage");
        assert!(coverage.complete);
        assert_eq!(coverage.passed_packages.len(), 2);
        assert_eq!(coverage.passed_tests.len(), 3);
        for (package, test) in [
            ("example.org/cache", "TestParent"),
            ("example.org/cache", "TestParent/child"),
            ("example.org/other", "TestParent"),
        ] {
            assert!(
                coverage
                    .passed_tests
                    .contains(&go::commitment(package, Some(test)))
            );
        }
    }

    #[test]
    fn repeated_runs_cannot_erase_a_failure_or_a_skip() {
        let repeated_failure = FAILED.replace(
            "--- FAIL: TestCache (0.01s)\n",
            "--- FAIL: TestCache (0.01s)\n=== RUN   TestCache\n--- PASS: TestCache (0.01s)\n",
        );
        let decoded = parsed(&repeated_failure);
        assert!(decoded.failed);
        let coverage = decoded.go_test_coverage.expect("coverage");
        let subject = go::commitment("example.org/cache", Some("TestCache"));
        assert!(coverage.failed_tests.contains(&subject));
        assert!(!coverage.passed_tests.contains(&subject));
        let repeated_skip = PASSED.replace(
            "=== RUN   TestCache\n",
            "=== RUN   TestCache\n--- SKIP: TestCache (0.00s)\n=== RUN   TestCache\n",
        );
        let decoded = parsed(&repeated_skip);
        assert!(decoded.allows_completion);
        assert!(
            !decoded
                .go_test_coverage
                .expect("coverage")
                .passed_tests
                .contains(&subject)
        );
    }

    #[test]
    fn missing_skipped_cached_and_empty_execution_never_supply_complete_proof() {
        for text in [
            "ok  \texample.org/cache\t0.012s\n".to_owned(),
            "PASS\nok  \texample.org/cache\t0.012s [no tests to run]\n".to_owned(),
            "?   \texample.org/cache\t[no test files]\n".to_owned(),
            PASSED.replace("0.012s", "(cached)"),
            PASSED.replace("--- PASS:", "--- SKIP:"),
            PASSED.replace("--- PASS: TestCache (0.01s)\n", ""),
            PASSED.replace("PASS\nok", "ok"),
            PASSED.replace("ok  \texample.org/cache\t0.012s\n", ""),
        ] {
            assert!(!successful(&text).succeeded(), "{text}");
        }
    }

    #[test]
    fn malformed_native_reports_never_salvage_a_successful_prefix() {
        for text in [
            format!("{PASSED}unexpected tail\n"),
            format!("{PASSED}ok  \texample.org/cache\t0.013s\n"),
            PASSED.replace("0.012s", "NaNs"),
            PASSED.replace("--- PASS:", "--- UNKNOWN:"),
            PASSED.replace("=== RUN   TestCache", "=== CONT  TestCache"),
            PASSED.replace("=== RUN   TestCache\n", ""),
            PASSED.replace("--- PASS: TestCache", "--- FAIL: TestCache"),
            PASSED.replace("PASS\nok", "FAIL\nok"),
            PASSED.replace(
                "=== RUN   TestCache",
                "=== RUN   TestCache\n=== PAUSE TestCache",
            ),
            format!("{PASSED}\x1b[2J"),
        ] {
            assert!(super::super::inspect(&text).is_err(), "{text}");
            assert!(!successful(&text).succeeded(), "{text}");
            assert!(failure_diagnostics(&successful(&text)).is_empty(), "{text}");
        }
    }

    #[test]
    fn styling_is_only_a_view_and_summary_only_build_failure_has_no_test_proof() {
        let colored = FAILED.replace("FAIL", "\x1b[31mFAIL\x1b[0m");
        assert_eq!(parsed(&colored).diagnostics, parsed(FAILED).diagnostics);
        let build = parsed(
            "# example.org/cache\ncache.go:3: undefined: missing\nFAIL\texample.org/cache [build failed]\nFAIL\n",
        );
        assert!(build.failed);
        assert_eq!(
            build.diagnostics,
            vec![go::diagnostic("example.org/cache", None, true)]
        );
        assert!(!build.go_test_coverage.expect("coverage").complete);
    }

    #[test]
    fn oversized_go_refuses_proof_without_suppressing_other_bounded_diagnostics() {
        for prefix in [
            FAILED.to_owned(),
            "\x1b[32m=== RUN   TestCache\x1b[0m\n".to_owned(),
        ] {
            let oversized = format!("{prefix}{}", "x".repeat(MAX_SCANNED_OUTPUT_BYTES));
            assert!(super::super::inspect(&oversized).is_err());
            assert!(failure_diagnostics(&successful(&oversized)).is_empty());
            assert!(!successful(&oversized).succeeded());
        }

        let libtest = "running 1 test\ntest cache_flush ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let expected = failure_diagnostics(&successful(libtest));
        assert_eq!(expected.len(), 1);
        // The UTF-8 tail also exercises the shared byte-boundary selection.
        let oversized = format!("{libtest}{}", "λ".repeat(MAX_SCANNED_OUTPUT_BYTES));
        assert!(matches!(super::super::inspect(&oversized), Ok(None)));
        assert_eq!(failure_diagnostics(&successful(&oversized)), expected);
        assert_eq!(
            symptom_line(&successful(&oversized)),
            symptom_line(&successful(libtest))
        );
        assert!(!successful(&oversized).succeeded());
    }
}
