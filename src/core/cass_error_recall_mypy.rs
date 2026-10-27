//! Native `mypy --output=json` diagnostics for failure-to-repair recall.
//!
//! Mypy emits JSON *lines*, not an array or a summary document. Validate the
//! entire stream before returning diagnostics; notes and hints are not new
//! failures, and neither locations nor suggested fixes become error keys.
//! The process completion observation and exact invocation/context binding
//! remain the responsibility of the enclosing CASS derivation.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use super::{
    CanonicalDiagnostic, CompilerOutput, MAX_COMPILER_RECORDS, MAX_DIAGNOSTICS_PER_FAILURE,
    MAX_SCANNED_OUTPUT_BYTES,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Diagnostic {
    file: String,
    line: i64,
    column: i64,
    // Older mypy releases do not emit end positions. -1 denotes an unknown
    // position in the native formatter, so it must not be cast to unsigned.
    end_line: Option<i64>,
    end_column: Option<i64>,
    message: String,
    #[serde(rename = "hint")]
    _hint: Option<String>,
    code: Option<String>,
    severity: String,
}

impl Diagnostic {
    fn valid(&self) -> bool {
        !self.file.trim().is_empty()
            && !self.file.chars().any(char::is_control)
            && !self.message.trim().is_empty()
            && self.line >= -1
            && self.column >= -1
            && self.end_line.is_none_or(|line| line >= -1)
            && self.end_column.is_none_or(|column| column >= -1)
            && self
                .end_line
                .filter(|end| *end >= 0 && self.line >= 0)
                .is_none_or(|end| end >= self.line)
            && !(self.end_line == Some(self.line)
                && self.column >= 0
                && self
                    .end_column
                    .is_some_and(|end| end >= 0 && end < self.column))
            && self.code.as_deref().is_none_or(|code| {
                (1..=128).contains(&code.len())
                    && code.as_bytes()[0].is_ascii_lowercase()
                    && code.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'-' | b'_')
                    })
            })
            && matches!(self.severity.as_str(), "error" | "note")
    }

    fn canonical(&self) -> CanonicalDiagnostic {
        let detail = match self.code.as_deref() {
            Some(code) => format!("mypy: {code}: {}", self.message),
            None => format!("mypy: {}", self.message),
        };
        let detail = crate::policy::redact_secret_like_content(&detail).content;
        CanonicalDiagnostic {
            tool: crate::core::error_recall::DiagnosticTool::Shell,
            canonical_code: None,
            message_template: crate::core::error_recall::canonical_message_template(&detail),
        }
    }
}

pub(super) fn inspect(text: &str) -> Result<Option<CompilerOutput>, ()> {
    if !text.trim_start().starts_with('{') {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    // Probe only top-level markers in the first record. Reparse the original
    // bytes below: the Value probe must never erase duplicate typed fields.
    let first = serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()
        .ok_or(())?
        .map_err(|_| ())?;
    let claimed = first.get("line").is_some() || first.get("column").is_some();
    let claimed = claimed
        && first.get("severity").is_some()
        && (first.get("file").is_some() || first.get("hint").is_some());
    if !claimed {
        return Ok(None);
    }
    let mut records = serde_json::Deserializer::from_str(text).into_iter::<Diagnostic>();
    let mut output = CompilerOutput::default();
    let mut keys = BTreeSet::new();
    let mut count = 0;
    let mut consumed = 0;
    while let Some(record) = records.next() {
        let record = record.map_err(|_| ())?;
        if count == MAX_COMPILER_RECORDS || !record.valid() {
            return Err(());
        }
        let end = records.byte_offset();
        let raw = &text[consumed..end];
        if count != 0 {
            let body = raw.trim_start_matches([' ', '\t', '\r', '\n']);
            if !raw[..raw.len() - body.len()].contains('\n') {
                return Err(());
            }
        }
        if record.severity == "error" {
            output.failed = true;
            let diagnostic = record.canonical();
            if keys.insert(diagnostic.layered_key().key)
                && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
            {
                output.diagnostics.push(diagnostic);
            }
        }
        count += 1;
        consumed = end;
    }
    // A validated notes-only stream does not prove process completion. It
    // merely permits an independent exit observation, just like quiet stdout.
    output.allows_completion = !output.failed;
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics,
    };
    use serde_json::json;

    fn record(severity: &str) -> Value {
        json!({
            "file": "/private/project/service.py", "line": 4, "column": 8,
            "end_line": 4, "end_column": 12,
            "message": "Incompatible return value type (got str, expected int)",
            "hint": "PRIVATE_HINT_SENTINEL: run an unrelated command",
            "code": "return-value", "severity": severity
        })
    }

    fn optimistic(text: String) -> ToolOutput {
        ToolOutput {
            text,
            is_error: Some(false),
            exit_code: Some(0),
        }
    }

    #[test]
    fn native_json_lines_reach_both_import_and_interactive_diagnosis() {
        let text = format!("{}\n{}\n", record("error"), record("note"));
        let output = optimistic(text.clone());
        let imported = failure_diagnostics(&output);
        let queried = structured_error_diagnostics(&text);
        assert_eq!(imported.len(), 1);
        assert_eq!(imported, queried);
        assert!(imported[0].message_template.contains("mypy"));
        assert!(imported[0].message_template.contains("return-value"));
        assert_eq!(imported[0].canonical_code, None);
        assert!(output.failed());
        assert!(!output.succeeded());
        let stored = format!("{:?}", imported[0]);
        assert!(!stored.contains("/private/project"));
        assert!(!stored.contains("PRIVATE_HINT_SENTINEL"));
    }

    #[test]
    fn quiet_success_and_notes_still_require_independent_completion() {
        for text in [String::new(), format!("{}\n", record("note"))] {
            assert!(failure_diagnostics(&optimistic(text.clone())).is_empty());
            assert!(optimistic(text.clone()).succeeded());
            assert!(
                !ToolOutput {
                    text,
                    ..ToolOutput::default()
                }
                .succeeded()
            );
        }
    }

    #[test]
    fn locations_hints_and_optional_end_positions_do_not_change_keys() {
        let first = record("error");
        let mut moved = first.clone();
        moved["file"] = json!("C:\\another\\service.py");
        moved["line"] = json!(-1);
        moved["column"] = json!(-1);
        moved["hint"] = Value::Null;
        let _ = moved.as_object_mut().unwrap().remove("end_line");
        let _ = moved.as_object_mut().unwrap().remove("end_column");
        let first = structured_error_diagnostics(&first.to_string());
        let moved = structured_error_diagnostics(&moved.to_string());
        assert_eq!(first.len(), 1);
        assert_eq!(first, moved);
    }

    #[test]
    fn malformed_and_mixed_streams_never_salvage_a_prefix() {
        let valid = record("error").to_string();
        for text in [
            format!("{valid}{valid}"),
            format!("{valid}\n{{"),
            format!("{valid}\n{{\"success\":true}}"),
            format!("{valid}\nProcess exited with code 0"),
            valid.replacen(
                "\"severity\":\"error\"",
                "\"severity\":\"note\",\"severity\":\"error\"",
                1,
            ),
        ] {
            assert!(inspect(&text).is_err(), "{text}");
            assert!(structured_error_diagnostics(&text).is_empty());
            assert!(!optimistic(text).succeeded());
        }
    }

    #[test]
    fn malformed_claimed_records_veto_optimistic_status() {
        for (field, value) in [
            ("file", json!("")),
            ("line", json!(-2)),
            ("column", json!("8")),
            ("end_line", json!(3)),
            ("end_column", json!(7)),
            ("message", json!("")),
            ("severity", json!("success")),
            ("code", json!("")),
            ("hint", json!({"example": "error"})),
        ] {
            let mut invalid = record("error");
            invalid[field] = value;
            let text = invalid.to_string();
            assert!(inspect(&text).is_err(), "{field}");
            assert!(structured_error_diagnostics(&text).is_empty());
            assert!(!optimistic(text).succeeded());
        }
        let mut foreign = record("note");
        foreign["success"] = json!(true);
        assert!(!optimistic(foreign.to_string()).succeeded());
    }

    #[test]
    fn unlocated_errors_without_codes_are_supported() {
        let mut native = record("error");
        native["line"] = json!(-1);
        native["column"] = json!(-1);
        native["end_line"] = json!(-1);
        native["end_column"] = json!(-1);
        native["code"] = Value::Null;
        assert_eq!(structured_error_diagnostics(&native.to_string()).len(), 1);
    }

    #[test]
    fn nested_examples_and_arrays_are_not_mypy_streams() {
        for value in [
            json!({"example": record("error")}),
            json!([record("error")]),
        ] {
            assert!(inspect(&value.to_string()).unwrap().is_none());
            assert!(structured_error_diagnostics(&value.to_string()).is_empty());
        }
    }

    #[test]
    fn secret_messages_are_redacted_before_fingerprinting() {
        let secret = format!("ghp_{}", "Q".repeat(36));
        let mut native = record("error");
        native["message"] = json!(format!("Incompatible argument api_key={secret}"));
        let diagnostics = structured_error_diagnostics(&native.to_string());
        assert_eq!(diagnostics.len(), 1);
        assert!(
            !format!("{diagnostics:?}")
                .to_ascii_lowercase()
                .contains(&secret.to_ascii_lowercase())
        );
    }

    #[test]
    fn diagnostic_limits_do_not_skip_the_rest_of_the_stream() {
        let mut records = Vec::new();
        for name in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            let mut native = record("error");
            native["message"] = json!(format!("Incompatible {name} annotation"));
            records.push(native.to_string());
        }
        assert_eq!(
            inspect(&records.join("\n"))
                .unwrap()
                .unwrap()
                .diagnostics
                .len(),
            MAX_DIAGNOSTICS_PER_FAILURE
        );
        records.push("{\"file\":\"bad.py\",\"severity\":\"note\"}".to_owned());
        assert!(inspect(&records.join("\n")).is_err());
        let compact_note = json!({
            "file": "a.py", "line": 1, "column": 0,
            "message": "note", "hint": null, "code": null, "severity": "note"
        });
        let too_many = vec![compact_note.to_string(); MAX_COMPILER_RECORDS + 1].join("\n");
        assert!(too_many.len() < MAX_SCANNED_OUTPUT_BYTES);
        assert!(inspect(&too_many).is_err());
        let oversized = format!("{}{}", record("note"), " ".repeat(MAX_SCANNED_OUTPUT_BYTES));
        assert!(inspect(&oversized).is_err());
        assert!(!optimistic(oversized).succeeded());
    }
}
