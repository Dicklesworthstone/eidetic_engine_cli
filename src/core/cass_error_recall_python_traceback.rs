//! Native Python script exceptions, separate from pytest/unittest reports.
//!
//! Only complete top-level interpreter reports supply diagnostics. Stack paths,
//! source lines and exception notes never become fingerprint text. Tracebacks
//! veto completion; they cannot prove that a command or its retry succeeded.

use super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};
use crate::core::error_recall::{DiagnosticTool, canonical_message_template};

const HEADER: &str = "Traceback (most recent call last):";
const DIRECT: &str = "The above exception was the direct cause of the following exception:";
const HANDLING: &str = "During handling of the above exception, another exception occurred:";
const MAX_FRAMES: usize = 512;
const MAX_CHAINS: usize = 32;

fn group_header(line: &str) -> bool {
    line.trim_start().starts_with("+ Exception Group Traceback")
}

fn positive_decimal(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 10
        && text.bytes().all(|value| value.is_ascii_digit())
        && text.parse::<u32>().is_ok_and(|value| value > 0)
}

fn frame(line: &str) -> bool {
    let Some(body) = line.strip_prefix("  File \"") else {
        return false;
    };
    let Some((path, position)) = body.rsplit_once("\", line ") else {
        return false;
    };
    let (number, function) = position
        .split_once(", in ")
        .map_or((position, None), |(n, f)| (n, Some(f)));
    !path.is_empty()
        && !path.chars().any(char::is_control)
        && positive_decimal(number)
        && function.is_none_or(|name| !name.is_empty() && !name.chars().any(char::is_control))
}

fn repeated_frame(line: &str) -> bool {
    line.strip_prefix("  [Previous line repeated ")
        .and_then(|rest| rest.strip_suffix(" more times]"))
        .is_some_and(positive_decimal)
}

fn exception(line: &str) -> Option<(&str, &str)> {
    let (kind, message) = line.split_once(':').unwrap_or((line, ""));
    if kind.is_empty()
        || kind.len() > 128
        || !kind.split('.').all(|part| {
            let mut bytes = part.bytes();
            bytes
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    {
        return None;
    }
    Some((kind, message.trim()))
}

fn mask_literals(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut quote = None;
    let mut escaped = false;
    for character in text.chars() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
        } else if matches!(character, '\'' | '"' | '`') {
            quote = Some(character);
            output.push_str("<id>");
        } else {
            output.push(character);
        }
    }
    output
}

fn canonical(kind: &str, message: &str) -> CanonicalDiagnostic {
    let message = crate::policy::redact_secret_like_content(message).content;
    CanonicalDiagnostic {
        tool: DiagnosticTool::Shell,
        // ValueError/RuntimeError are not native diagnostic codes. Keep their
        // masked message in identity rather than merging every use of a type.
        canonical_code: None,
        message_template: canonical_message_template(&format!(
            "Python exception {kind}: {}",
            mask_literals(&message),
        )),
    }
}

/// None belongs to another producer. Err is an incomplete/unsupported claimed
/// report and may never be treated as clean stdout, including after the output
/// diagnostic cap. Other producers are left to their existing native decoders.
pub(super) fn decode(text: &str) -> Result<Option<Vec<CanonicalDiagnostic>>, ()> {
    // Probe only the first top-level line. Unrelated producers retain their
    // own byte/control policies; captured examples inside JSON are not traces.
    let Some(prefix) = text.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    if prefix.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Ok(None);
    }
    let Ok(prefix) = super::outcome::terminal_text(prefix) else {
        return Ok(None);
    };
    if !prefix.starts_with("Traceback (most recent call last)")
        && !prefix.starts_with("  File \"")
        && !group_header(&prefix)
    {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    let visible = super::outcome::terminal_text(text)?;
    let mut lines = visible.lines();
    let Some(mut first) = lines.find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    if group_header(first)
        || (first.starts_with("Traceback (most recent call last)") && first != HEADER)
    {
        return Err(());
    }
    if first != HEADER && !first.starts_with("  File \"") {
        return Ok(None);
    }
    let mut diagnostics = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut total_frames = 0_usize;
    for _ in 0..MAX_CHAINS {
        let syntax_only = first != HEADER;
        let mut frames = 0_usize;
        let mut current = if syntax_only {
            Some(first)
        } else {
            lines.next()
        };
        let diagnostic = loop {
            let line = current.ok_or(())?;
            if line.starts_with("  File ") {
                if !frame(line) {
                    return Err(());
                }
                frames += 1;
                total_frames += 1;
                if total_frames > MAX_FRAMES {
                    return Err(());
                }
            } else if frames > 0 && (line.starts_with("    ") || repeated_frame(line)) {
                // Interpreter-rendered source/caret rows carry no authority.
            } else {
                if frames == 0 {
                    return Err(());
                }
                let (kind, message) = exception(line).ok_or(())?;
                if matches!(
                    kind.rsplit('.').next(),
                    Some("ExceptionGroup" | "BaseExceptionGroup")
                ) || (syntax_only
                    && !matches!(kind, "SyntaxError" | "IndentationError" | "TabError"))
                {
                    return Err(());
                }
                break canonical(kind, message);
            }
            current = lines.next();
        };
        if diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
            && seen.insert(diagnostic.message_template.clone())
        {
            diagnostics.push(diagnostic);
        }
        // Python permits multiline messages and add_note() output. Preserve
        // those in the original transcript, not as extra exception classes.
        // A chain separator, however, commits us to validating the NEXT trace.
        let mut next = None;
        while let Some(line) = lines.next() {
            if matches!(line, DIRECT | HANDLING) {
                next = Some(lines.find(|line| !line.trim().is_empty()).ok_or(())?);
                break;
            }
            if line.starts_with("Traceback (most recent call last)")
                || group_header(line)
                || line.starts_with("  File ")
            {
                return Err(());
            }
        }
        match next {
            None => return Ok(Some(diagnostics)),
            Some(line) if line == HEADER || line.starts_with("  File \"") => first = line,
            Some(_) => return Err(()),
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics,
    };

    fn trace(message: &str) -> String {
        format!(
            "{HEADER}\n  File \"/private/project/run.py\", line 12, in <module>\n    PRIVATE_SOURCE()\n    ~~~~~~~~~~~~~~^^\n{message}\n"
        )
    }

    #[test]
    fn script_exceptions_share_import_and_interactive_templates_without_inventing_codes() {
        let text = trace("ValueError: invalid literal for int() with base 10: 'private-value'");
        let output = ToolOutput {
            text: text.clone(),
            is_error: Some(false),
            exit_code: Some(0),
        };
        let imported = failure_diagnostics(&output);
        assert_eq!(imported.len(), 1);
        assert_eq!(imported, structured_error_diagnostics(&text));
        assert_eq!(
            crate::core::cass_error_recall::symptom_line(&output),
            Some(imported[0].message_template.clone()),
        );
        assert_eq!(imported[0].tool, DiagnosticTool::Shell);
        assert!(imported[0].canonical_code.is_none());
        assert!(imported[0].layered_key().key.starts_with("shell:tmpl:"));
        assert_eq!(
            imported[0].message_template,
            "python exception valueerror: invalid literal for int() with base <num> <id>"
        );
        assert!(output.failed());
        assert!(!output.succeeded());
        assert_eq!(output.text, text);
        for secret in ["PRIVATE_SOURCE", "/private/project", "private-value"] {
            assert!(!imported[0].message_template.contains(secret));
        }
        let distinct = decode(&trace("ValueError: incompatible tensor dimensions"))
            .unwrap()
            .unwrap();
        assert_ne!(distinct[0].layered_key().key, imported[0].layered_key().key);
    }

    #[test]
    fn complete_chains_and_notes_keep_distinct_causes_but_not_note_or_stack_contents() {
        for separator in [DIRECT, HANDLING] {
            let first = trace("KeyError: 'PRIVATE_LOOKUP'");
            let last = trace("package.BuildFailure: could not load configuration");
            let text = format!("{first}PRIVATE_NOTE\n\n{separator}\n\n{last}another note\n");
            let parsed = decode(&text).unwrap().unwrap();
            assert_eq!(parsed.len(), 2);
            assert!(
                parsed[0]
                    .message_template
                    .starts_with("python exception keyerror:")
            );
            assert!(
                parsed[1]
                    .message_template
                    .contains("could not load configuration")
            );
            assert!(
                parsed
                    .iter()
                    .all(|entry| !entry.message_template.contains("private"))
            );
            let duplicate = format!("{first}\n{separator}\n\n{first}");
            assert_eq!(decode(&duplicate).unwrap().unwrap().len(), 1);
        }
    }

    #[test]
    fn syntax_errors_and_colorized_reports_are_failures_not_success_signals() {
        for kind in ["SyntaxError", "IndentationError", "TabError"] {
            let text = format!(
                "  File \"C:\\private\\run.py\", line 3\n    if True print('x')\n            ^^^^^\n{kind}: invalid syntax\n"
            );
            assert_eq!(decode(&text).unwrap().unwrap().len(), 1);
            let colored = format!("\u{1b}[31m{text}\u{1b}[0m");
            assert_eq!(decode(&colored).unwrap(), decode(&text).unwrap());
            assert!(
                !ToolOutput {
                    text: colored,
                    exit_code: Some(0),
                    ..ToolOutput::default()
                }
                .succeeded()
            );
        }
        let recursive = format!(
            "{HEADER}\n  File \"run.py\", line 2, in recur\n  [Previous line repeated 998 more times]\nRecursionError: maximum recursion depth exceeded\n"
        );
        assert_eq!(decode(&recursive).unwrap().unwrap().len(), 1);
        assert_eq!(
            decode(&trace("KeyboardInterrupt")).unwrap().unwrap().len(),
            1
        );
    }

    #[test]
    fn malformed_or_incomplete_reports_refuse_the_whole_claim_even_after_the_cap() {
        for text in [
            "Traceback (most recent call last)".to_owned(),
            "Traceback (most recent call last): unfinished".to_owned(),
            format!("{HEADER}\n"),
            format!("{HEADER}\nValueError: missing frames"),
            format!("{HEADER}\n  File \"x.py\", line nope, in f\nValueError: bad"),
            format!("{HEADER}\n  File \"x.py\", line 0, in f\nValueError: bad"),
            format!("{HEADER}\n  File \"x.py\", line 1, in f\n    pending()\n"),
            format!("{}\n{DIRECT}\n", trace("ValueError: bad")),
            format!("{}\n{HANDLING}\nnot a traceback", trace("ValueError: bad")),
            "  File \"x.py\", line 2\n    x\nValueError: not a syntax report".to_owned(),
            "  + Exception Group Traceback (most recent call last):\n  | ExceptionGroup: broken (2 sub-exceptions)".to_owned(),
        ] {
            assert!(decode(&text).is_err(), "{text:?}");
            assert!(structured_error_diagnostics(&text).is_empty());
            assert!(!ToolOutput { text, exit_code: Some(0), ..ToolOutput::default() }.succeeded());
        }
        let valid = (0..7)
            .map(|n| {
                trace(&format!(
                    "RuntimeError: failure class_{}",
                    char::from(b'a' + n)
                ))
            })
            .collect::<Vec<_>>()
            .join(&format!("\n{DIRECT}\n\n"));
        assert_eq!(
            decode(&valid).unwrap().unwrap().len(),
            MAX_DIAGNOSTICS_PER_FAILURE
        );
        assert!(decode(&format!("{valid}\n{DIRECT}\n{HEADER}\n")).is_err());
        assert!(
            decode(&format!(
                "{}{}",
                trace("ValueError: bad"),
                "x".repeat(MAX_SCANNED_OUTPUT_BYTES)
            ))
            .is_err()
        );
    }

    #[test]
    fn quoted_json_prose_and_other_producers_cannot_create_python_fingerprints() {
        let report = trace("ValueError: bad");
        for text in [
            format!("Example traceback:\n{report}"),
            format!("```python\n{report}```"),
            serde_json::json!({"example": report}).to_string(),
            "ValueError: plain prose without a traceback".to_owned(),
            "All tests passed".to_owned(),
        ] {
            assert!(decode(&text).unwrap().is_none(), "{text}");
        }
        let empty = ToolOutput {
            text: String::new(),
            exit_code: Some(0),
            ..ToolOutput::default()
        };
        assert!(empty.succeeded());
    }
}
