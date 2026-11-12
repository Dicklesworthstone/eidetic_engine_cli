//! Native Vitest RUN reports. Only verbose, fully named inventories prove a
//! repair; default file aggregates can contribute failure diagnostics only.
//!
//! Grammar follows Vitest's verbose/base reporters and reporter snapshots at
//! vitest-dev/vitest@47c87ebb5025010141cf8aa5dc69c5b57c10d2fa:
//! packages/vitest/src/node/reporters/{verbose,base}.ts and
//! test/e2e/test/reporters/{verbose.test.ts,__snapshots__/default.test.ts.snap}.
//! The supported form is one single-project, non-watch run. Console payloads,
//! retries, snapshots, annotations and additional reporter sections are not
//! completion evidence. Error bodies never supply test names or process exits.

use std::collections::{BTreeMap, BTreeSet};

use super::super::python::terminal;
use super::{CompilerOutput, Decoded, Execution, MAX_TEST_RESULTS, Subject, retain, subject};

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

#[derive(Default)]
struct Counts {
    values: [usize; 4],
    total: usize,
}

#[derive(Default)]
struct File {
    aggregate_status: Option<Status>,
    counts: [usize; 4],
}

fn number(text: &str) -> Result<usize, ()> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    text.parse()
        .ok()
        .filter(|value| *value <= MAX_TEST_RESULTS)
        .ok_or(())
}

fn status(text: &str) -> Result<Status, ()> {
    match text {
        "passed" => Ok(Status::Passed),
        "failed" => Ok(Status::Failed),
        "skipped" => Ok(Status::Skipped),
        "todo" => Ok(Status::Todo),
        _ => Err(()),
    }
}

fn count_part(text: &str) -> Result<(usize, Status), ()> {
    let (value, label) = text.split_once(' ').ok_or(())?;
    let value = number(value)?;
    if value == 0 {
        return Err(());
    }
    Ok((value, status(label)?))
}

fn counts(text: &str) -> Result<Counts, ()> {
    if text == "no tests" {
        return Ok(Counts::default());
    }
    let (states, total) = text.rsplit_once(" (").ok_or(())?;
    let mut result = Counts {
        total: number(total.strip_suffix(')').ok_or(())?)?,
        ..Counts::default()
    };
    for part in states.split(" | ") {
        let (value, status) = count_part(part)?;
        let slot = &mut result.values[status.index()];
        if *slot != 0 {
            return Err(());
        }
        *slot = value;
    }
    if result.values.iter().sum::<usize>() != result.total {
        return Err(());
    }
    Ok(result)
}

fn milliseconds(text: &str) -> bool {
    text.strip_suffix("ms")
        .is_some_and(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn duration(text: &str) -> bool {
    milliseconds(text)
        || text.strip_suffix('s').is_some_and(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
                && value
                    .parse::<f64>()
                    .is_ok_and(|value| value.is_finite() && value >= 0.0)
        })
}

fn elapsed(text: &str) -> bool {
    let (time, breakdown) = text
        .split_once(' ')
        .map_or((text, None), |(time, rest)| (time, Some(rest)));
    duration(time)
        && breakdown.is_none_or(|value| {
            value
                .strip_prefix('(')
                .and_then(|value| value.strip_suffix(')'))
                .is_some_and(|value| {
                    !value.is_empty()
                        && value.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || b" ,.:_%()-".contains(&byte)
                        })
                })
        })
}

fn clock(text: &str) -> bool {
    let parts: Vec<_> = text.split(':').collect();
    parts.len() == 3
        && parts.iter().enumerate().all(|(index, part)| {
            part.len() == 2
                && number(part).is_ok_and(|value| value < if index == 0 { 24 } else { 60 })
        })
}

fn file_name(text: &str) -> Result<&str, ()> {
    // includeTaskLocation adds the test's source line to verbose file names.
    // Locations move during repairs and are not part of the test identity.
    let name = text
        .rsplit_once(':')
        .filter(|(_, line)| !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(text, |(name, _)| name);
    if name.trim() != name
        || name.is_empty()
        || name.starts_with('|')
        || ![".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"]
            .iter()
            .any(|extension| name.ends_with(extension))
    {
        return Err(());
    }
    Ok(name)
}

fn identity<'a>(text: &'a str, root: &str) -> Result<(&'a str, Subject), ()> {
    let (file, title) = text.split_once(" > ").ok_or(())?;
    let file = file_name(file)?;
    let names: Vec<_> = title.split(" > ").collect();
    if names.len() > MAX_DEPTH
        || names
            .iter()
            .any(|name| name.is_empty() || name.trim() != *name)
    {
        return Err(());
    }
    let mut parts = vec!["vitest-terminal-test", root, file];
    parts.extend(names);
    Ok((file, subject(&parts)))
}

fn record(line: &str) -> Option<(Status, &str)> {
    // Exactly the native top-level prefix. Default reporter children and
    // source frames are not a verbose inventory.
    let (symbol, text) = line.strip_prefix(' ')?.split_once(' ')?;
    let status = match symbol {
        "✓" => Status::Passed,
        "×" | "❯" => Status::Failed,
        "↓" => Status::Skipped,
        "□" => Status::Todo,
        _ => return None,
    };
    Some((status, text))
}

fn without_time(text: &str, status: Status) -> Result<&str, ()> {
    if let Some((name, time)) = text.rsplit_once(' ')
        && milliseconds(time)
    {
        return Ok(name);
    }
    if matches!(status, Status::Skipped | Status::Todo) {
        Ok(text)
    } else {
        Err(())
    }
}

fn aggregate(text: &str) -> Result<(&str, [usize; 4]), ()> {
    let (file, states) = text.rsplit_once(" (").ok_or(())?;
    let mut parts = states.strip_suffix(')').ok_or(())?.split(" | ");
    let (total, label) = parts.next().ok_or(())?.split_once(' ').ok_or(())?;
    let total = number(total)?;
    if !matches!(label, "test" | "tests") {
        return Err(());
    }
    let mut counts = [0_usize; 4];
    for part in parts {
        let (value, status) = count_part(part)?;
        if status == Status::Passed || counts[status.index()] != 0 {
            return Err(());
        }
        counts[status.index()] = value;
    }
    counts[0] = total.checked_sub(counts.iter().sum()).ok_or(())?;
    Ok((file_name(file)?, counts))
}

fn error_banner(line: &str) -> Option<&str> {
    let line = line.trim();
    let body = line.strip_prefix('⎯')?.trim_start_matches('⎯').trim();
    let body = body.strip_suffix('⎯')?.trim_end_matches('⎯').trim();
    Some(body)
}

fn recognizable(text: &str) -> bool {
    text.lines().any(|line| {
        let native = line.trim_start_matches(' ');
        // Jest indents diagnostic and console bodies by at least four spaces.
        // Printed Vitest examples there must remain payloads of that family.
        line.len() - native.len() <= 1
            && (native.starts_with("RUN  v")
                || native.starts_with("DEV  v")
                || native.starts_with("Test Files "))
    })
}

fn error_details<'a>(
    lines: &[&'a str],
    root: &str,
    failed: usize,
    files: &BTreeMap<&str, File>,
    assertions: &BTreeMap<Subject, Status>,
    aggregates: bool,
) -> Result<Vec<&'a str>, ()> {
    let header = lines
        .first()
        .and_then(|line| error_banner(line))
        .ok_or(())?;
    if number(header.strip_prefix("Failed Tests ").ok_or(())?)? != failed || failed == 0 {
        return Err(());
    }
    let mut pending = Vec::new();
    let mut seen = BTreeSet::new();
    let mut message = None;
    let mut messages = Vec::new();
    let mut divider_total = None;
    let mut groups = 0;
    let mut occurrences = 0;
    for line in &lines[1..] {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(divider) = error_banner(line) {
            let (index, total) = divider
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .and_then(|value| value.split_once('/'))
                .ok_or(())?;
            let total = number(total)?;
            groups += 1;
            occurrences += pending.len();
            if number(index)? != groups
                || total < occurrences
                || divider_total.is_some_and(|expected| expected != total)
                || pending.is_empty()
            {
                return Err(());
            }
            divider_total = Some(total);
            messages.push(message.take().ok_or(())?);
            seen.extend(pending.drain(..));
            continue;
        }
        // After the first diagnostic line, the entire diff/code-frame/stack
        // payload is opaque. A printed FAIL header cannot add a test subject.
        if message.is_some() {
            continue;
        }
        if let Some(name) = line.strip_prefix(" FAIL  ") {
            let (file, key) = identity(name, root)?;
            if !files.contains_key(file)
                || (!aggregates && assertions.get(&key) != Some(&Status::Failed))
                || (aggregates && files[file].aggregate_status != Some(Status::Failed))
            {
                return Err(());
            }
            pending.push(key);
        } else if pending.is_empty() {
            return Err(());
        } else {
            message = Some(line.trim());
        }
    }
    // The upstream divider counts errors before merging identical stacks:
    // two adjacent FAIL headers can legitimately share a single [1/2]. Count
    // their occurrences, not just groups, and reject a missing final error.
    if !pending.is_empty()
        || message.is_some()
        || seen.len() != failed
        || messages.is_empty()
        || divider_total != Some(occurrences)
    {
        return Err(());
    }
    if !aggregates
        && assertions
            .iter()
            .any(|(key, status)| *status == Status::Failed && !seen.contains(key))
    {
        return Err(());
    }
    Ok(messages)
}

pub(super) fn decode(text: &str) -> Result<Option<Decoded>, ()> {
    if text.trim_start().starts_with(['{', '[']) {
        return Ok(None);
    }
    let visible = match terminal::normalize(text) {
        Ok(visible) => visible,
        Err(()) => {
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
    if lines.len() > MAX_LINES
        || lines.iter().any(|line| {
            let line = line.trim_start();
            line.starts_with("stdout | ") || line.starts_with("stderr | ")
        })
    {
        // Vitest writes user logs without indentation or escaping after these
        // headers. Their contents cannot be distinguished from real records.
        return Err(());
    }
    let summaries: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("Test Files "))
        .map(|(index, _)| index)
        .collect();
    let [summary] = summaries.as_slice() else {
        return Err(());
    };
    let footer: Vec<_> = lines[*summary..]
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    let [file_summary, test_summary, started, time] = footer.as_slice() else {
        return Err(());
    };
    let file_counts = counts(file_summary.strip_prefix("Test Files ").ok_or(())?.trim())?;
    let test_counts = counts(test_summary.strip_prefix("Tests ").ok_or(())?.trim())?;
    if file_counts.total == 0
        || !clock(started.strip_prefix("Start at ").ok_or(())?.trim())
        || !elapsed(time.strip_prefix("Duration ").ok_or(())?.trim())
    {
        return Err(());
    }
    let mut run_root = None;
    let mut files: BTreeMap<&str, File> = BTreeMap::new();
    let mut assertions = BTreeMap::new();
    let mut aggregates = false;
    let mut last_status = None;
    let mut details = None;
    for (index, line) in lines[..*summary].iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if run_root.is_none() {
            if line.starts_with('>') {
                continue;
            }
            let header = line.trim().strip_prefix("RUN  v").ok_or(())?;
            let (version, root) = header.split_once(' ').ok_or(())?;
            if version.is_empty()
                || !version.starts_with(|ch: char| ch.is_ascii_digit())
                || !version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
                || root.trim().is_empty()
            {
                return Err(());
            }
            run_root = Some(root.trim());
            continue;
        }
        if error_banner(line).is_some() {
            details = Some(index);
            break;
        }
        if let Some((state, text)) = record(line) {
            let text = without_time(text, state)?;
            if text.contains(" > ") {
                if aggregates {
                    return Err(());
                }
                let (file, key) = identity(text, run_root.ok_or(())?)?;
                if assertions.insert(key, state).is_some() || assertions.len() > MAX_TEST_RESULTS {
                    return Err(());
                }
                files.entry(file).or_default().counts[state.index()] += 1;
            } else {
                if !assertions.is_empty() {
                    return Err(());
                }
                let (file, counts) = aggregate(text)?;
                if (state == Status::Passed && counts[1] != 0)
                    || files
                        .insert(
                            file,
                            File {
                                aggregate_status: Some(state),
                                counts,
                            },
                        )
                        .is_some()
                {
                    return Err(());
                }
                aggregates = true;
            }
            last_status = Some(state);
        } else if line.starts_with("   → ") && last_status == Some(Status::Failed) {
            // The detailed error block provides the diagnostic, not an inline
            // assertion message whose continuation could resemble a record.
        } else if aggregates && line.starts_with("   ") {
            // Default reporter children are partial and can never prove repair.
        } else {
            return Err(());
        }
    }
    let root = run_root.ok_or(())?;
    if files.len() != file_counts.total {
        return Err(());
    }
    let mut observed_files = [0_usize; 4];
    let mut observed_tests = [0_usize; 4];
    let mut execution = Execution::default();
    for (name, file) in &files {
        for (observed, count) in observed_tests.iter_mut().zip(file.counts) {
            *observed += count;
        }
        let state = file.aggregate_status.unwrap_or_else(|| {
            if file.counts[1] > 0 {
                Status::Failed
            } else if file.counts[0] > 0 {
                Status::Passed
            } else if file.counts[2] > 0 {
                Status::Skipped
            } else {
                Status::Todo
            }
        });
        observed_files[state.index()] += 1;
        let key = subject(&["vitest-terminal-file", root, name]);
        execution.inventory.insert(key);
        match state {
            Status::Passed => {
                execution.passed.insert(key);
            }
            Status::Failed => {
                execution.failed.insert(key);
            }
            _ => {}
        }
    }
    if observed_files != file_counts.values || observed_tests != test_counts.values {
        return Err(());
    }
    let mut output = CompilerOutput::default();
    let mut diagnostic_keys = BTreeSet::new();
    if let Some(start) = details {
        for message in error_details(
            &lines[start..*summary],
            root,
            test_counts.values[1],
            &files,
            &assertions,
            aggregates,
        )? {
            retain(&mut output, &mut diagnostic_keys, message);
        }
    } else if test_counts.values[1] != 0 || file_counts.values[1] != 0 {
        return Err(());
    }
    for (key, state) in assertions {
        execution.inventory.insert(key);
        match state {
            Status::Passed => {
                execution.passed.insert(key);
            }
            Status::Failed => {
                execution.failed.insert(key);
            }
            _ => {}
        }
    }
    output.failed |= file_counts.values[1] > 0 || test_counts.values[1] > 0;
    execution.complete = !aggregates && test_counts.total > 0;
    output.allows_completion = execution.complete
        && !output.failed
        && test_counts.values[2] == 0
        && test_counts.values[3] == 0;
    execution.allows_completion = output.allows_completion;
    Ok(Some(Decoded { output, execution }))
}
