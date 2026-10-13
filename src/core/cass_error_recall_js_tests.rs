//! Complete Jest-compatible JSON reports (including Vitest's JSON reporter).
//!
//! Only typed assertion failures and file-level execution errors are mined.
//! Names, source paths, snapshots, metadata and stack frames are not diagnostic
//! text. Every counter and record is checked before returning any fingerprints.
//! A report can veto success, but cannot establish process completion itself.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};
use super::compiler::CompilerOutput;

const MAX_TEST_RESULTS: usize = 1024;

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(true)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Family {
    #[serde(default, deserialize_with = "present")]
    test_results: bool,
    #[serde(default, deserialize_with = "present")]
    num_total_tests: bool,
    #[serde(default, deserialize_with = "present")]
    num_failed_tests: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    num_total_test_suites: u64,
    num_passed_test_suites: u64,
    num_failed_test_suites: u64,
    num_pending_test_suites: u64,
    num_total_tests: u64,
    num_passed_tests: u64,
    num_failed_tests: u64,
    num_pending_tests: u64,
    num_todo_tests: u64,
    start_time: f64,
    success: bool,
    test_results: Vec<FileResult>,
    #[serde(default)]
    was_interrupted: bool,
    #[serde(default)]
    num_runtime_error_test_suites: u64,
    snapshot: Option<Snapshot>,
    // Reject a conflicting native/compiler/linter envelope even when null.
    #[serde(default, deserialize_with = "present")]
    schema: bool,
    #[serde(default, deserialize_with = "present")]
    reason: bool,
    #[serde(default, rename = "$message_type", deserialize_with = "present")]
    message_type: bool,
    #[serde(default, deserialize_with = "present")]
    general_diagnostics: bool,
    #[serde(default, deserialize_with = "present")]
    results: bool,
}

#[derive(Deserialize)]
struct Snapshot {
    failure: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileResult {
    name: String,
    status: String,
    message: String,
    start_time: f64,
    end_time: f64,
    assertion_results: Vec<Assertion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Assertion {
    ancestor_titles: Vec<String>,
    full_name: String,
    title: String,
    status: String,
    failure_messages: Option<Vec<String>>,
}

fn counter_sum(values: &[u64]) -> Result<u64, ()> {
    values
        .iter()
        .try_fold(0_u64, |sum, value| sum.checked_add(*value).ok_or(()))
}

fn time(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

/// Diagnostic and coverage consumers use the same complete-document validator.
pub(in crate::core::cass_error_recall) fn inspect(text: &str) -> Result<Option<CompilerOutput>, ()> {
    decode(text).map(|decoded| decoded.map(|decoded| decoded.output))
}

type Subject = [u8; 32];

/// Transient exact source identities. Never persisted or exposed as provenance.
#[derive(Default)]
pub(super) struct Execution {
    inventory: BTreeSet<Subject>,
    failed: BTreeSet<Subject>,
    passed: BTreeSet<Subject>,
    complete: bool,
}

impl Execution {
    pub(super) fn has_known_failures(&self) -> bool {
        self.complete && !self.failed.is_empty()
    }

    pub(super) fn verified_by(&self, proof: &Self) -> bool {
        self.has_known_failures()
            && proof.complete
            && self.inventory == proof.inventory
            && self.failed.is_subset(&proof.passed)
    }
}

pub(super) struct Decoded {
    pub output: CompilerOutput,
    pub execution: Execution,
}

fn subject(parts: &[&str]) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.javascript-test.proof_subject.v1\0");
    hash.update(&(parts.len() as u64).to_le_bytes());
    for part in parts {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    *hash.finalize().as_bytes()
}

/// Probe only root fields, then reparse the original bytes into typed structs:
/// a probe must never erase duplicate fields or salvage a truncated document.
pub(super) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    if !text.trim_start().starts_with('{') {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    let family = serde_json::Deserializer::from_str(text)
        .into_iter::<Family>()
        .next()
        .ok_or(())?
        .map_err(|_| ())?;
    if !family.test_results && !(family.num_total_tests && family.num_failed_tests) {
        return Ok(None);
    }
    let report: Report = serde_json::from_str(text).map_err(|_| ())?;
    if report.schema
        || report.reason
        || report.message_type
        || report.general_diagnostics
        || report.results
        || !time(report.start_time)
        || report.test_results.len() > MAX_TEST_RESULTS
        || report.num_total_tests > MAX_TEST_RESULTS as u64
        || report.num_total_test_suites < report.test_results.len() as u64
        || counter_sum(&[
            report.num_passed_test_suites,
            report.num_failed_test_suites,
            report.num_pending_test_suites,
        ])? != report.num_total_test_suites
        || report.num_runtime_error_test_suites > report.num_failed_test_suites
    {
        return Err(());
    }
    let mut output = CompilerOutput::default();
    let mut execution = Execution {
        complete: !report.was_interrupted
            && report.num_runtime_error_test_suites == 0
            && report.num_pending_test_suites == 0
            && !report.snapshot.as_ref().is_some_and(|snapshot| snapshot.failure),
        ..Execution::default()
    };
    let mut counts = [0_u64; 4];
    let mut files = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut seen = 0_usize;
    let mut failed_files = 0_u64;
    for file in &report.test_results {
        if file.name.trim().is_empty()
            || !files.insert(file.name.as_str())
            || !time(file.start_time)
            || !time(file.end_time)
            || file.end_time < file.start_time
        {
            return Err(());
        }
        seen = seen.checked_add(file.assertion_results.len()).ok_or(())?;
        if seen > MAX_TEST_RESULTS {
            return Err(());
        }
        execution.inventory.insert(subject(&["file", &file.name]));
        let mut names = BTreeSet::new();
        let mut file_failed = false;
        let mut file_executed = false;
        for assertion in &file.assertion_results {
            let full_name = assertion
                .ancestor_titles
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(assertion.title.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            if assertion.title.trim().is_empty()
                || assertion.full_name != full_name
                || !names.insert(assertion.full_name.as_str())
            {
                return Err(());
            }
            let category = match assertion.status.as_str() {
                "passed" => 0,
                "failed" => 1,
                "pending" | "skipped" | "disabled" => 2,
                "todo" => 3,
                _ => return Err(()),
            };
            // Component boundaries matter: ["a b", "c"] must not alias
            // ["a", "b c"], even though their flattened fullName is equal.
            let mut identity = vec!["test", file.name.as_str()];
            identity.extend(assertion.ancestor_titles.iter().map(String::as_str));
            identity.push(assertion.title.as_str());
            let identity = subject(&identity);
            execution.inventory.insert(identity);
            if category == 0 {
                execution.passed.insert(identity);
            } else if category == 1 {
                execution.failed.insert(identity);
            }
            counts[category] += 1;
            file_executed |= category < 2;
            if category == 1 {
                file_failed = true;
                let message = assertion
                    .failure_messages
                    .as_ref()
                    .and_then(|messages| messages.iter().find(|message| !message.trim().is_empty()))
                    .map(String::as_str)
                    .unwrap_or("test assertion failed");
                retain(&mut output, &mut keys, message);
            } else if assertion
                .failure_messages
                .as_ref()
                .is_some_and(|messages| messages.iter().any(|message| !message.trim().is_empty()))
            {
                // Retry/salvage output must not erase an earlier failed attempt.
                return Err(());
            }
        }
        match file.status.as_str() {
            "failed" => {
                failed_files += 1;
                if !file_failed {
                    // A setup/runtime failure does not reveal the selected
                    // assertion inventory. Keep its diagnostic, but do not
                    // pretend a later unrelated green file proves its repair.
                    execution.complete = false;
                    retain(
                        &mut output,
                        &mut keys,
                        if file.message.trim().is_empty() {
                            "test file execution failed"
                        } else {
                            &file.message
                        },
                    );
                }
            }
            "passed" if !file_failed => {}
            "pending" | "skipped" | "disabled" if !file_executed => {}
            _ => return Err(()),
        }
    }
    if counts
        != [
            report.num_passed_tests,
            report.num_failed_tests,
            report.num_pending_tests,
            report.num_todo_tests,
        ]
        || counter_sum(&counts)? != report.num_total_tests
        || failed_files > report.num_failed_test_suites
    {
        return Err(());
    }
    // Vitest counts nested describe suites, while testResults contains files.
    // Their totals need not match. Failed suites, interruption, runtime errors
    // and snapshot failure independently veto an optimistic outer wrapper.
    output.failed |= !report.success
        || report.num_failed_test_suites > 0
        || report.was_interrupted
        || report.num_runtime_error_test_suites > 0
        || report.snapshot.as_ref().is_some_and(|snapshot| snapshot.failure);
    output.allows_completion = !output.failed
        && report.num_passed_tests > 0
        && report.num_pending_test_suites == 0;
    Ok(Some(Decoded { output, execution }))
}

fn retain(output: &mut CompilerOutput, keys: &mut BTreeSet<String>, message: &str) {
    // Use the first diagnostic line, not assertion identity, code frames, or
    // stack frames. Redaction and canonicalization precede fingerprinting.
    let first = message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("test failed");
    let safe = crate::policy::redact_secret_like_content(first).content;
    let diagnostic = CanonicalDiagnostic {
        tool: crate::core::error_recall::DiagnosticTool::Shell,
        canonical_code: None,
        message_template: crate::core::error_recall::canonical_message_template(&format!(
            "javascript-test: {safe}"
        )),
    };
    output.failed = true;
    if keys.insert(diagnostic.layered_key().key)
        && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
    {
        output.diagnostics.push(diagnostic);
    }
}

#[cfg(test)]
#[path = "cass_error_recall_js_tests_tests.rs"]
mod tests;
