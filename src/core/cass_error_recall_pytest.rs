//! Bounded pytest-json-report intake for the CASS failure-to-repair loop.
//!
//! Decode the original complete document, not a generic JSON value that loses
//! duplicate fields. Diagnostic text comes only from a failed stage's crash
//! message; environment, metadata, captured streams and tracebacks are ignored.
//! Test coverage remains transient and independent of process completion.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};
use super::compiler::CompilerOutput;

const MAX_RESULTS: usize = 1024;
type Subject = [u8; 32];

#[path = "cass_error_recall_pytest_collection.rs"]
mod collection;

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(true)
}

#[derive(Deserialize)]
struct Family {
    #[serde(default, deserialize_with = "present")]
    root: bool,
    #[serde(default, deserialize_with = "present")]
    exitcode: bool,
    #[serde(default, deserialize_with = "present")]
    tests: bool,
    #[serde(default, deserialize_with = "present")]
    summary: bool,
}

#[derive(Deserialize)]
struct Report {
    created: f64,
    duration: f64,
    exitcode: u8,
    root: String,
    summary: Summary,
    tests: Option<Vec<Test>>,
    collectors: Option<Vec<Collector>>,
    #[serde(default, deserialize_with = "present")]
    schema: bool,
    #[serde(default, deserialize_with = "present")]
    reason: bool,
    #[serde(default, rename = "$message_type", deserialize_with = "present")]
    message_type: bool,
    #[serde(default, rename = "testResults", deserialize_with = "present")]
    javascript: bool,
    #[serde(default, rename = "generalDiagnostics", deserialize_with = "present")]
    pyright: bool,
    #[serde(default, deserialize_with = "present")]
    results: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    collected: u64,
    total: u64,
    #[serde(default)]
    deselected: u64,
    #[serde(default)]
    passed: u64,
    #[serde(default)]
    failed: u64,
    #[serde(default)]
    error: u64,
    #[serde(default)]
    skipped: u64,
    #[serde(default)]
    xfailed: u64,
    #[serde(default)]
    xpassed: u64,
}

impl Summary {
    fn outcomes(&self) -> [u64; 6] {
        [
            self.passed,
            self.failed,
            self.error,
            self.skipped,
            self.xfailed,
            self.xpassed,
        ]
    }
}

#[derive(Deserialize)]
struct Test {
    nodeid: String,
    outcome: String,
    setup: Option<Stage>,
    call: Option<Stage>,
    teardown: Option<Stage>,
}

#[derive(Deserialize)]
struct Stage {
    duration: f64,
    outcome: String,
    crash: Option<Crash>,
}

#[derive(Deserialize)]
struct Crash {
    message: String,
}

#[derive(Deserialize)]
struct Collector {
    nodeid: String,
    outcome: String,
    result: Option<Vec<CollectedNode>>,
}

#[derive(Deserialize)]
struct CollectedNode {
    nodeid: String,
}

#[derive(Default)]
pub(super) struct Execution {
    root: Subject,
    inventory: BTreeSet<Subject>,
    failed: BTreeSet<Subject>,
    passed: BTreeSet<Subject>,
    failed_collectors: BTreeSet<Subject>,
    executed_collectors: BTreeSet<Subject>,
    collection_only: bool,
    complete: bool,
}

impl Execution {
    pub(super) fn has_known_failures(&self) -> bool {
        (self.complete && !self.failed.is_empty())
            || (self.collection_only && !self.failed_collectors.is_empty())
    }

    pub(super) fn verified_by(&self, proof: &Self) -> bool {
        if !self.has_known_failures()
            || !proof.complete
            || !proof.failed.is_empty()
            || self.root != proof.root
        {
            return false;
        }
        if self.collection_only {
            // Collection failed before test execution, so there is no selected
            // test inventory to compare. Each failed collector must explicitly
            // own a normally passing descendant in the clean retry instead.
            self.failed_collectors.is_subset(&proof.executed_collectors)
        } else {
            // Node IDs include the file, class and parameter identity. A fix
            // may add regression tests without losing coverage of the original
            // run. Never accept a removed subject or a skipped failed test.
            self.inventory.is_subset(&proof.inventory) && self.failed.is_subset(&proof.passed)
        }
    }
}

pub(super) struct Decoded {
    pub output: CompilerOutput,
    pub execution: Execution,
}

fn commitment(kind: &str, value: &str) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.pytest.proof_subject.v1\0");
    for part in [kind, value] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    *hash.finalize().as_bytes()
}

fn valid_time(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

pub(in crate::core::cass_error_recall) fn inspect(
    text: &str,
) -> Result<Option<CompilerOutput>, ()> {
    decode(text).map(|decoded| decoded.map(|decoded| decoded.output))
}

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
    if !(family.exitcode && (family.root || family.tests || family.summary)) {
        return Ok(None);
    }
    let report: Report = serde_json::from_str(text).map_err(|_| ())?;
    if !valid_time(report.created)
        || !valid_time(report.duration)
        || !valid_identity(&report.root)
        || report.exitcode > 5
        || report.schema
        || report.reason
        || report.message_type
        || report.javascript
        || report.pyright
        || report.results
    {
        return Err(());
    }
    let total = report
        .summary
        .outcomes()
        .into_iter()
        .try_fold(0_u64, |sum, count| sum.checked_add(count).ok_or(()))?;
    let selected = report
        .summary
        .collected
        .checked_sub(report.summary.deselected)
        .ok_or(())?;
    if total != report.summary.total || total > selected {
        return Err(());
    }
    let mut execution = Execution {
        root: commitment("root", &report.root),
        complete: report.exitcode <= 1 && total == selected,
        ..Execution::default()
    };
    let mut output = CompilerOutput::default();
    let mut keys = BTreeSet::new();
    let mut unlocated_collection_failure = false;
    if let Some(collectors) = &report.collectors {
        if collectors.len() > MAX_RESULTS {
            return Err(());
        }
        let mut names = BTreeSet::new();
        for collector in collectors {
            // pytest's root collector legitimately has an empty nodeid.
            if collector.nodeid.chars().any(char::is_control)
                || !names.insert(collector.nodeid.as_str())
            {
                return Err(());
            }
            match collector.outcome.as_str() {
                "failed" => {
                    execution.complete = false;
                    if valid_identity(&collector.nodeid) {
                        execution
                            .failed_collectors
                            .insert(commitment("collector", &collector.nodeid));
                    } else {
                        unlocated_collection_failure = true;
                    }
                    retain(&mut output, &mut keys, "collection", "collection failed");
                }
                "passed" | "skipped" => {}
                _ => return Err(()),
            }
        }
    }
    let mut counts = [0_u64; 6];
    if let Some(tests) = &report.tests {
        if tests.len() > MAX_RESULTS || tests.len() as u64 != total {
            return Err(());
        }
        for test in tests {
            if !valid_identity(&test.nodeid) {
                return Err(());
            }
            let subject = commitment("nodeid", &test.nodeid);
            if !execution.inventory.insert(subject) {
                return Err(());
            }
            let category = match test.outcome.as_str() {
                "passed" => 0,
                "failed" => 1,
                "error" => 2,
                "skipped" => 3,
                "xfailed" => 4,
                "xpassed" => 5,
                _ => return Err(()),
            };
            counts[category] += 1;
            let stages = [
                test.setup.as_ref(),
                test.call.as_ref(),
                test.teardown.as_ref(),
            ];
            for stage in stages.into_iter().flatten() {
                if !valid_time(stage.duration)
                    || !matches!(stage.outcome.as_str(), "passed" | "failed" | "skipped")
                    || stage
                        .crash
                        .as_ref()
                        .is_some_and(|crash| crash.message.trim().is_empty())
                    || (stage.outcome == "passed" && stage.crash.is_some())
                {
                    return Err(());
                }
            }
            let stage_is = |index: usize, expected: &str| {
                stages[index].is_some_and(|stage| stage.outcome == expected)
            };
            // A call cannot run after a failed/skipped setup. Missing lifecycle
            // stages retain diagnostics but never certify completed coverage.
            if test.call.is_some() && !stage_is(0, "passed") {
                return Err(());
            }
            execution.complete &= test.setup.is_some()
                && test.teardown.is_some()
                && (!stage_is(0, "passed") || test.call.is_some());
            match category {
                0 | 5
                    if stages
                        .into_iter()
                        .flatten()
                        .any(|stage| stage.outcome != "passed") =>
                {
                    return Err(());
                }
                1 if !stage_is(1, "failed") || stage_is(2, "failed") => return Err(()),
                2 if !stage_is(0, "failed") && !stage_is(2, "failed") => return Err(()),
                3 | 4
                    if !stages
                        .into_iter()
                        .flatten()
                        .any(|stage| stage.outcome == "skipped") =>
                {
                    return Err(());
                }
                _ => {}
            }
            if category == 0
                && stages
                    .into_iter()
                    .all(|stage| stage.is_some_and(|stage| stage.outcome == "passed"))
            {
                execution.passed.insert(subject);
            }
            // A teardown skip/xfail can overwrite pytest's aggregate outcome
            // after the call failed. Failed phases still own diagnostics and
            // subject requirements; a later benign label cannot erase them.
            if stages
                .into_iter()
                .flatten()
                .any(|stage| stage.outcome == "failed")
            {
                execution.failed.insert(subject);
                for (name, stage) in ["setup", "call", "teardown"].into_iter().zip(stages) {
                    if let Some(stage) = stage.filter(|stage| stage.outcome == "failed") {
                        retain(
                            &mut output,
                            &mut keys,
                            name,
                            stage
                                .crash
                                .as_ref()
                                .map_or("test stage failed", |crash| crash.message.as_str()),
                        );
                    }
                }
            }
        }
        if counts != report.summary.outcomes() {
            return Err(());
        }
    } else {
        // --json-report-summary intentionally has no subject inventory.
        execution.complete = false;
    }
    // A normal collection abort uses exit 2 and may have discovered unrelated
    // tests without running them. Only the observed failed collectors can be
    // covered; mixed executions, summary-only runs and unidentified root errors
    // remain unproven. Existing test-failure inventory rules are unchanged.
    execution.collection_only = matches!(report.exitcode, 1 | 2)
        && total == 0
        && report.tests.as_ref().is_some_and(Vec::is_empty)
        && !execution.failed_collectors.is_empty()
        && !unlocated_collection_failure;
    execution.executed_collectors = collection::executed_collectors(
        report.collectors.as_deref().unwrap_or(&[]),
        &execution.passed,
    )?;
    output.failed |= report.exitcode != 0 || report.summary.failed > 0 || report.summary.error > 0;
    output.allows_completion = !output.failed && execution.complete && !execution.passed.is_empty();
    Ok(Some(Decoded { output, execution }))
}

fn retain(output: &mut CompilerOutput, keys: &mut BTreeSet<String>, stage: &str, message: &str) {
    let first = message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("test stage failed");
    let safe = crate::policy::redact_secret_like_content(first).content;
    let diagnostic = CanonicalDiagnostic {
        tool: crate::core::error_recall::DiagnosticTool::Shell,
        canonical_code: None,
        message_template: crate::core::error_recall::canonical_message_template(&format!(
            "pytest {stage}: {safe}"
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
#[path = "cass_error_recall_pytest_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cass_error_recall_pytest_native_tests.rs"]
mod native_tests;

#[cfg(test)]
#[path = "cass_error_recall_pytest_collection_tests.rs"]
mod collection_tests;

#[cfg(test)]
#[path = "cass_error_recall_pytest_growth_tests.rs"]
mod growth_tests;
