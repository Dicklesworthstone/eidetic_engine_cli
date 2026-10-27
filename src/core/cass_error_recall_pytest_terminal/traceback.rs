//! Ordinary Python command failures in the shared diagnostic/repair lane.
//!
//! Only exception summaries become signatures. Stack paths, source lines,
//! locals, carets and notes are never diagnostic content. A traceback cannot
//! prove process success, even when truncated or wrapped in exit-code zero.
//! Native test reports keep their earlier, stricter test-inventory dispatch.

#[path = "exception_group.rs"]
mod exception_group;

#[path = "syntax.rs"]
mod syntax;

use std::collections::BTreeSet;

use super::{
    CanonicalDiagnostic, Decoded, Execution, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES,
    terminal,
};

const HEADER: &str = "Traceback (most recent call last):";
const CAUSE: &str = "The above exception was the direct cause of the following exception:";
const CONTEXT: &str = "During handling of the above exception, another exception occurred:";
const MAX_LINES: usize = 4096;
const MAX_CHAIN: usize = 32;
const MAX_FRAMES: usize = 512;

fn exception_line(line: &str) -> bool {
    let (name, message) = line
        .split_once(':')
        .map_or((line, None), |(name, message)| (name, Some(message)));
    if name.is_empty() || name.len() > 256 {
        return false;
    }
    let identifier = |part: &str| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_alphanumeric() || ch == '_')
    };
    if !name.split('.').all(identifier) {
        return false;
    }
    // A message-bearing custom class is allowed; an arbitrary bare word such
    // as OK is not an exception summary. Empty standard exceptions print only
    // their type (AssertionError, KeyboardInterrupt, StopIteration, ...).
    message.is_some()
        || name.ends_with("Error")
        || name.ends_with("Exception")
        || matches!(
            name,
            "KeyboardInterrupt"
                | "SystemExit"
                | "GeneratorExit"
                | "StopIteration"
                | "StopAsyncIteration"
        )
}

fn frame(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("  File \"") else {
        return false;
    };
    let Some((path, position)) = rest.rsplit_once("\", line ") else {
        return false;
    };
    let (number, function) = position
        .split_once(", in ")
        .map_or((position, None), |(number, function)| {
            (number, Some(function))
        });
    !path.is_empty()
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && number.parse::<u64>().is_ok_and(|line| line > 0)
        && function.is_none_or(|function| !function.trim().is_empty())
}

fn repeated_frames(line: &str) -> bool {
    line.strip_prefix("  [Previous line repeated ")
        .and_then(|rest| rest.strip_suffix(" more times]"))
        .is_some_and(|count| {
            !count.is_empty()
                && count.bytes().all(|byte| byte.is_ascii_digit())
                && count.parse::<u64>().is_ok_and(|count| count > 0)
        })
}

fn diagnostic(summary: &str) -> CanonicalDiagnostic {
    let safe = crate::policy::redact_secret_like_content(summary).content;
    CanonicalDiagnostic {
        tool: crate::core::error_recall::DiagnosticTool::Shell,
        canonical_code: None,
        message_template: crate::core::error_recall::canonical_message_template(&format!(
            "python traceback: {safe}"
        )),
    }
}

/// Validate the whole selected tail before returning any exception. A chain
/// can yield several classes, but malformed later sections cannot leave a
/// plausible prefix in the persistent store. The display cap is applied last.
fn summaries(text: &str) -> Option<Vec<&str>> {
    let lines: Vec<_> = text.lines().collect();
    let mut index = lines.iter().position(|line| *line == HEADER)?;
    let mut summaries = Vec::new();
    let mut frames = 0_usize;
    loop {
        if summaries.len() == MAX_CHAIN || lines.get(index).copied() != Some(HEADER) {
            return None;
        }
        index += 1;
        let mut section_frames = 0_usize;
        loop {
            let line = *lines.get(index)?;
            if frame(line) {
                frames += 1;
                section_frames += 1;
                if frames > MAX_FRAMES {
                    return None;
                }
            } else if section_frames > 0 && (line.starts_with("    ") || repeated_frames(line)) {
                // The interpreter indents rendered source and locals. Never
                // scan their text for exception types or instruction snippets.
            } else {
                break;
            }
            index += 1;
        }
        let summary = *lines.get(index)?;
        if section_frames == 0 || !exception_line(summary) {
            return None;
        }
        summaries.push(summary);
        index += 1;
        while lines.get(index).is_some_and(|line| line.trim().is_empty()) {
            index += 1;
        }
        if index == lines.len() {
            return Some(summaries);
        }
        if !matches!(lines[index], CAUSE | CONTEXT) {
            // Notes/multiline custom messages are deliberately not guessed.
            // They retain a failure veto, but no partially parsed signature.
            return None;
        }
        index += 1;
        while lines.get(index).is_some_and(|line| line.trim().is_empty()) {
            index += 1;
        }
    }
}

/// Compose complete top-level ordinary/group traceback sections. Only exact,
/// unindented CPython connectors are boundaries: source lines and group-local
/// margins stay opaque to this layer. A damaged section invalidates the whole
/// chain, including any otherwise valid prefix or later ordinary traceback.
fn chained_summaries(text: &str) -> Option<Vec<&str>> {
    let mut result = Vec::new();
    let mut frames = 0_usize;
    let mut sections = 0_usize;
    let mut start = 0;
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if matches!(line, CAUSE | CONTEXT) {
            if sections == MAX_CHAIN {
                return None;
            }
            append_chain_section(
                &text[start..offset],
                sections == 0,
                &mut frames,
                &mut result,
            )?;
            sections += 1;
            start = offset + raw.len();
        }
        offset += raw.len();
    }
    // The terminal summary of a mixed chain may otherwise be only a streamed
    // prefix. Standalone group handling retains its existing border contract.
    if sections == MAX_CHAIN || (sections > 0 && !text.ends_with('\n')) {
        return None;
    }
    append_chain_section(&text[start..], sections == 0, &mut frames, &mut result)?;
    Some(result)
}

fn append_chain_section<'a>(
    text: &'a str,
    first: bool,
    frames: &mut usize,
    result: &mut Vec<&'a str>,
) -> Option<()> {
    let header = text.lines().find(|line| {
        *line == HEADER || line.trim_start() == exception_group::ROOT_HEADER.trim_start()
    })?;
    // Ordinary stdout can precede the first traceback. After a connector, only
    // blank separators may precede the next traceback, never arbitrary prose.
    if !first && text.lines().find(|line| !line.trim().is_empty()) != Some(header) {
        return None;
    }
    if header != HEADER {
        return exception_group::append_summaries(text, frames, result);
    }
    let section = summaries(text)?;
    if section.len() != 1 {
        return None;
    }
    // The ordinary decoder already validated frame/source placement. Count
    // only that validated suffix; displayed source has four-space indentation.
    let count = text
        .lines()
        .skip_while(|line| *line != HEADER)
        .filter(|line| frame(line))
        .count();
    *frames = frames.checked_add(count)?;
    if *frames > MAX_FRAMES {
        return None;
    }
    result.extend(section);
    Some(())
}

pub(super) fn inspect(text: &str) -> Option<Decoded> {
    if let Some(output) = syntax::inspect(text) {
        return Some(output);
    }
    // Cheap family probe before allocation. Standard CPython styling wraps
    // this header; it does not put terminal commands inside the header words.
    let mut end = text.len().min(MAX_SCANNED_OUTPUT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let probe = &text[..end];
    if probe.trim_start().starts_with(['{', '[']) || !probe.contains(HEADER) {
        return None;
    }
    let mut output = Decoded {
        diagnostics: Vec::new(),
        symptom: None,
        failed: true,
        allows_completion: false,
        execution: Execution {
            command_failure: true,
            ..Execution::default()
        },
    };
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || text.lines().count() > MAX_LINES {
        return Some(output);
    }
    let visible = match terminal::normalize(text) {
        Ok(visible) => visible,
        Err(()) => return Some(output),
    };
    // A quoted or indented example is not a native traceback. An unindented
    // header may follow ordinary stdout from the same invocation.
    let ordinary = visible.lines().any(|line| line == HEADER);
    if !ordinary
        && !visible
            .lines()
            .any(|line| line.trim_start() == "+ Exception Group Traceback (most recent call last):")
    {
        return None;
    }
    let summaries = if visible
        .lines()
        .any(|line| line.trim_start() == exception_group::ROOT_HEADER.trim_start())
    {
        chained_summaries(&visible)
    } else {
        summaries(&visible)
    };
    let Some(summaries) = summaries else {
        return Some(output);
    };
    output.symptom = summaries
        .first()
        .map(|summary| crate::policy::redact_secret_like_content(summary).content);
    let mut keys = BTreeSet::new();
    for summary in summaries {
        let item = diagnostic(summary);
        if keys.insert(item.layered_key().key)
            && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
        {
            output.diagnostics.push(item);
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics,
    };

    fn traceback(summary: &str) -> String {
        format!(
            "{HEADER}\n  File \"/private/project/task.py\", line 7, in <module>\n    private_source_sentinel()\n    ~~~~~~~~~~~~~~~~~~~~~~~^^\n{summary}\n"
        )
    }

    #[test]
    fn native_exceptions_use_the_same_key_for_import_and_public_diagnosis() {
        for summary in [
            "ModuleNotFoundError: No module named 'widgets'",
            "TypeError: expected bytes, got str",
            "ValueError: invalid configuration",
            "AssertionError",
            "app.errors.ConfigFailure: missing endpoint",
        ] {
            let text = traceback(summary);
            let decoded = inspect(&text).unwrap();
            assert_eq!(decoded.diagnostics.len(), 1);
            assert!(decoded.failed && !decoded.allows_completion);
            assert_eq!(structured_error_diagnostics(&text), decoded.diagnostics);
            for exit_code in [None, Some(0), Some(1)] {
                let output = ToolOutput {
                    text: text.clone(),
                    is_error: Some(false),
                    exit_code,
                };
                assert_eq!(failure_diagnostics(&output), decoded.diagnostics);
                assert!(output.failed());
                assert!(!output.succeeded());
            }
        }
    }

    #[test]
    fn class_identity_ignores_stack_locations_code_and_styling() {
        let text = traceback("ValueError: invalid configuration");
        let expected = inspect(&text).unwrap().diagnostics;
        for rendered in [
            text.replace("/private/project/task.py", "/other/task.py")
                .replace("line 7", "line 91"),
            text.replace("private_source_sentinel()", "totally_different_source()"),
            format!("ordinary stdout\n{text}"),
            format!("F\n{text}"),
            text.replace('\n', "\r\n"),
            text.replace("ValueError", "\u{1b}[31mValueError\u{1b}[0m"),
        ] {
            assert_eq!(
                super::super::decode(&rendered)
                    .unwrap()
                    .unwrap()
                    .diagnostics,
                expected
            );
        }
        let rendered = format!("{expected:?}");
        assert!(!rendered.contains("private_source_sentinel"));
        assert!(!rendered.contains("/private/project"));
    }

    #[test]
    fn complete_chains_deduplicate_classes_and_reject_damaged_later_sections() {
        let first = traceback("ValueError: invalid configuration");
        let second = traceback("RuntimeError: startup aborted");
        for connector in [CAUSE, CONTEXT] {
            let chain = format!("{first}\n{connector}\n\n{second}");
            let result = inspect(&chain).unwrap();
            assert_eq!(result.diagnostics.len(), 2);
            let repeated = format!("{first}\n{connector}\n\n{first}");
            assert_eq!(inspect(&repeated).unwrap().diagnostics.len(), 1);
            let broken = format!("{chain}\n{connector}\n\n{HEADER}\n  File \"unfinished");
            let decoded = inspect(&broken).unwrap();
            assert!(decoded.failed && !decoded.allows_completion);
            assert!(decoded.diagnostics.is_empty());
        }
    }

    #[test]
    fn malformed_tracebacks_keep_a_failure_veto_without_prefix_fingerprints() {
        let good = traceback("ValueError: invalid configuration");
        for text in [
            HEADER.to_owned(),
            good.replace("line 7", "line -1"),
            good.replace("line 7", "line unknown"),
            good.replace("ValueError: invalid configuration", "OK"),
            format!("{good}Process exited with code 0\n"),
            format!("{good}{HEADER}\n"),
            good.replace("ValueError", "\u{1b}[2JValueError"),
            format!("{good}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES)),
        ] {
            let decoded = inspect(&text).expect("claimed traceback");
            assert!(decoded.failed && !decoded.allows_completion);
            assert!(decoded.diagnostics.is_empty(), "{text:?}");
            assert!(
                !ToolOutput {
                    text,
                    is_error: Some(false),
                    exit_code: Some(0),
                }
                .succeeded()
            );
        }
        for text in [
            "ordinary stdout",
            "ValueError: an example",
            "    Traceback (most recent call last):",
            "{\"example\":\"Traceback (most recent call last):\"}",
        ] {
            assert!(inspect(text).is_none());
        }
    }

    #[test]
    fn credentials_are_redacted_before_fingerprinting_not_after_keying() {
        let credential = format!("ghp_{}", "Q".repeat(36));
        let raw = traceback(&format!("RuntimeError: api_key={credential}"));
        let result = inspect(&raw).unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        let summary = crate::policy::redact_secret_like_content(&format!(
            "RuntimeError: api_key={credential}"
        ))
        .content;
        assert_eq!(
            result.diagnostics,
            inspect(&traceback(&summary)).unwrap().diagnostics
        );
        assert!(!format!("{:?}", result.diagnostics).contains(&credential));
        assert!(!format!("{:?}", result.diagnostics).contains(&credential.to_ascii_lowercase()));
    }

    #[test]
    fn ordinary_exceptions_do_not_downgrade_test_execution_requirements() {
        use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};

        let exception = ToolOutput {
            text: traceback("ValueError: invalid configuration"),
            is_error: Some(true),
            exit_code: Some(1),
        };
        assert!(!Requirement::capture(&exception, true).is_guarded());
        let pytest = ToolOutput {
            text: "=== test session starts ===\ncollected 1 item\ntests/test_queue.py::test_drain FAILED [100%]\n=== 1 failed in 0.01s ===\n".to_owned(),
            is_error: Some(true),
            exit_code: Some(1),
        };
        let mut required = Requirement::capture(&pytest, true);
        assert!(required.is_guarded());
        required.refresh(&exception, true);
        assert!(required.is_guarded());
        let success = ToolOutput {
            text: "done".to_owned(),
            is_error: Some(false),
            exit_code: Some(0),
        };
        assert!(!required.is_verified_by(&Proof::capture(&success)));
    }

    #[test]
    fn unsupported_groups_and_late_chain_overflow_never_certify_success() {
        let group = "  + Exception Group Traceback (most recent call last):\n  | ExceptionGroup: grouped (1 sub-exception)\n";
        let chain = std::iter::repeat_n(traceback("ValueError: failed"), MAX_CHAIN + 1)
            .collect::<Vec<_>>()
            .join(&format!("\n{CAUSE}\n\n"));
        for text in [group, chain.as_str()] {
            let decoded = inspect(text).unwrap();
            assert!(decoded.failed && !decoded.allows_completion);
            assert!(decoded.diagnostics.is_empty());
            assert!(
                !ToolOutput {
                    text: text.to_owned(),
                    is_error: Some(false),
                    exit_code: Some(0),
                }
                .succeeded()
            );
        }
    }
}

#[cfg(test)]
#[path = "traceback_chain_tests.rs"]
mod chain_tests;
