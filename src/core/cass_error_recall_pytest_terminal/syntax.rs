//! Standalone CPython parser failures (which have no Traceback header).
//!
//! Validate the complete location/source/caret/summary projection, then retain
//! only the redacted exception summary. A recognized incomplete or unsupported
//! report remains a failure veto, never partial diagnostics or success proof.

use super::{Decoded, Execution, MAX_LINES, MAX_SCANNED_OUTPUT_BYTES, diagnostic, terminal};

fn location(line: &str) -> bool {
    let Some((path, number)) = line
        .strip_prefix("  File \"")
        .and_then(|rest| rest.rsplit_once("\", line "))
    else {
        return false;
    };
    !path.is_empty()
        && !path.chars().any(char::is_control)
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && number.parse::<u64>().is_ok_and(|value| value > 0)
}

fn exception_summary(line: &str) -> bool {
    line.split_once(": ").is_some_and(|(kind, message)| {
        matches!(kind, "SyntaxError" | "IndentationError" | "TabError")
            && !message.trim().is_empty()
            && !line.chars().any(char::is_control)
    })
}

fn caret(line: &str) -> bool {
    line.strip_prefix("    ").is_some_and(|marker| {
        marker.contains('^')
            && marker
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'^' | b'~'))
    })
}

/// Parser errors have one location, optional rendered source and caret, and
/// one terminal summary. Notes, trace chains, multiple diagnostics and compiler
/// wrappers are not silently spliced together. Even a valid prefix is withheld.
fn summary(text: &str) -> Option<&str> {
    // CPython terminates the report with a newline; a partial final summary
    // is not a completed diagnostic, even if its prefix resembles a class.
    if !text.ends_with('\n') {
        return None;
    }
    let mut lines = text.lines().skip_while(|line| line.trim().is_empty());
    if !location(lines.next()?) {
        return None;
    }
    let mut line = lines.next()?;
    if !exception_summary(line) {
        // Source text is opaque, including strings spelling fake exceptions or
        // success trailers. The interpreter, not this decoder, owns its syntax.
        line.strip_prefix("    ")?;
        line = lines.next()?;
        if caret(line) {
            line = lines.next()?;
        }
    }
    (exception_summary(line) && lines.all(|line| line.trim().is_empty())).then_some(line)
}

pub(super) fn inspect(text: &str) -> Option<Decoded> {
    // Family detection is bounded even when a caller passes oversized output.
    // Only the first nonblank line may be a standalone parser location. A
    // quoted example later in ordinary stdout is not a new diagnostic stream.
    let mut end = text.len().min(MAX_SCANNED_OUTPUT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let probe = text[..end].lines().find(|line| !line.trim().is_empty())?;
    if !probe.contains("  File \"") {
        return None;
    }
    let first = match terminal::normalize(probe) {
        Ok(first) => first,
        Err(()) if probe.starts_with("  File \"") => std::borrow::Cow::Borrowed(probe),
        Err(()) => return None,
    };
    if !first.starts_with("  File \"") {
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
    let Ok(visible) = terminal::normalize(text) else {
        return Some(output);
    };
    if let Some(summary) = summary(&visible) {
        output.symptom = Some(crate::policy::redact_secret_like_content(summary).content);
        output.diagnostics.push(diagnostic(summary));
    }
    Some(output)
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "syntax_store_tests.rs"]
mod store_tests;
