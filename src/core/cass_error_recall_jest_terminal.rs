//! Native Jest reports, including the individual inventory from `--verbose`.
//!
//! Aggregate-only reports can contribute failure fingerprints, but cannot prove
//! a repair. Verbose reports bind file, describe hierarchy and assertion name;
//! both previously failing and passing tests must actually pass on the retry.
//! Diagnostic bodies and captured console output never contribute subjects or
//! process exits. The caller separately requires a completed, identical command.

use std::collections::{BTreeMap, BTreeSet};

use super::super::python::terminal;
use super::{CompilerOutput, Decoded, Execution, MAX_TEST_RESULTS, retain, subject};

const MAX_LINES: usize = 4096;
const MAX_DEPTH: usize = 32;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Status {
    Passed,
    Failed,
    Skipped,
    Todo,
}

impl Status {
    fn index(self) -> usize {
        match self {
            Self::Passed => 0,
            Self::Failed => 1,
            Self::Skipped => 2,
            Self::Todo => 3,
        }
    }
}

struct File<'a> {
    name: &'a str,
    failed: bool,
    counts: [usize; 4],
    assertions: BTreeMap<String, Status>,
    diagnostics: BTreeMap<&'a str, &'a str>,
}

#[derive(Default)]
struct Counts {
    values: [usize; 4],
    total: usize,
}

fn number(value: &str) -> Result<usize, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value
        .parse()
        .ok()
        .filter(|value| *value <= MAX_TEST_RESULTS)
        .ok_or(())
}

fn counts(text: &str, suites: bool) -> Result<Counts, ()> {
    let mut result = Counts::default();
    let mut seen = BTreeSet::new();
    let mut parts = text.trim().split(',').peekable();
    while let Some(part) = parts.next() {
        let fields: Vec<_> = part.split_whitespace().collect();
        if parts.peek().is_none() {
            let (run, total) = match fields.as_slice() {
                [total, "total"] => (None, number(total)?),
                [run, "of", total, "total"] if suites => (Some(number(run)?), number(total)?),
                _ => return Err(()),
            };
            result.total = total;
            if result.values.iter().sum::<usize>() != total
                || run.is_some_and(|run| run != result.values[0] + result.values[1])
            {
                return Err(());
            }
        } else {
            let [value, label] = fields.as_slice() else {
                return Err(());
            };
            let slot = match *label {
                "passed" => 0,
                "failed" => 1,
                "skipped" => 2,
                "todo" if !suites => 3,
                _ => return Err(()),
            };
            if !seen.insert(slot) {
                return Err(());
            }
            result.values[slot] = number(value)?;
        }
    }
    Ok(result)
}

/// A changed snapshot is not evidence that an earlier assertion still passes.
fn unchanged_snapshots(text: &str) -> Result<bool, ()> {
    let mut seen = BTreeSet::new();
    let mut unchanged = true;
    let mut total = 0_usize;
    let mut parts = text.trim().split(',').peekable();
    while let Some(part) = parts.next() {
        let fields: Vec<_> = part.split_whitespace().collect();
        let [value, label] = fields.as_slice() else {
            return Err(());
        };
        let value = number(value)?;
        if parts.peek().is_none() {
            if *label != "total" || total != value {
                return Err(());
            }
        } else {
            if !matches!(
                *label,
                "passed" | "failed" | "written" | "updated" | "obsolete" | "removed"
            ) || !seen.insert(*label)
            {
                return Err(());
            }
            unchanged &= *label == "passed" || value == 0;
            total = total.checked_add(value).ok_or(())?;
        }
    }
    Ok(unchanged)
}

fn duration(text: &str) -> bool {
    let Some((value, unit)) = text.split_once(' ') else {
        return false;
    };
    matches!(unit, "ms" | "s")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && value
            .parse::<f64>()
            .is_ok_and(|value| value.is_finite() && value >= 0.0)
}

fn without_duration(text: &str) -> &str {
    text.rsplit_once(" (")
        .filter(|(_, suffix)| suffix.strip_suffix(')').is_some_and(duration))
        .map_or(text, |(name, _)| name)
}

fn file_header(line: &str) -> Option<(bool, &str)> {
    // Colored Jest headers have one leading space; diagnostic and console
    // payloads have at least four. Never promote a payload's fake PASS line.
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 1 {
        return None;
    }
    let (failed, name) = if let Some(name) = trimmed.strip_prefix("FAIL ") {
        (true, name)
    } else {
        (false, trimmed.strip_prefix("PASS ")?)
    };
    Some((failed, without_duration(name.trim())))
}

fn assertion(text: &str) -> Option<(Status, &str)> {
    let (icon, title) = text.split_once(' ')?;
    let (status, title) = match icon {
        "✓" | "√" => (Status::Passed, without_duration(title)),
        "✕" | "×" => (Status::Failed, without_duration(title)),
        "○" => (
            Status::Skipped,
            title.strip_prefix("skipped ").unwrap_or(title),
        ),
        "✎" => (Status::Todo, title.strip_prefix("todo ").unwrap_or(title)),
        _ => return None,
    };
    Some((status, title))
}

fn retain_detail<'a>(
    file: &mut File<'a>,
    detail: &mut Option<(&'a str, Option<&'a str>)>,
) -> Result<(), ()> {
    let Some((title, message)) = detail.take() else {
        return Ok(());
    };
    if !file.failed || file.diagnostics.insert(title, message.ok_or(())?).is_some() {
        return Err(());
    }
    Ok(())
}

fn recognizable(text: &str) -> bool {
    text.lines().any(|line| {
        line.starts_with("Test Suites:")
            || file_header(line).is_some_and(|(_, name)| {
                // A bare PASS/FAIL word also belongs to other tools. Before
                // the Jest summary arrives, require a plausible JS/TS file
                // header; completed custom-extension reports still use their
                // full native summary as the family discriminator.
                name.split_whitespace().any(|part| {
                    [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"]
                        .iter()
                        .any(|extension| part.ends_with(extension))
                })
            })
    })
}

pub(super) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    // Other terminal families must retain their own validators. A recognized
    // but unfinished Jest report remains guarded instead of becoming generic
    // shell output when a streamed prefix or trailing summary is incomplete.
    if text.trim_start().starts_with(['{', '[']) {
        return Ok(None);
    }
    let visible = match terminal::normalize(text) {
        Ok(visible) => visible,
        Err(()) => {
            // A valid styled header can precede an invalid/truncated trailer.
            // Keep that native stream guarded, while leaving unrelated output
            // with its own terminal controls to the appropriate decoder.
            let native = recognizable(text)
                || text
                    .lines()
                    .any(|line| terminal::normalize(line).is_ok_and(|line| recognizable(&line)));
            return if native { Err(()) } else { Ok(None) };
        }
    };
    if !recognizable(&visible) {
        return Ok(None);
    }
    let lines: Vec<_> = visible.lines().collect();
    if lines.len() > MAX_LINES {
        return Err(());
    }
    let summaries: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("Test Suites:"))
        .map(|(index, _)| index)
        .collect();
    let [summary] = summaries.as_slice() else {
        return Err(());
    };
    let suites = counts(
        lines[*summary].strip_prefix("Test Suites:").ok_or(())?,
        true,
    )?;
    let tests = counts(
        lines
            .get(summary + 1)
            .and_then(|line| line.strip_prefix("Tests:"))
            .ok_or(())?,
        false,
    )?;
    let snapshots_unchanged = unchanged_snapshots(
        lines
            .get(summary + 2)
            .and_then(|line| line.strip_prefix("Snapshots:"))
            .ok_or(())?,
    )?;
    let time = lines
        .get(summary + 3)
        .and_then(|line| line.strip_prefix("Time:"))
        .ok_or(())?
        .trim();
    let time_valid = time.split_once(", estimated ").map_or_else(
        || duration(time),
        |(elapsed, expected)| duration(elapsed) && duration(expected),
    );
    if !time_valid || suites.total == 0 {
        return Err(());
    }
    let mut footer = false;
    for line in lines
        .iter()
        .skip(summary + 4)
        .filter(|line| !line.trim().is_empty())
    {
        if footer || !line.starts_with("Ran all test suites") || !line.ends_with('.') {
            return Err(());
        }
        footer = true;
    }

    let mut files: Vec<File<'_>> = Vec::new();
    let mut names = BTreeSet::new();
    let mut ancestors = Vec::new();
    let mut detail = None;
    let mut payload = false;
    let mut unfinished_group = false;
    let mut execution = Execution::default();
    let mut observed = [0_usize; 4];
    for line in &lines[..*summary] {
        if let Some((failed, name)) = file_header(line) {
            if unfinished_group {
                return Err(());
            }
            if let Some(file) = files.last_mut() {
                retain_detail(file, &mut detail)?;
            }
            if name.is_empty() || !names.insert(name) || files.len() == MAX_TEST_RESULTS {
                return Err(());
            }
            execution
                .inventory
                .insert(subject(&["jest-terminal-file", name]));
            files.push(File {
                name,
                failed,
                counts: [0; 4],
                assertions: BTreeMap::new(),
                diagnostics: BTreeMap::new(),
            });
            ancestors.clear();
            payload = false;
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some(file) = files.last_mut() else {
            // npm/pnpm script banners carry no test or completion authority.
            if line.starts_with('>') {
                continue;
            }
            return Err(());
        };
        if let Some(title) = line.strip_prefix("  ● ") {
            if unfinished_group {
                return Err(());
            }
            retain_detail(file, &mut detail)?;
            if title.is_empty() {
                return Err(());
            }
            if title != "Console" {
                detail = Some((title, None));
            }
            payload = true;
            continue;
        }
        if payload {
            // Only the first native diagnostic line contributes text. Never
            // mine its frames, expected values, test names or captured output.
            if !line.starts_with("    ") {
                return Err(());
            }
            if let Some((_, message)) = detail.as_mut()
                && message.is_none()
            {
                *message = Some(line.trim());
            }
            continue;
        }
        let body = line.trim_start_matches(' ');
        let indent = line.len() - body.len();
        let level = indent / 2;
        if indent == 0
            || indent % 2 != 0
            || level > MAX_DEPTH
            || ancestors.len() + 1 < level
            || (unfinished_group && level <= ancestors.len())
        {
            return Err(());
        }
        ancestors.truncate(level - 1);
        if let Some((status, title)) = assertion(body) {
            unfinished_group = false;
            if title.trim() != title || title.is_empty() || title.contains('›') {
                return Err(());
            }
            let display = ancestors
                .iter()
                .copied()
                .chain(std::iter::once(title))
                .collect::<Vec<_>>()
                .join(" › ");
            if file.assertions.insert(display, status).is_some() {
                return Err(());
            }
            let mut parts = vec!["jest-terminal-test", file.name];
            parts.extend(ancestors.iter().copied());
            parts.push(title);
            let key = subject(&parts);
            execution.inventory.insert(key);
            match status {
                Status::Passed => {
                    execution.passed.insert(key);
                }
                Status::Failed => {
                    execution.failed.insert(key);
                }
                _ => {}
            }
            file.counts[status.index()] += 1;
            observed[status.index()] += 1;
            if observed.iter().sum::<usize>() > MAX_TEST_RESULTS {
                return Err(());
            }
        } else {
            if body.trim() != body || body.contains('›') {
                return Err(());
            }
            ancestors.push(body);
            unfinished_group = true;
        }
    }
    if unfinished_group {
        return Err(());
    }
    if let Some(file) = files.last_mut() {
        retain_detail(file, &mut detail)?;
    }
    if files.len() != suites.values[0] + suites.values[1]
        || files.iter().filter(|file| file.failed).count() != suites.values[1]
        || observed
            .iter()
            .zip(tests.values)
            .any(|(seen, expected)| *seen > expected)
    {
        return Err(());
    }
    let all_verbose = files.iter().all(|file| !file.assertions.is_empty());
    if all_verbose && observed != tests.values {
        return Err(());
    }
    let mut output = CompilerOutput::default();
    let mut keys = BTreeSet::new();
    let mut runtime_failure = false;
    let mut failure_details = 0_usize;
    for file in files {
        if file.failed != (!file.diagnostics.is_empty() || file.counts[1] > 0) {
            return Err(());
        }
        for (title, message) in &file.diagnostics {
            if *title == "Test suite failed to run" {
                runtime_failure = true;
            } else {
                failure_details += 1;
                if !file.assertions.is_empty()
                    && file.assertions.get(*title) != Some(&Status::Failed)
                {
                    return Err(());
                }
            }
            retain(&mut output, &mut keys, message);
        }
        if file.assertions.iter().any(|(title, status)| {
            *status == Status::Failed && !file.diagnostics.contains_key(title.as_str())
        }) {
            return Err(());
        }
    }
    if failure_details != tests.values[1] {
        return Err(());
    }
    output.failed |= suites.values[1] > 0 || tests.values[1] > 0;
    execution.complete =
        all_verbose && suites.values[2] == 0 && !runtime_failure && snapshots_unchanged;
    output.allows_completion = !output.failed && execution.complete && tests.values[0] > 0;
    execution.allows_completion = output.allows_completion;
    Ok(Some(Decoded { output, execution }))
}
