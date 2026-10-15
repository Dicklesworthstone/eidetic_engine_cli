//! Native nextest terminal reports in the CASS failure/repair lane.
//!
//! A test identity includes its binary ID: equal test names in different
//! binaries are not interchangeable. Only reconciled, named executions can
//! prove a repair. Captured stdout, elapsed times and run UUIDs are not test
//! identities, and neither a producer summary nor a final PASS is an exit code.

use std::collections::{BTreeMap, BTreeSet};

use super::super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};

// Use the same conservative terminal interpretation as Python reports. In
// particular, cursor movement, erasure and bare CR must never erase a failure.
#[path = "cass_error_recall_pytest_terminal/terminal.rs"]
mod terminal;

const MAX_TESTS: usize = 1024;
const MAX_LINES: usize = 4096;
const SEPARATOR: &str = "────────────";
type Subject = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Passed,
    Failed,
    Skipped,
    Slow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Selection {
    tests: usize,
    binaries: usize,
    skipped: usize,
}

#[derive(Default)]
pub(in crate::core::cass_error_recall) struct Execution {
    selection: Option<Selection>,
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
            && proof.failed.is_empty()
            && self.selection == proof.selection
            && self.inventory == proof.inventory
            && self.failed.is_subset(&proof.passed)
    }
}

pub(in crate::core::cass_error_recall) struct Decoded {
    pub diagnostics: Vec<CanonicalDiagnostic>,
    pub failed: bool,
    pub allows_completion: bool,
    pub execution: Execution,
}

fn count(text: &str) -> Result<usize, ()> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    text.parse().map_err(|_| ())
}

fn number_with_unit(text: &str, singular: &str, plural: &str) -> Result<usize, ()> {
    let (number, unit) = text.split_once(' ').ok_or(())?;
    let number = count(number)?;
    // Older nextest versions also print plural units after one test.
    if unit != plural && !(number == 1 && unit == singular) {
        return Err(());
    }
    Ok(number)
}

fn selection(text: &str) -> Result<Selection, ()> {
    let rest = text.strip_prefix("Starting ").ok_or(())?;
    let (tests, rest) = rest.split_once(" across ").ok_or(())?;
    let tests = number_with_unit(tests, "test", "tests")?;
    let (binaries, skipped) = match rest.split_once(" (") {
        Some((binaries, skipped)) => {
            let skipped = skipped.strip_suffix(')').ok_or(())?;
            let skipped = skipped
                .strip_suffix(" tests skipped")
                .or_else(|| skipped.strip_suffix(" test skipped"))
                .or_else(|| skipped.strip_suffix(" skipped"))
                .ok_or(())?;
            (binaries, count(skipped)?)
        }
        None => (rest, 0),
    };
    let binaries = number_with_unit(binaries, "binary", "binaries")?;
    if tests > MAX_TESTS || (tests > 0 && binaries == 0) {
        return Err(());
    }
    Ok(Selection {
        tests,
        binaries,
        skipped,
    })
}

/// Nextest right-aligns status labels in a twelve-column field. Do not trim
/// arbitrary captured lines into producer rows or treat a nested libtest run
/// as another top-level suite.
fn producer_row(line: &str) -> Option<(&str, &str)> {
    let label = line.get(..12)?;
    let body = line.get(12..)?.strip_prefix(' ')?;
    let label = label.trim_start_matches(' ');
    (!label.is_empty() && !label.ends_with(' ')).then_some((label, body))
}

fn timed_body(body: &str, skipped: bool, slow: bool) -> Result<&str, ()> {
    let (time, rest) = body
        .strip_prefix('[')
        .and_then(|body| body.split_once("] "))
        .ok_or(())?;
    let time = time.trim();
    if skipped {
        if !time.is_empty() {
            return Err(());
        }
    } else {
        let time = if slow {
            time.strip_prefix('>').unwrap_or(time).trim_start()
        } else {
            time
        };
        let seconds = time.strip_suffix('s').ok_or(())?;
        if seconds.is_empty()
            || !seconds
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            || !seconds
                .parse::<f64>()
                .is_ok_and(|seconds| seconds.is_finite() && seconds >= 0.0)
        {
            return Err(());
        }
    }
    Ok(rest)
}

fn test_result<'a>(
    label: &str,
    body: &'a str,
    selected: Selection,
) -> Result<(&'a str, &'a str, Status), ()> {
    let status = match label {
        "PASS" => Status::Passed,
        "FAIL" | "TIMEOUT" | "ABORT" | "LEAK-FAIL" => Status::Failed,
        "SKIP" => Status::Skipped,
        "SLOW" => Status::Slow,
        _ => return Err(()),
    };
    let mut rest = timed_body(body, status == Status::Skipped, status == Status::Slow)?;
    // Recent producers annotate completion order. It is neither a binary ID
    // nor part of the test name. Stress-iteration labels are not supported.
    if let Some(ordinal) = rest.strip_prefix('(') {
        let (ordinal, tail) = ordinal.split_once(") ").ok_or(())?;
        let (finished, total) = ordinal.split_once('/').ok_or(())?;
        let finished = count(finished)?;
        if finished == 0 || finished > selected.tests || count(total)? != selected.tests {
            return Err(());
        }
        rest = tail;
    }
    let (binary, name) = rest.split_once(' ').ok_or(())?;
    if binary.is_empty()
        || binary.starts_with(['[', '('])
        || binary.chars().any(char::is_whitespace)
        || name.is_empty()
        || name != name.trim()
        || name.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok((binary, name, status))
}

#[derive(Clone, Copy)]
struct Summary {
    ran: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
}

fn summary(body: &str) -> Result<Summary, ()> {
    let rest = timed_body(body, false, false)?;
    let (run, counters) = rest.split_once(" run: ").ok_or(())?;
    let ran = number_with_unit(run, "test", "tests")?;
    let mut counts = BTreeMap::new();
    for counter in counters.split(", ") {
        let (number, name) = counter.split_once(' ').ok_or(())?;
        if !matches!(name, "passed" | "failed" | "skipped")
            || counts.insert(name, count(number)?).is_some()
        {
            return Err(());
        }
    }
    let passed = *counts.get("passed").ok_or(())?;
    let failed = counts.get("failed").copied().unwrap_or(0);
    let skipped = counts.get("skipped").copied().unwrap_or(0);
    if passed.checked_add(failed) != Some(ran) || ran > MAX_TESTS {
        return Err(());
    }
    Ok(Summary {
        ran,
        passed,
        failed,
        skipped,
    })
}

fn subject(binary: &str, name: &str) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.nextest.proof_subject.v1\0");
    for field in [binary, name] {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    *hash.finalize().as_bytes()
}

fn diagnostic(binary: &str, name: &str) -> CanonicalDiagnostic {
    let safe = crate::policy::redact_secret_like_content(&format!("{binary} {name}")).content;
    let key = blake3::hash(safe.as_bytes()).to_hex();
    // Key the redacted identity before the generic canonicalizer normalizes
    // numeric parameters. Never key captured panic values or secret text.
    super::super::from_cargo(None, &format!("nextest test failure {key} {safe}"))
}

fn is_run_id(line: &str) -> bool {
    let Some((id, profile)) = line
        .trim_start_matches(' ')
        .strip_prefix("Nextest run ID ")
        .and_then(|rest| rest.split_once(" with nextest profile: "))
    else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        && !profile.is_empty()
        && !profile.chars().any(char::is_whitespace)
}

/// None selects another family. A recognized but damaged nextest report is
/// Err, not permission to harvest its nested libtest stdout as independent
/// proof. Incomplete named runs can supply diagnostics, never completion.
pub(in crate::core::cass_error_recall) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    let recognized = text.contains("Nextest run ID ")
        || text.lines().any(|line| {
            line.contains("Starting ") && line.contains(" across ") && line.contains(" binar")
        });
    if !recognized {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || text.lines().count() > MAX_LINES {
        return Err(());
    }
    let visible = terminal::normalize(text)?;
    let mut selected = None;
    let mut run_id = false;
    let mut ended = None;
    let mut capture = false;
    let mut seen_capture = false;
    let mut separator = false;
    let mut nodes = BTreeMap::<(&str, &str), Status>::new();
    let mut final_nodes = BTreeSet::new();
    for line in visible.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line == SEPARATOR {
            separator = true;
            capture = false;
            continue;
        }
        if selected.is_none() {
            if is_run_id(line) && !run_id {
                run_id = true;
                continue;
            }
            if let Some(("Starting", body)) = producer_row(line) {
                selected = Some(selection(&format!("Starting {body}"))?);
                separator = false;
                continue;
            }
            // Only ordinary Cargo progress can precede the nextest producer.
            // Warning bodies, quoted examples and mixed command logs abstain.
            if !run_id
                && producer_row(line)
                    .is_some_and(|(label, _)| matches!(label, "Compiling" | "Finished"))
            {
                continue;
            }
            return Err(());
        }
        let selected = selected.ok_or(())?;
        if matches!(line.trim(), "stdout ---" | "stderr ---" | "stdout ───" | "stderr ───") {
            capture = true;
            seen_capture = true;
            continue;
        }
        if let Some((label, body)) = producer_row(line) {
            if label == "Summary" {
                if ended.is_some() || !separator {
                    return Err(());
                }
                ended = Some(summary(body)?);
                capture = false;
                continue;
            }
            if matches!(label, "PASS" | "FAIL" | "TIMEOUT" | "ABORT" | "LEAK-FAIL" | "SKIP" | "SLOW") {
                let (binary, name, status) = test_result(label, body, selected)?;
                if ended.is_some() {
                    // Final status listings repeat results, not executions.
                    if nodes.get(&(binary, name)) != Some(&status)
                        || !final_nodes.insert((binary, name))
                    {
                        return Err(());
                    }
                } else if status != Status::Slow
                    && (nodes.len() == MAX_TESTS || nodes.insert((binary, name), status).is_some())
                {
                    return Err(());
                }
                capture = false;
                separator = false;
                continue;
            }
        }
        if capture && line.starts_with("    ") {
            continue;
        }
        if ended.is_some_and(|summary| summary.failed > 0) && line == "error: test run failed" {
            continue;
        }
        return Err(());
    }
    let mut execution = Execution {
        selection: selected,
        ..Execution::default()
    };
    let mut diagnostics = Vec::new();
    let mut skipped = 0;
    for ((binary, name), status) in &nodes {
        let id = subject(binary, name);
        match status {
            Status::Skipped => skipped += 1,
            Status::Passed => {
                execution.inventory.insert(id);
                execution.passed.insert(id);
            }
            Status::Failed => {
                execution.inventory.insert(id);
                execution.failed.insert(id);
                if diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE {
                    diagnostics.push(diagnostic(binary, name));
                }
            }
            Status::Slow => return Err(()),
        }
    }
    if let Some(summary) = ended {
        let selected = selected.ok_or(())?;
        if summary.ran != selected.tests
            || summary.ran != execution.inventory.len()
            || summary.passed != execution.passed.len()
            || summary.failed != execution.failed.len()
            || summary.skipped != selected.skipped
            || skipped > summary.skipped
        {
            return Err(());
        }
        execution.complete = true;
    }
    let failed = !execution.failed.is_empty();
    let allows_completion = execution.complete
        && !failed
        && !execution.inventory.is_empty()
        && !seen_capture;
    Ok(Some(Decoded {
        diagnostics,
        failed,
        allows_completion,
        execution,
    }))
}

#[cfg(test)]
#[path = "cass_error_recall_nextest_tests.rs"]
mod tests;
