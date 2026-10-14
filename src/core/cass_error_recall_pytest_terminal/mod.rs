//! Native pytest terminal reports, without a pytest plugin or JSON reporter.
//!
//! Read test progress and the final short summary, never traceback frames,
//! captured stdout, locals or assertion expansions. Quiet reports can supply
//! fingerprints; only a complete named inventory can certify a repair. Exact
//! invocation/context and independent process completion remain caller-owned.

use std::collections::{BTreeMap, BTreeSet};

use super::super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};

mod terminal;

const MAX_TESTS: usize = 1024;
const MAX_LINES: usize = 4096;
type Subject = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Passed,
    Failed,
    Error,
    Skipped,
    Xfailed,
    Xpassed,
}

impl Status {
    fn failed(self) -> bool {
        matches!(self, Self::Failed | Self::Error)
    }

    fn counter(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Error => "error",
            Self::Skipped => "skipped",
            Self::Xfailed => "xfailed",
            Self::Xpassed => "xpassed",
        }
    }
}

#[derive(Default)]
pub(in crate::core::cass_error_recall) struct Execution {
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

pub(in crate::core::cass_error_recall) struct Decoded {
    pub diagnostics: Vec<CanonicalDiagnostic>,
    pub symptom: Option<String>,
    pub failed: bool,
    pub allows_completion: bool,
    pub execution: Execution,
}

fn subject(node: &str) -> Subject {
    let mut hash = blake3::Hasher::new();
    hash.update(b"ee.cass.pytest.proof_subject.v1\0");
    hash.update(&(node.len() as u64).to_le_bytes());
    hash.update(node.as_bytes());
    *hash.finalize().as_bytes()
}

fn banner(line: &str) -> Option<&str> {
    let line = line.trim();
    (line.starts_with("===") && line.ends_with("===")).then(|| line.trim_matches('=').trim())
}

fn progress_tail(tail: &str) -> bool {
    let tail = tail.trim();
    if tail.is_empty() {
        return true;
    }
    tail.strip_prefix('[')
        .and_then(|tail| tail.strip_suffix("%]"))
        .and_then(|tail| tail.trim().parse::<u8>().ok())
        .is_some_and(|percent| percent <= 100)
}

fn quiet_progress(line: &str) -> bool {
    let (marks, tail) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    !marks.is_empty()
        && marks.bytes().all(|byte| b".FEsxX".contains(&byte))
        && progress_tail(tail)
}

fn test_node(node: &str) -> bool {
    let Some((path, name)) = node.split_once("::") else {
        return false;
    };
    path.ends_with(".py")
        && !name.is_empty()
        && node == node.trim()
        && !node.chars().any(char::is_control)
}

fn progress(line: &str) -> Option<(&str, Status)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    for (label, status) in [
        ("PASSED", Status::Passed),
        ("FAILED", Status::Failed),
        ("ERROR", Status::Error),
        ("SKIPPED", Status::Skipped),
        ("XFAIL", Status::Xfailed),
        ("XPASS", Status::Xpassed),
    ] {
        let separator = format!(" {label}");
        let Some((node, tail)) = line.rsplit_once(separator.as_str()) else {
            continue;
        };
        // Skip/xfail reasons are display text, not test identities or proof.
        let tail = tail.trim();
        let tail = if !matches!(status, Status::Passed | Status::Failed | Status::Error)
            && tail.starts_with('(')
        {
            tail.rsplit_once(')')?.1.trim()
        } else {
            tail
        };
        if test_node(node) && progress_tail(tail) {
            return Some((node, status));
        }
    }
    None
}

fn selected_count(line: &str) -> Option<u64> {
    let text = line
        .strip_prefix("collecting ... collected ")
        .or_else(|| line.strip_prefix("collected "))?;
    let mut parts = text.split(" / ");
    let count = parts.next()?;
    let (number, units) = count.split_once(' ')?;
    if !matches!(units, "item" | "items") {
        return None;
    }
    let total = number.parse::<u64>().ok()?;
    let rest: Vec<_> = parts.collect();
    match rest.as_slice() {
        [] => Some(total),
        [deselected, selected] => {
            let excluded = deselected.strip_suffix(" deselected")?.parse::<u64>().ok()?;
            let selected = selected.strip_suffix(" selected")?.parse::<u64>().ok()?;
            (excluded.checked_add(selected) == Some(total)).then_some(selected)
        }
        _ => None,
    }
}

#[derive(Default)]
struct Totals(BTreeMap<&'static str, u64>);

impl Totals {
    fn get(&self, name: &str) -> u64 {
        self.0.get(name).copied().unwrap_or(0)
    }

    fn add(&mut self, status: Status) {
        *self.0.entry(status.counter()).or_default() += 1;
    }

    fn same_execution(&self, other: &Self) -> bool {
        ["passed", "failed", "error", "skipped", "xfailed", "xpassed"]
            .iter()
            .all(|name| self.get(name) == other.get(name))
    }

    fn failed(&self) -> bool {
        self.get("failed") > 0 || self.get("error") > 0
    }
}

fn totals(line: &str) -> Option<Totals> {
    let text = banner(line).unwrap_or(line.trim());
    let (counts, duration) = text.rsplit_once(" in ")?;
    let duration_parts: Vec<_> = duration.split_whitespace().collect();
    let seconds = match duration_parts.as_slice() {
        [seconds] => *seconds,
        [seconds, clock] if clock.starts_with('(') && clock.ends_with(')') => {
            let fields: Vec<_> = clock[1..clock.len() - 1].split(':').collect();
            if fields.len() != 3
                || fields.iter().any(|field| {
                    field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                return None;
            }
            *seconds
        }
        _ => None?,
    };
    let seconds = seconds.strip_suffix('s')?;
    let seconds = seconds.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    let mut totals = Totals::default();
    for counter in counts.split(", ") {
        let (count, name) = counter.split_once(' ')?;
        if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let name = match name {
            "passed" => "passed",
            "failed" => "failed",
            "error" | "errors" => "error",
            "skipped" => "skipped",
            "xfailed" => "xfailed",
            "xpassed" => "xpassed",
            "deselected" => "deselected",
            "warning" | "warnings" => "warning",
            _ => return None,
        };
        if totals.0.insert(name, count.parse().ok()?).is_some() {
            return None;
        }
    }
    (!totals.0.is_empty()).then_some(totals)
}

/// None is another producer. A recognized but incomplete report is not a
/// green run. Damaged reports refuse the whole interpretation, not a prefix.
pub(in crate::core::cass_error_recall) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    // Probe only the producer header before interpreting terminal controls.
    // Unrelated colored compiler output must retain its existing decoder.
    let visible_first = match terminal::normalize(first) {
        Ok(first) => first,
        Err(()) if first.contains("test session starts") => return Err(()),
        Err(()) => return Ok(None),
    };
    let session_header = banner(&visible_first) == Some("test session starts");
    if !session_header && !quiet_progress(&visible_first) {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || text.lines().count() > MAX_LINES {
        return Err(());
    }
    let visible = terminal::normalize(text)?;
    let text = visible.as_ref();
    // A lone dot, F or x is ordinary stdout, not a claimed test report. Quiet
    // mode needs either pytest's progress percentage or a report boundary.
    if !session_header
        && !visible_first.contains('[')
        && !text.lines().any(|line| {
            banner(line) == Some("short test summary info") || totals(line).is_some()
        })
    {
        return Ok(None);
    }
    let lines: Vec<_> = text.lines().collect();
    let first_index = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .ok_or(())?;
    let last = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .ok_or(())?;
    let footer = totals(lines[last]);
    let mut selected = None;
    let mut collection_seen = false;
    let mut progress_open = true;
    let mut in_summary = false;
    let mut summary_seen = false;
    let mut ambiguous = false;
    let mut interrupted = false;
    let mut observed = Totals::default();
    let mut nodes = BTreeMap::<String, Status>::new();
    let mut summaries = BTreeMap::<String, (Status, String)>::new();
    for (index, line) in lines.iter().enumerate() {
        let line = line.trim_end();
        if index == last && footer.is_some() {
            break;
        }
        if totals(line).is_some() {
            return Err(());
        }
        if let Some(label) = banner(line) {
            match label {
                "test session starts" if index == first_index => {}
                "test session starts" => return Err(()),
                "short test summary info" => {
                    if summary_seen {
                        return Err(());
                    }
                    summary_seen = true;
                    in_summary = true;
                    progress_open = false;
                }
                _ => {
                    progress_open = false;
                    in_summary = false;
                }
            }
            continue;
        }
        if line.starts_with("!!!") {
            interrupted = true;
            progress_open = false;
            in_summary = false;
            continue;
        }
        if progress_open {
            if line.starts_with("collected ") || line.starts_with("collecting ... collected ") {
                if collection_seen {
                    return Err(());
                }
                collection_seen = true;
                selected = selected_count(line);
                continue;
            }
            if let Some((node, status)) = progress(line) {
                if nodes.len() == MAX_TESTS && !nodes.contains_key(node) {
                    return Err(());
                }
                observed.add(status);
                if let Some(previous) = nodes.insert(node.to_owned(), status) {
                    // Teardown can report ERROR after PASSED for the same
                    // selected test. Other repeats/plugins lack phase identity.
                    if previous != Status::Passed || status != Status::Error {
                        ambiguous = true;
                    }
                    if previous.failed() {
                        nodes.insert(node.to_owned(), previous);
                    }
                }
            }
        }
        if in_summary {
            let (status, rest) = if let Some(rest) = line.strip_prefix("FAILED ") {
                (Status::Failed, rest)
            } else if let Some(rest) = line.strip_prefix("ERROR ") {
                (Status::Error, rest)
            } else {
                continue;
            };
            let (node, message) = rest.split_once(" - ").unwrap_or((rest, ""));
            if node.trim().is_empty() || node.chars().any(char::is_control) {
                return Err(());
            }
            if summaries.len() == MAX_TESTS {
                return Err(());
            }
            if summaries
                .insert(node.to_owned(), (status, message.to_owned()))
                .is_some()
            {
                ambiguous = true;
            }
        }
    }
    // Summaries may supply quiet-mode identities, but never invent named
    // passes or expand a verbose inventory. Every explicit contradiction vetoes.
    for (node, (status, _)) in &summaries {
        if let Some(progress_status) = nodes.get(node) {
            if progress_status != status {
                return Err(());
            }
        } else if !nodes.is_empty() {
            ambiguous = true;
        }
    }
    let mut failures: BTreeMap<_, _> = nodes
        .iter()
        .filter(|(_, status)| status.failed())
        .map(|(node, status)| (node.as_str(), (*status, "")))
        .collect();
    for (node, (status, message)) in &summaries {
        failures.insert(node, (*status, message.as_str()));
    }
    if let Some(footer) = &footer {
        let failed = failures
            .values()
            .filter(|(status, _)| *status == Status::Failed)
            .count() as u64;
        let errors = failures
            .values()
            .filter(|(status, _)| *status == Status::Error)
            .count() as u64;
        if failed > footer.get("failed") || errors > footer.get("error") {
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
    execution.complete = !ambiguous
        && !interrupted
        && selected == Some(nodes.len() as u64)
        && footer
            .as_ref()
            .is_some_and(|footer| observed.same_execution(footer));
    let failed = !failures.is_empty() || footer.as_ref().is_some_and(Totals::failed);
    let allows_completion = execution.complete
        && !failed
        && footer.as_ref().is_some_and(|footer| footer.get("passed") > 0);
    let symptom = failures.first_key_value().map(|(node, (_, message))| {
        crate::policy::redact_secret_like_content(&format!("pytest failure in {node}: {message}"))
            .content
    });
    let diagnostics = failures
        .into_iter()
        .take(MAX_DIAGNOSTICS_PER_FAILURE)
        .map(|(node, _)| diagnostic(node))
        .collect();
    Ok(Some(Decoded {
        diagnostics,
        symptom,
        failed,
        allows_completion,
        execution,
    }))
}

fn diagnostic(node: &str) -> CanonicalDiagnostic {
    // Redact BEFORE deriving a persistent signature. Exact raw subject hashes
    // above are transient coverage only. Paths/parameters never become codes.
    let node = crate::policy::redact_secret_like_content(node).content;
    let key = blake3::hash(node.as_bytes()).to_hex();
    let label = node.rsplit("::").next().unwrap_or("test");
    // Progress can arrive before the final exception summary. Keep the class
    // invariant across polling boundaries, verbosity and traceback rendering.
    let detail = format!("pytest test failure {key} {label}");
    CanonicalDiagnostic {
        tool: crate::core::error_recall::DiagnosticTool::Shell,
        canonical_code: None,
        message_template: crate::core::error_recall::canonical_message_template(&detail),
    }
}

#[cfg(test)]
#[path = "cass_error_recall_pytest_tests.rs"]
mod tests;

#[cfg(test)]
mod store_tests;
