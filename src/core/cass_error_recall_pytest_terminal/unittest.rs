//! CPython TextTestRunner reports in the shared CASS diagnostic/repair lane.
//!
//! Tracebacks, captured output and descriptions are never diagnostic classes.
//! Quiet reports can identify failures; only a complete verbose inventory can
//! verify a retry. Producer summaries cannot replace process completion.

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
fn test_identity(text: &str) -> Option<&str> {
    let (method, qualified) = text.split_once(" (")?;
    let qualified = qualified.strip_suffix(')')?;
    let identifier = |value: &str| {
        let mut chars = value.chars();
        chars
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_alphanumeric() || ch == '_')
    };
    (identifier(method) && qualified.contains('.') && qualified.split('.').all(identifier))
        .then_some(text)
}

fn progress(line: &str) -> Option<(&str, Status)> {
    let (description, status) = line.rsplit_once(" ... ")?;
    let node = test_identity(description)?;
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
            if let Some(node) = test_identity(description) {
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
                if nodes.len() == MAX_TESTS || nodes.insert(node, status).is_some() {
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
                if nodes.len() == MAX_TESTS || nodes.insert(node, status).is_some() {
                    return Err(());
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
            if details.insert(node, status).is_some() && complete_subject {
                return Err(());
            }
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
    failures.extend(details);
    if let Some(summary) = &summary {
        if summary.no_tests && ran_count != Some(0) {
            return Err(());
        }
        let mut known = [0_u64; 6];
        for status in failures.values() {
            known[status.index()] += 1;
        }
        if [1, 2, 5]
            .iter()
            .any(|index| known[*index] > summary.counts[*index])
        {
            return Err(());
        }
    }
    let mut execution = Execution::default();
    for (node, status) in &nodes {
        let name = subject(node);
        execution.inventory.insert(name);
        if status.failed() {
            execution.failed.insert(name);
        } else if *status == Status::Passed {
            execution.passed.insert(name);
        }
    }
    execution.complete = verbose
        && !ambiguous
        && description.is_none()
        && ran_count == Some(nodes.len() as u64)
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
}
