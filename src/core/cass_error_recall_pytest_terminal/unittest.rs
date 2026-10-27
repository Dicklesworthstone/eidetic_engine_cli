//! CPython TextTestRunner reports in the shared CASS diagnostic/repair lane.
//!
//! Tracebacks, captured output and descriptions are never diagnostic classes.
//! Quiet reports can identify failures; only a complete verbose inventory can
//! verify a retry. Producer summaries cannot replace process completion.
//! Class/module fixtures are error holders, not tests. Setup failures retain
//! diagnostics without inventing the tests they prevented from running;
//! teardown repairs require the complete prior executed inventory to pass.

use std::collections::BTreeMap;

use super::{
    CanonicalDiagnostic, Decoded, Execution, MAX_DIAGNOSTICS_PER_FAILURE, MAX_LINES,
    MAX_SCANNED_OUTPUT_BYTES, MAX_TESTS, Subject, terminal,
};

const ERROR_SEPARATOR: &str =
    "======================================================================";
const RUN_SEPARATOR: &str =
    "----------------------------------------------------------------------";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Passed,
    Failed,
    Error,
    Skipped,
    ExpectedFailure,
    UnexpectedSuccess,
}

impl Status {
    fn index(self) -> usize {
        match self {
            Self::Passed => 0,
            Self::Failed => 1,
            Self::Error => 2,
            Self::Skipped => 3,
            Self::ExpectedFailure => 4,
            Self::UnexpectedSuccess => 5,
        }
    }

    fn failed(self) -> bool {
        matches!(self, Self::Failed | Self::Error | Self::UnexpectedSuccess)
    }
}

/// Accept a complete ordinary TestCase description, not a traceback or a
/// suffix of a subtest. Keep the display identity verbatim: differences between
/// Python versions may miss a match, but cannot invent test equivalence.
fn description_parts(text: &str) -> Option<(&str, &str)> {
    let (method, qualified) = text.split_once(" (")?;
    let qualified = qualified.strip_suffix(')')?;
    let identifier = |value: &str| {
        let mut chars = value.chars();
        chars
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_alphanumeric() || ch == '_')
    };
    (identifier(method) && qualified.split('.').all(identifier)).then_some((method, qualified))
}

fn test_identity(text: &str) -> Option<&str> {
    let (_, qualified) = description_parts(text)?;
    (qualified.contains('.') && fixture_phase(text).is_none()).then_some(text)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixturePhase {
    Setup,
    Teardown,
}

/// CPython's _ErrorHolder uses a class or module name, with no test-method
/// suffix. A module can be a single identifier (including __main__). Keep the
/// entire display identity for diagnostics; never guess a module/class split
/// from the dotted name of a successful test.
fn fixture_phase(text: &str) -> Option<FixturePhase> {
    let (method, qualified) = description_parts(text)?;
    match method {
        "setUpClass" if qualified.contains('.') => Some(FixturePhase::Setup),
        "tearDownClass" if qualified.contains('.') => Some(FixturePhase::Teardown),
        "setUpModule" => Some(FixturePhase::Setup),
        "tearDownModule" => Some(FixturePhase::Teardown),
        _ => None,
    }
}

fn node_identity(text: &str) -> Option<&str> {
    fixture_phase(text)
        .map(|_| text)
        .or_else(|| test_identity(text))
}

fn progress(line: &str) -> Option<(&str, Status)> {
    let (description, status) = line.rsplit_once(" ... ")?;
    let node = node_identity(description)?;
    Some((node, progress_status(status)?))
}

fn progress_status(status: &str) -> Option<Status> {
    Some(match status {
        "ok" => Status::Passed,
        "FAIL" => Status::Failed,
        "ERROR" => Status::Error,
        "expected failure" => Status::ExpectedFailure,
        "unexpected success" => Status::UnexpectedSuccess,
        other
            if other.strip_prefix("skipped ").is_some_and(|reason| {
                (reason.starts_with('\'') && reason.ends_with('\''))
                    || (reason.starts_with('"') && reason.ends_with('"'))
            }) =>
        {
            Status::Skipped
        }
        _ => return None,
    })
}

/// A failed subtest names its parent plus a display-only parameter suffix.
/// Record the parent test's error class without interpreting Python repr or
/// leaking parameters. A normal parent pass does not prove that exact subtest
/// ran again: such reports deliberately have no complete proof inventory.
fn error_header(line: &str) -> Option<(&str, Status, bool)> {
    for (prefix, status) in [
        ("FAIL: ", Status::Failed),
        ("ERROR: ", Status::Error),
        ("UNEXPECTED SUCCESS: ", Status::UnexpectedSuccess),
    ] {
        if let Some(description) = line.strip_prefix(prefix) {
            if let Some(node) = node_identity(description) {
                return Some((node, status, true));
            }
            let boundary = description.find(") ")?;
            let node = test_identity(&description[..=boundary])?;
            let suffix = &description[boundary + 2..];
            let subtest = !suffix.chars().any(char::is_control)
                && ((suffix.starts_with('(') && suffix.ends_with(')'))
                    || (suffix.starts_with('[') && suffix.ends_with([']', ')'])));
            return subtest.then_some((node, status, false));
        }
    }
    None
}

fn count(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn ran(line: &str) -> Option<u64> {
    let (number, rest) = line.strip_prefix("Ran ")?.split_once(' ')?;
    let number = count(number)?;
    let duration = if number == 1 {
        rest.strip_prefix("test in ")?
    } else {
        rest.strip_prefix("tests in ")?
    };
    let seconds = duration.strip_suffix('s')?.parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some(number)
}

struct Footer {
    failed: bool,
    no_tests: bool,
    counts: [u64; 6],
}

fn footer(line: &str) -> Option<Footer> {
    let (label, details) = match line.split_once(" (") {
        Some((label, details)) => (label, Some(details.strip_suffix(')')?)),
        None => (line, None),
    };
    let failed = match label {
        "OK" | "NO TESTS RAN" => false,
        "FAILED" => true,
        _ => return None,
    };
    let mut counts = [0; 6];
    let mut seen = [false; 6];
    if let Some(details) = details {
        for counter in details.split(", ") {
            let (name, value) = counter.split_once('=')?;
            let index = match name {
                "failures" => 1,
                "errors" => 2,
                "skipped" => 3,
                "expected failures" => 4,
                "unexpected successes" => 5,
                _ => return None,
            };
            if seen[index] {
                return None;
            }
            seen[index] = true;
            counts[index] = count(value)?;
        }
    }
    let counted_failure = counts[1] > 0 || counts[2] > 0 || counts[5] > 0;
    (failed == counted_failure).then_some(Footer {
        failed,
        no_tests: label == "NO TESTS RAN",
        counts,
    })
}

fn subject(node: &str) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.unittest.proof_subject.v1\0");
    hash.update(&(node.len() as u64).to_le_bytes());
    hash.update(node.as_bytes());
    *hash.finalize().as_bytes()
}

fn diagnostic(node: &str) -> CanonicalDiagnostic {
    // Redact BEFORE keying. Traceback messages, locals and parameter values
    // never become reusable error classes or public excerpts.
    let safe = crate::policy::redact_secret_like_content(node).content;
    let key = blake3::hash(safe.as_bytes()).to_hex();
    CanonicalDiagnostic {
        tool: crate::core::error_recall::DiagnosticTool::Shell,
        canonical_code: None,
        message_template: crate::core::error_recall::canonical_message_template(&format!(
            "python unittest test failure {key} {safe}"
        )),
    }
}

pub(super) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let visible_first = match terminal::normalize(first) {
        Ok(first) => first,
        Err(()) => return Ok(None),
    };
    let verbose = progress(&visible_first).is_some()
        || test_identity(&visible_first).is_some()
        || visible_first
            .strip_suffix(" ... ")
            .and_then(test_identity)
            .is_some();
    let quiet =
        !visible_first.is_empty() && visible_first.bytes().all(|byte| b".FEsxu".contains(&byte));
    let headed = visible_first == ERROR_SEPARATOR || visible_first == RUN_SEPARATOR;
    // Dots and separators are ordinary output unless the producer's run line
    // is also present. A pytest footer does not select this decoder.
    if !verbose && (!(quiet || headed) || !text.lines().any(|line| line.starts_with("Ran "))) {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || text.lines().count() > MAX_LINES {
        return Err(());
    }
    let visible = terminal::normalize(text)?;
    let lines: Vec<_> = visible.lines().collect();
    let last = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .ok_or(())?;
    let summary = footer(lines[last]);
    if summary.is_none()
        && ["OK", "FAILED", "NO TESTS RAN"].iter().any(|label| {
            lines[last] == *label
                || lines[last]
                    .strip_prefix(*label)
                    .is_some_and(|tail| tail.starts_with(" ("))
        })
    {
        return Err(());
    }
    let mut ran_count = None;
    let mut inventory_open = true;
    let mut ambiguous = false;
    let mut nodes = BTreeMap::<&str, Status>::new();
    let mut details = BTreeMap::<&str, Status>::new();
    let mut fixture_progress = BTreeMap::<&str, u64>::new();
    let mut fixture_details = BTreeMap::<&str, u64>::new();
    let mut observed = [0_u64; 6];
    let mut description = None;
    for (index, line) in lines.iter().enumerate() {
        // TextTestRunner places the first docstring line between the test ID
        // and its status. It is a single adjacent display line, never a new
        // test identity, a diagnostic message or an independently trusted row.
        if let Some((node, started_at)) = description.take() {
            let displayed_status = line
                .rsplit_once(" ... ")
                .filter(|(text, _)| !text.trim().is_empty() && progress(line).is_none())
                .and_then(|(_, status)| progress_status(status));
            if let Some(status) = displayed_status
                && started_at + 1 == index
            {
                if observed.iter().sum::<u64>() >= MAX_TESTS as u64
                    || nodes.insert(node, status).is_some()
                {
                    return Err(());
                }
                observed[status.index()] += 1;
                continue;
            }
            ambiguous = true;
        }
        if line.trim().is_empty() {
            continue;
        }
        if index == last && summary.is_some() {
            break;
        }
        if footer(line).is_some() {
            // Concatenated runs or a trailing failure cannot be a clean prefix.
            return Err(());
        }
        if line.starts_with("Ran ") {
            if ran_count.is_some() || index == 0 || lines[index - 1] != RUN_SEPARATOR {
                return Err(());
            }
            ran_count = Some(ran(line).ok_or(())?);
            inventory_open = false;
            continue;
        }
        if *line == ERROR_SEPARATOR || *line == RUN_SEPARATOR {
            inventory_open = false;
            continue;
        }
        if inventory_open && verbose {
            if let Some((node, status)) = progress(line) {
                if observed.iter().sum::<u64>() >= MAX_TESTS as u64 {
                    return Err(());
                }
                let repeated_fixture = fixture_phase(node).is_some() && status == Status::Error;
                if let Some(previous) = nodes.insert(node, status)
                    && !(repeated_fixture && previous == Status::Error)
                {
                    return Err(());
                }
                if repeated_fixture {
                    *fixture_progress.entry(node).or_default() += 1;
                }
                observed[status.index()] += 1;
            } else if let Some(node) = test_identity(line) {
                description = Some((node, index));
            } else {
                ambiguous = true;
            }
        }
        if index > 0 && lines[index - 1] == ERROR_SEPARATOR {
            let Some((node, status, complete_subject)) = error_header(line) else {
                ambiguous = true;
                continue;
            };
            ambiguous |= !complete_subject;
            if details.len() == MAX_TESTS && !details.contains_key(node) {
                return Err(());
            }
            let repeated_fixture = fixture_phase(node).is_some() && status == Status::Error;
            if let Some(previous) = details.insert(node, status)
                && complete_subject
                && !(repeated_fixture && previous == Status::Error)
            {
                return Err(());
            }
            if repeated_fixture {
                if fixture_details.values().sum::<u64>() >= MAX_TESTS as u64 {
                    return Err(());
                }
                *fixture_details.entry(node).or_default() += 1;
            }
        }
    }
    // Each failed class cleanup can report the same _ErrorHolder again. Keep
    // multiplicity for summary/proof checks but one diagnostic per identity.
    // Test identities still cannot repeat. SkipTest is also a native fixture
    // outcome (including teardown), but never supplies completion authority.
    for (node, status) in nodes.iter().chain(details.iter()) {
        if fixture_phase(node).is_some() && !matches!(status, Status::Error | Status::Skipped) {
            return Err(());
        }
    }
    for (node, status) in &details {
        match nodes.get(node) {
            Some(progress_status) if progress_status != status => return Err(()),
            None if verbose => ambiguous = true,
            _ => {}
        }
    }
    let mut failures: BTreeMap<_, _> = nodes
        .iter()
        .filter(|(_, status)| status.failed())
        .map(|(node, status)| (*node, *status))
        .collect();
    failures.extend(details.iter().map(|(node, status)| (*node, *status)));
    if let Some(summary) = &summary {
        if summary.no_tests && ran_count != Some(0) {
            return Err(());
        }
        let mut known = [0_u64; 6];
        for (node, status) in &failures {
            // Progress and detail sections describe the same callbacks, not
            // two independent failures. Neither may contradict the footer.
            let occurrences = fixture_progress
                .get(node)
                .copied()
                .unwrap_or(0)
                .max(fixture_details.get(node).copied().unwrap_or(0))
                .max(1);
            known[status.index()] += occurrences;
        }
        if [1, 2, 5]
            .iter()
            .any(|index| known[*index] > summary.counts[*index])
        {
            return Err(());
        }
    }
    let mut execution = Execution::default();
    let has_fixture = nodes.keys().any(|node| fixture_phase(node).is_some());
    let fixture_withheld = nodes.iter().any(|(node, status)| {
        fixture_phase(node)
            .is_some_and(|phase| phase == FixturePhase::Setup || *status == Status::Skipped)
    });
    let teardown_failed = nodes.iter().any(|(node, status)| {
        fixture_phase(node) == Some(FixturePhase::Teardown) && *status == Status::Error
    });
    let mut tests_run = 0_u64;
    for (node, status) in &nodes {
        if fixture_phase(node).is_some() {
            continue;
        }
        tests_run += 1;
        let name = subject(node);
        execution.inventory.insert(name);
        if status.failed() || (teardown_failed && *status != Status::Skipped) {
            // The private failed set is also a must-pass proof requirement.
            // A teardown can affect any executed subject in this run. Require
            // them all to pass on retry rather than inventing a fixture-test
            // alias or guessing ownership from dotted class/module names.
            execution.failed.insert(name);
        } else if *status == Status::Passed {
            execution.passed.insert(name);
        }
    }
    execution.complete = verbose
        && !ambiguous
        && !fixture_withheld
        && description.is_none()
        && ran_count == Some(tests_run)
        && (!has_fixture
            || nodes.iter().all(|(node, status)| {
                fixture_phase(node).is_none()
                    || *status == Status::Skipped
                    || fixture_progress.get(node) == fixture_details.get(node)
            }))
        && summary
            .as_ref()
            .is_some_and(|summary| observed[1..] == summary.counts[1..]);
    let failed = !failures.is_empty() || summary.as_ref().is_some_and(|summary| summary.failed);
    let allows_completion = execution.complete && !failed && observed[0] > 0;
    let symptom = failures.first_key_value().map(|(node, _)| {
        crate::policy::redact_secret_like_content(&format!("unittest failure in {node}")).content
    });
    let diagnostics = failures
        .keys()
        .take(MAX_DIAGNOSTICS_PER_FAILURE)
        .map(|node| diagnostic(node))
        .collect();
    Ok(Some(Decoded {
        diagnostics,
        symptom,
        failed,
        allows_completion,
        execution,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics,
    };

    const CACHE: &str = "test_cache (tests.test_cache.CacheTests.test_cache)";
    const CONTROL: &str = "test_control (tests.test_cache.CacheTests.test_control)";

    fn report(cache: &str) -> String {
        let (detail, summary) = match cache {
            "FAIL" => (
                format!(
                    "\n{ERROR_SEPARATOR}\nFAIL: {CACHE}\n{RUN_SEPARATOR}\nTraceback (most recent call last):\nAssertionError: private body\n"
                ),
                "FAILED (failures=1)",
            ),
            "ERROR" => (
                format!(
                    "\n{ERROR_SEPARATOR}\nERROR: {CACHE}\n{RUN_SEPARATOR}\nTraceback (most recent call last):\nValueError: private body\n"
                ),
                "FAILED (errors=1)",
            ),
            "skipped 'not exercised'" => (String::new(), "OK (skipped=1)"),
            "expected failure" => (String::new(), "OK (expected failures=1)"),
            _ => (String::new(), "OK"),
        };
        format!(
            "{CACHE} ... {cache}\n{CONTROL} ... ok\n{detail}\n{RUN_SEPARATOR}\nRan 2 tests in 0.001s\n\n{summary}\n"
        )
    }

    fn decoded(text: &str) -> Decoded {
        // Exercise the production dispatch, not just the leaf parser.
        super::super::decode(text)
            .expect("valid report")
            .expect("native unittest")
    }

    #[test]
    fn native_failures_and_matching_executed_retries_share_the_recall_lane() {
        for status in ["FAIL", "ERROR"] {
            let failure = decoded(&report(status));
            let proof = decoded(&report("ok"));
            assert!(failure.failed);
            assert!(!failure.allows_completion);
            assert!(failure.execution.has_known_failures());
            assert!(proof.allows_completion);
            assert!(failure.execution.verified_by(&proof.execution));
            let output = ToolOutput {
                text: report(status),
                is_error: Some(false),
                exit_code: Some(0),
            };
            assert!(
                output.failed(),
                "native failure overrides optimistic wrapper"
            );
            assert!(!output.succeeded());
            assert_eq!(
                failure_diagnostics(&output),
                structured_error_diagnostics(&output.text)
            );
            assert_eq!(failure.diagnostics.len(), 1);
        }
    }

    #[test]
    fn skipped_expected_and_unrelated_tests_cannot_certify_the_failed_test() {
        let failure = decoded(&report("FAIL"));
        for status in ["skipped 'not exercised'", "expected failure"] {
            let proof = decoded(&report(status));
            assert!(!failure.execution.verified_by(&proof.execution));
        }
        let unrelated =
            report("ok").replace(CACHE, "test_other (tests.test_cache.CacheTests.test_other)");
        assert!(
            !failure
                .execution
                .verified_by(&decoded(&unrelated).execution)
        );
        let quiet = format!("..\n{RUN_SEPARATOR}\nRan 2 tests in 0.001s\n\nOK\n");
        assert!(!decoded(&quiet).allows_completion);
        assert!(!failure.execution.verified_by(&decoded(&quiet).execution));
    }

    #[test]
    fn quiet_failure_is_recallable_but_has_no_invented_inventory() {
        let verbose = report("FAIL");
        let quiet = verbose.replacen(&format!("{CACHE} ... FAIL\n{CONTROL} ... ok"), "F.", 1);
        let quiet = decoded(&quiet);
        assert!(quiet.failed);
        assert!(!quiet.execution.has_known_failures());
        assert_eq!(quiet.diagnostics, decoded(&verbose).diagnostics);
        assert!(!quiet.symptom.unwrap().contains("private body"));
    }

    #[test]
    fn malformed_totals_duplicate_names_and_trailing_runs_never_prove_success() {
        let valid = report("ok");
        for malformed in [
            valid.replace("Ran 2 tests", "Ran 1 test"),
            valid.replace("0.001s", "NaNs"),
            valid.replace(CONTROL, CACHE),
            format!("{valid}Process exited with code 1.\n"),
            format!("{valid}{valid}"),
            valid.replace("OK", "OK (errors=1)"),
        ] {
            assert!(
                !matches!(super::super::decode(&malformed), Ok(Some(report)) if report.allows_completion)
            );
        }
        assert!(super::super::decode(&report("FAIL").replace("failures=1", "failures=0")).is_err());
    }

    #[test]
    fn process_completion_is_independent_of_the_complete_test_inventory() {
        let output = ToolOutput {
            text: report("ok"),
            is_error: Some(false),
            exit_code: Some(1),
        };
        assert!(!output.succeeded());
        let pending = ToolOutput {
            text: report("ok"),
            is_error: None,
            exit_code: None,
        };
        assert!(!pending.succeeded());
    }

    #[test]
    fn names_are_producer_scoped_and_bounded_reports_fail_closed() {
        assert_ne!(subject(CACHE), super::super::subject(CACHE));
        assert!(decode("unrelated stdout").unwrap().is_none());
        assert!(decode("F\n1 failed in 0.01s\n").unwrap().is_none());
        let too_large = format!("{}{}", report("FAIL"), "x".repeat(MAX_SCANNED_OUTPUT_BYTES));
        assert!(decode(&too_large).is_err());
        let too_many_lines = format!("{}{}", report("FAIL"), "\n".repeat(MAX_LINES));
        assert!(decode(&too_many_lines).is_err());
        let credential = format!("ghp_{}", "Q".repeat(36));
        let with_secret = report("FAIL").replace("test_cache", &credential);
        let failure = decoded(&with_secret);
        assert!(!failure.symptom.unwrap().contains(&credential));
        assert!(
            failure
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.message_template.contains(&credential))
        );
    }

    #[test]
    fn multiline_descriptions_preserve_test_identity_not_docstring_text() {
        let raw = report("FAIL")
            .replacen(
                &format!("{CACHE} ... FAIL"),
                &format!("{CACHE}\nA private description of the cache. ... FAIL"),
                1,
            )
            .replace(
                &format!("FAIL: {CACHE}\n"),
                &format!("FAIL: {CACHE}\nA private description of the cache.\n"),
            );
        let proof = report("ok").replacen(
            &format!("{CACHE} ... ok"),
            &format!("{CACHE}\nA changed description, not a changed test. ... ok"),
            1,
        );
        let failure = decoded(&raw);
        assert!(failure.execution.has_known_failures());
        assert_eq!(failure.diagnostics, decoded(&report("FAIL")).diagnostics);
        assert!(failure.execution.verified_by(&decoded(&proof).execution));
        assert!(!failure.symptom.unwrap().contains("private description"));
        // SGR colors and CRLF are rendering, not different test identities.
        let styled = proof
            .replace("ok", "\u{1b}[32mok\u{1b}[0m")
            .replace('\n', "\r\n");
        assert!(failure.execution.verified_by(&decoded(&styled).execution));
    }

    #[test]
    fn missing_or_ambiguous_docstring_status_cannot_skip_a_test() {
        for displayed in [
            "A description without a status.",
            "Extra line.\nA description. ... ok",
            "\nA description. ... ok",
            "test_other (tests.test_cache.CacheTests.test_other) ... ok",
        ] {
            let raw = report("ok").replacen(
                &format!("{CACHE} ... ok"),
                &format!("{CACHE}\n{displayed}"),
                1,
            );
            assert!(
                !matches!(super::super::decode(&raw), Ok(Some(report)) if report.allows_completion)
            );
        }
    }

    #[test]
    fn subtest_failures_recall_the_parent_without_claiming_parameter_coverage() {
        let secret = format!("ghp_{}", "Q".repeat(36));
        let suffix = format!("(token='{secret}')");
        let raw = report("FAIL")
            .replacen(
                &format!("{CACHE} ... FAIL"),
                &format!("{CACHE} ... \n  {CACHE} {suffix} ... FAIL"),
                1,
            )
            .replace(
                &format!("FAIL: {CACHE}\n"),
                &format!("FAIL: {CACHE} {suffix}\n"),
            );
        let failure = decoded(&raw);
        assert!(failure.failed);
        assert_eq!(failure.diagnostics, decoded(&report("FAIL")).diagnostics);
        assert!(!failure.execution.has_known_failures());
        assert!(
            !failure
                .execution
                .verified_by(&decoded(&report("ok")).execution)
        );
        assert!(!failure.symptom.unwrap().contains(&secret));
        let quiet = format!(
            "F.\n{ERROR_SEPARATOR}\nFAIL: {CACHE} {suffix}\n{RUN_SEPARATOR}\nignored traceback\n{RUN_SEPARATOR}\nRan 2 tests in 0.001s\n\nFAILED (failures=1)\n"
        );
        assert_eq!(decoded(&quiet).diagnostics, failure.diagnostics);
    }

    #[test]
    fn unexpected_success_is_a_failed_run_and_zero_tests_never_certify_repairs() {
        let raw = report("FAIL")
            .replace(" ... FAIL", " ... unexpected success")
            .replace("FAIL: ", "UNEXPECTED SUCCESS: ")
            .replace("failures=1", "unexpected successes=1");
        let failure = decoded(&raw);
        assert!(failure.failed);
        assert!(!failure.allows_completion);
        assert!(failure.execution.has_known_failures());
        assert!(
            failure
                .execution
                .verified_by(&decoded(&report("ok")).execution)
        );
        for result in ["OK", "NO TESTS RAN"] {
            let empty = format!("{RUN_SEPARATOR}\nRan 0 tests in 0.000s\n\n{result}\n");
            assert!(!decoded(&empty).allows_completion);
        }
    }

    // Native TextTestRunner _ErrorHolder shape: fixture errors contribute to
    // errors=, but never to Ran N. No traceback content supplies a subject.
    fn fixture_report(phase: &str, owner: &str, tests: &str, ran: u64) -> String {
        let node = format!("{phase} ({owner})");
        let progress = if phase.starts_with("setUp") {
            format!("{node} ... ERROR\n{tests}")
        } else {
            format!("{tests}{node} ... ERROR\n")
        };
        let units = if ran == 1 { "test" } else { "tests" };
        format!(
            "{progress}\n{ERROR_SEPARATOR}\nERROR: {node}\n{RUN_SEPARATOR}\nTraceback (most recent call last):\nRuntimeError: private fixture body\n\n{RUN_SEPARATOR}\nRan {ran} {units} in 0.001s\n\nFAILED (errors=1)\n"
        )
    }

    #[test]
    fn setup_fixtures_are_recallable_without_inventing_unexecuted_test_inventory() {
        for (phase, owner) in [
            ("setUpClass", "tests.test_cache.CacheTests"),
            ("setUpModule", "tests.test_cache"),
            ("setUpModule", "__main__"),
            ("setUpModule", "tests"),
        ] {
            let text = fixture_report(phase, owner, "", 0);
            let failure = decoded(&text);
            assert!(failure.failed, "{phase} ({owner})");
            assert_eq!(failure.diagnostics.len(), 1);
            assert!(!failure.allows_completion);
            assert!(!failure.execution.has_known_failures());
            assert!(failure.execution.inventory.is_empty());
            assert!(
                !failure
                    .execution
                    .verified_by(&decoded(&report("ok")).execution)
            );
            let output = ToolOutput {
                text,
                is_error: Some(false),
                exit_code: Some(0),
            };
            assert!(output.failed());
            assert!(!output.succeeded());
            assert_eq!(failure_diagnostics(&output), failure.diagnostics);
            assert_eq!(
                structured_error_diagnostics(&output.text),
                failure.diagnostics
            );
            assert!(!failure.symptom.unwrap().contains("private fixture body"));
        }
    }

    #[test]
    fn teardown_repairs_require_the_whole_executed_inventory_to_pass() {
        let tests = format!("{CACHE} ... ok\n{CONTROL} ... ok\n");
        for (phase, owner) in [
            ("tearDownClass", "tests.test_cache.CacheTests"),
            ("tearDownModule", "tests.test_cache"),
            ("tearDownModule", "__main__"),
        ] {
            let failure = decoded(&fixture_report(phase, owner, &tests, 2));
            assert!(failure.execution.has_known_failures());
            assert_eq!(failure.execution.inventory.len(), 2);
            assert_eq!(failure.execution.failed.len(), 2);
            assert!(
                failure
                    .execution
                    .verified_by(&decoded(&report("ok")).execution)
            );
            for retry in [
                report("skipped 'not exercised'"),
                report("expected failure"),
                report("ok").replace(CACHE, "test_other (tests.test_cache.Other.test_other)"),
                format!("{CONTROL} ... ok\n{RUN_SEPARATOR}\nRan 1 test in 0.001s\n\nOK\n"),
                format!("..\n{RUN_SEPARATOR}\nRan 2 tests in 0.001s\n\nOK\n"),
                format!("{RUN_SEPARATOR}\nRan 0 tests in 0.001s\n\nOK\n"),
            ] {
                assert!(
                    !failure.execution.verified_by(&decoded(&retry).execution),
                    "{retry}"
                );
            }
        }
    }

    #[test]
    fn fixture_proof_uses_the_production_requirement_and_process_completion_gate() {
        use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};

        let tests = format!("{CACHE} ... ok\n{CONTROL} ... ok\n");
        let failure = ToolOutput {
            text: fixture_report("tearDownClass", "tests.test_cache.CacheTests", &tests, 2),
            is_error: Some(true),
            exit_code: Some(1),
        };
        let required = Requirement::capture(&failure, true);
        assert!(required.is_guarded());
        for (is_error, exit_code, verified) in [
            (None, None, false),
            (Some(false), Some(1), false),
            (Some(false), Some(0), true),
        ] {
            let proof = ToolOutput {
                text: report("ok"),
                is_error,
                exit_code,
            };
            assert_eq!(required.is_verified_by(&Proof::capture(&proof)), verified);
        }
        let pending = Requirement::capture(&failure, false);
        let proof = ToolOutput {
            text: report("ok"),
            is_error: Some(false),
            exit_code: Some(0),
        };
        assert!(!pending.is_verified_by(&Proof::capture(&proof)));
    }

    #[test]
    fn another_class_passing_does_not_prove_a_withheld_setup_was_repaired() {
        let tests = format!("{CONTROL} ... ok\n");
        let text = fixture_report("setUpClass", "tests.hidden.HiddenTests", &tests, 1);
        let failure = decoded(&text);
        assert!(failure.failed);
        assert!(!failure.execution.has_known_failures());
        assert!(
            !failure
                .execution
                .verified_by(&decoded(&report("ok")).execution)
        );
        let skipped = format!(
            "setUpModule (tests.hidden) ... skipped 'not available'\n{tests}\n{RUN_SEPARATOR}\nRan 1 test in 0.001s\n\nOK (skipped=1)\n"
        );
        assert!(!decoded(&skipped).allows_completion);
    }

    #[test]
    fn fixture_count_mismatch_missing_details_and_impossible_statuses_refuse_proof() {
        let tests = format!("{CACHE} ... ok\n{CONTROL} ... ok\n");
        let text = fixture_report("tearDownModule", "tests.test_cache", &tests, 2);
        for malformed in [
            text.replace("Ran 2 tests", "Ran 3 tests"),
            text.replace(
                "ERROR: tearDownModule (tests.test_cache)",
                "ERROR: tearDownModule (other)",
            ),
            text.replace(
                "ERROR: tearDownModule (tests.test_cache)",
                "an unrecognized detail",
            ),
            format!("{text}another command is still running\n"),
            text.replace(" ... ERROR", " ... ok"),
            text.replace("ERROR", "FAIL")
                .replace("errors=1", "failures=1"),
        ] {
            assert!(
                !matches!(super::super::decode(&malformed), Ok(Some(report)) if report.execution.has_known_failures() || report.allows_completion),
                "{malformed}"
            );
        }
        let duplicate = text.replacen(
            "tearDownModule (tests.test_cache) ... ERROR\n",
            "tearDownModule (tests.test_cache) ... ERROR\ntearDownModule (tests.test_cache) ... ERROR\n",
            1,
        );
        assert!(super::super::decode(&duplicate).is_err());
    }

    #[test]
    fn quiet_and_redacted_module_fixtures_supply_diagnostics_but_not_repair_authority() {
        let text = fixture_report("setUpModule", "__main__", "", 0);
        let quiet = text.replacen("setUpModule (__main__) ... ERROR", "E", 1);
        let failure = decoded(&quiet);
        assert_eq!(failure.diagnostics, decoded(&text).diagnostics);
        assert!(!failure.execution.has_known_failures());
        let credential = format!("ghp_{}", "Q".repeat(36));
        let sensitive = decoded(&fixture_report("setUpModule", &credential, "", 0));
        assert!(!sensitive.symptom.unwrap().contains(&credential));
        assert!(sensitive.diagnostics.iter().all(|diagnostic| {
            !diagnostic.message_template.contains(&credential)
                && !diagnostic.message_template.contains("private fixture body")
        }));
    }

    fn cleanup_detail(number: usize) -> String {
        format!(
            "{ERROR_SEPARATOR}\nERROR: tearDownClass (tests.test_cache.CacheTests)\n{RUN_SEPARATOR}\nTraceback (most recent call last):\nRuntimeError: private cleanup {number}\n\n"
        )
    }

    fn repeated_cleanup_report() -> String {
        let progress = "tearDownClass (tests.test_cache.CacheTests) ... ERROR\n";
        format!(
            "{CACHE} ... ok\n{CONTROL} ... ok\n{progress}{progress}\n{}{}{RUN_SEPARATOR}\nRan 2 tests in 0.001s\n\nFAILED (errors=2)\n",
            cleanup_detail(0),
            cleanup_detail(1),
        )
    }

    #[test]
    fn multiple_class_cleanup_errors_keep_one_class_and_require_all_executed_tests() {
        let text = repeated_cleanup_report();
        let failure = decoded(&text);
        assert!(failure.failed);
        assert!(failure.execution.has_known_failures());
        assert_eq!(failure.execution.inventory.len(), 2);
        assert_eq!(failure.execution.failed.len(), 2);
        assert_eq!(failure.diagnostics.len(), 1);
        let single = fixture_report(
            "tearDownClass",
            "tests.test_cache.CacheTests",
            &format!("{CACHE} ... ok\n{CONTROL} ... ok\n"),
            2,
        );
        assert_eq!(failure.diagnostics, decoded(&single).diagnostics);
        assert!(
            failure
                .execution
                .verified_by(&decoded(&report("ok")).execution)
        );
        assert!(!failure.symptom.unwrap().contains("private cleanup"));
    }

    #[test]
    fn repeated_fixture_counts_do_not_relax_test_duplicates_or_detail_reconciliation() {
        let text = repeated_cleanup_report();
        assert!(super::super::decode(&text.replace("errors=2", "errors=1")).is_err());
        let missing_detail = text.replace(&cleanup_detail(0), "");
        let incomplete = decoded(&missing_detail);
        assert!(incomplete.failed);
        assert!(!incomplete.execution.has_known_failures());
        let duplicate_test = text.replacen(
            &format!("{CACHE} ... ok\n"),
            &format!("{CACHE} ... ok\n{CACHE} ... ok\n"),
            1,
        );
        assert!(super::super::decode(&duplicate_test).is_err());
        let mixed = text.replacen(
            "tearDownClass (tests.test_cache.CacheTests) ... ERROR",
            "tearDownClass (tests.test_cache.CacheTests) ... skipped 'ambiguous callback'",
            1,
        );
        assert!(super::super::decode(&mixed).is_err());
    }

    #[test]
    fn a_skipped_teardown_retains_test_failures_but_never_certifies_completion() {
        let skipped = "tearDownClass (tests.test_cache.CacheTests) ... skipped 'not completed'\n";
        let text = report("FAIL")
            .replacen(ERROR_SEPARATOR, &format!("{skipped}\n{ERROR_SEPARATOR}"), 1)
            .replace("FAILED (failures=1)", "FAILED (failures=1, skipped=1)");
        let failure = decoded(&text);
        assert!(failure.failed);
        assert_eq!(failure.diagnostics, decoded(&report("FAIL")).diagnostics);
        assert!(!failure.execution.has_known_failures());
        let passed = report("ok")
            .replacen(RUN_SEPARATOR, &format!("{skipped}\n{RUN_SEPARATOR}"), 1)
            .replace("\nOK\n", "\nOK (skipped=1)\n");
        let proof = decoded(&passed);
        assert!(!proof.failed);
        assert!(!proof.allows_completion);
    }

    #[test]
    fn repeated_cleanup_callbacks_are_bounded_even_under_one_unique_identity() {
        let progress = "tearDownClass (m.C) ... ERROR\n".repeat(MAX_TESTS + 1);
        let text = format!(
            "{progress}\n{RUN_SEPARATOR}\nRan 0 tests in 0.001s\n\nFAILED (errors={})\n",
            MAX_TESTS + 1,
        );
        assert!(text.len() < MAX_SCANNED_OUTPUT_BYTES);
        assert!(text.lines().count() < MAX_LINES);
        assert!(super::super::decode(&text).is_err());
    }
}
