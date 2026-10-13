//! Bounded compiler JSON diagnostics and completion checks.
//!
//! Cargo's compiler-message embeds a rustc diagnostic. Use its typed level,
//! code and message, never rendered output, children, artifacts or examples.
//! Pyright and ESLint reports use their own complete-document validators.
//! Neither a build-finished marker nor an analysis summary is process success:
//! they only permit an independent completion observation. Any error wins.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};

#[path = "cass_error_recall_libtest.rs"]
mod libtest;

#[path = "cass_error_recall_checkers.rs"]
mod checkers;

const MAX_COMPILER_RECORDS: usize = 256;

#[derive(Debug, Default)]
pub(super) struct CompilerOutput {
    pub diagnostics: Vec<CanonicalDiagnostic>,
    pub failed: bool,
    pub allows_completion: bool,
    /// Only actual libtest streams may enter the test-inventory projection.
    pub has_test_execution: bool,
    /// Transient commitments to linted files, never source text or provenance.
    pub eslint_coverage: Option<EslintCoverage>,
}

#[derive(Debug, Default)]
pub(super) struct EslintCoverage {
    pub checked_files: BTreeSet<[u8; 32]>,
    pub failed_files: BTreeSet<[u8; 32]>,
}

#[derive(Deserialize)]
struct Code {
    code: String,
}

#[derive(Deserialize)]
struct Diagnostic {
    #[serde(rename = "$message_type")]
    message_type: Option<String>,
    message: String,
    code: Option<Code>,
    level: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Message {
    Text(String),
    Diagnostic(Diagnostic),
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    event: Option<String>,
    schema: Option<String>,
    reason: Option<String>,
    #[serde(rename = "$message_type")]
    message_type: Option<String>,
    message: Option<Message>,
    code: Option<Code>,
    level: Option<String>,
    success: Option<bool>,
}

fn cargo_reason(reason: &str) -> bool {
    matches!(
        reason,
        "compiler-message" | "compiler-artifact" | "build-script-executed" | "build-finished"
    )
}

/// None means another output family, not successful compiler output. Invalid
/// or conflicting streams are errors, never a salvaged prefix. Cargo may
/// transition to libtest only after a successful build-finished event. Typed
/// deserialization rejects duplicate discriminator, level, code and status
/// fields while tolerating unrelated future Rust producer metadata.
pub(super) fn inspect(text: &str) -> Result<Option<CompilerOutput>, ()> {
    let start = text.trim_start();
    if !start.starts_with(['{', '[']) {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    if let Some(output) = checkers::inspect(text)? {
        return Ok(Some(output));
    }
    if start.starts_with('[') {
        // Unrelated arrays are data, not an object-shaped compiler stream.
        return Ok(None);
    }
    // Identify the family before interpreting generic field names like code
    // or message. An unrelated producer may legitimately use other shapes.
    // Only the first record is probed; the selected compiler stream below is
    // then validated completely, including trailing records and separators.
    #[derive(Deserialize)]
    struct Family {
        #[serde(rename = "type")]
        kind: Option<Value>,
        reason: Option<Value>,
        #[serde(rename = "$message_type")]
        message_type: Option<Value>,
    }
    let family = serde_json::Deserializer::from_str(text)
        .into_iter::<Family>()
        .next()
        .ok_or(())?
        .map_err(|_| ())?;
    if !family
        .reason
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(cargo_reason)
        && family.message_type.as_ref().and_then(Value::as_str) != Some("diagnostic")
        && !matches!(
            family.kind.as_ref().and_then(Value::as_str),
            Some("suite" | "test")
        )
    {
        return Ok(None);
    }
    let mut records = serde_json::Deserializer::from_str(text).into_iter::<Record>();
    let mut output = CompilerOutput::default();
    let mut keys = BTreeSet::new();
    let mut count = 0;
    let mut consumed = 0;
    let mut cargo = false;
    let mut rustc = false;
    let mut finished = None;
    let mut harness = libtest::Harness::default();
    while let Some(record) = records.next() {
        let record = record.map_err(|_| ())?;
        if count == MAX_COMPILER_RECORDS {
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
        if record.kind.is_some() {
            if rustc
                || (cargo && finished != Some(true))
                || record.schema.is_some()
                || record.message_type.is_some()
                || record.code.is_some()
                || record.level.is_some()
                || record.success.is_some()
                || record.reason.as_deref().is_some_and(cargo_reason)
            {
                return Err(());
            }
            if let Some(diagnostic) = harness.observe(raw)?
                && keys.insert(diagnostic.layered_key().key)
                && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
            {
                output.diagnostics.push(diagnostic);
            }
            output.failed |= harness.failed();
            count += 1;
            consumed = end;
            continue;
        }
        // Test binaries may follow Cargo's build, never precede more compiler
        // output. A stray event field cannot masquerade as a build record.
        if harness.seen() || record.event.is_some() {
            return Err(());
        }
        let diagnostic = match (record.reason.as_deref(), record.message_type.as_deref()) {
            (Some("compiler-message"), None) => {
                if record.schema.is_some()
                    || record.level.is_some()
                    || record.code.is_some()
                    || record.success.is_some()
                    || finished.is_some()
                {
                    return Err(());
                }
                cargo = true;
                let Some(Message::Diagnostic(diagnostic)) = record.message else {
                    return Err(());
                };
                Some(diagnostic)
            }
            (Some("compiler-artifact" | "build-script-executed" | "build-finished"), None) => {
                if record.schema.is_some()
                    || record.message.is_some()
                    || record.level.is_some()
                    || record.code.is_some()
                    || finished.is_some()
                {
                    return Err(());
                }
                cargo = true;
                if record.reason.as_deref() == Some("build-finished") {
                    let success = record.success.ok_or(())?;
                    output.failed |= !success;
                    finished = Some(success);
                } else if record.success.is_some() {
                    return Err(());
                }
                None
            }
            (None, Some("diagnostic")) => {
                if record.schema.is_some() || record.success.is_some() {
                    return Err(());
                }
                rustc = true;
                let Some(Message::Text(message)) = record.message else {
                    return Err(());
                };
                Some(Diagnostic {
                    message_type: record.message_type,
                    message,
                    code: record.code,
                    level: record.level.ok_or(())?,
                })
            }
            // In particular, a known reason paired with a conflicting native
            // discriminator cannot fall through to generic successful JSON.
            _ => return Err(()),
        };
        if cargo && rustc {
            return Err(());
        }
        if let Some(diagnostic) = diagnostic {
            if diagnostic
                .message_type
                .as_deref()
                .is_some_and(|kind| kind != "diagnostic")
                || diagnostic.message.trim().is_empty()
            {
                return Err(());
            }
            match diagnostic.level.as_str() {
                "error" | "ice" | "error: internal compiler error" => {
                    output.failed = true;
                    // Preserve the E#### class used by plain rustc output.
                    // Named lints and other codes use the redacted message
                    // signature instead of persisting arbitrary code text.
                    let code = diagnostic
                        .code
                        .as_ref()
                        .map(|code| code.code.as_str())
                        .filter(|code| {
                            code.len() == 5
                                && code.starts_with('E')
                                && code.as_bytes()[1..].iter().all(u8::is_ascii_digit)
                        });
                    let message =
                        crate::policy::redact_secret_like_content(&diagnostic.message).content;
                    let canonical = super::from_rustc(code, &message);
                    if keys.insert(canonical.layered_key().key)
                        && output.diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE
                    {
                        output.diagnostics.push(canonical);
                    }
                }
                // The compiler's aggregate failure note is not another error
                // class. Warnings/help are never promoted by error-like prose.
                "failure-note" => output.failed = true,
                "warning" | "note" | "help" => {}
                _ => return Err(()),
            }
        }
        count += 1;
        consumed = end;
    }
    output.has_test_execution = harness.seen();
    output.allows_completion = !output.failed
        && (!cargo || finished == Some(true))
        && (!harness.seen() || harness.allows_completion());
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{ToolOutput, failure_diagnostics};
    use serde_json::json;

    fn diagnostic(code: &str, level: &str) -> Value {
        json!({
            "$message_type": "diagnostic",
            "message": "the trait bound Widget: Serialize is not satisfied",
            "code": {"code": code, "explanation": null},
            "level": level,
            "spans": [], "children": [], "rendered": "PRIVATE_RENDERED_SENTINEL"
        })
    }

    fn cargo(message: Value) -> String {
        json!({"reason": "compiler-message", "message": message}).to_string()
    }

    fn parsed(text: &str) -> CompilerOutput {
        inspect(text)
            .expect("valid compiler stream")
            .expect("compiler output")
    }

    #[test]
    fn cargo_jsonl_preserves_exact_rustc_codes_and_deduplicates_failures() {
        let error = cargo(diagnostic("E0277", "error"));
        let text = format!(
            "{{\"reason\":\"compiler-artifact\",\"fresh\":true}}\n{error}\n{error}\n{}\n{{\"reason\":\"build-finished\",\"success\":false}}\n",
            cargo(diagnostic("E0308", "warning"))
        );
        let result = parsed(&text);
        assert!(result.failed);
        assert!(!result.allows_completion);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].layered_key().key, "rustc:E0277");
        assert!(
            !result.diagnostics[0]
                .message_template
                .contains("private_rendered_sentinel")
        );
        let from_import = failure_diagnostics(&ToolOutput {
            text,
            is_error: Some(true),
            exit_code: Some(101),
        });
        assert_eq!(from_import, result.diagnostics);
    }

    #[test]
    fn standalone_rustc_diagnostics_use_the_same_class_and_redact_before_keying() {
        let raw = diagnostic("E0277", "error").to_string();
        assert_eq!(
            parsed(&raw).diagnostics,
            parsed(&cargo(diagnostic("E0277", "error"))).diagnostics
        );
        let secret = "sk-proj-ABCDEF1234567890ABCDEF1234567890";
        let mut message = diagnostic("E0277", "error");
        message["code"] = Value::Null;
        message["message"] = json!(format!("unable to authenticate api_key={secret}"));
        let result = parsed(&message.to_string());
        assert_eq!(result.diagnostics.len(), 1);
        assert!(
            !result.diagnostics[0]
                .message_template
                .contains(&secret.to_ascii_lowercase())
        );
    }

    #[test]
    fn warnings_and_build_markers_never_supply_process_completion_by_themselves() {
        let warning = cargo(diagnostic("E0277", "warning"));
        assert!(parsed(&warning).diagnostics.is_empty());
        assert!(!parsed(&warning).allows_completion);
        let complete = format!("{warning}\n{{\"reason\":\"build-finished\",\"success\":true}}");
        assert!(parsed(&complete).allows_completion);
        let result = ToolOutput {
            text: complete,
            is_error: None,
            exit_code: None,
        };
        assert!(
            !result.succeeded(),
            "build completion is not process completion"
        );
        assert!(
            ToolOutput {
                exit_code: Some(0),
                ..result
            }
            .succeeded()
        );
    }

    #[test]
    fn compiler_failure_overrides_optimistic_wrapper_and_build_success() {
        for text in [
            format!(
                "{}\n{{\"reason\":\"build-finished\",\"success\":true}}",
                cargo(diagnostic("E0308", "error"))
            ),
            "{\"reason\":\"build-finished\",\"success\":false}".to_owned(),
            json!({"$message_type":"diagnostic","level":"failure-note","message":"aborting due to previous errors","code":null}).to_string(),
        ] {
            let output = ToolOutput {
                text,
                is_error: Some(false),
                exit_code: Some(0),
            };
            assert!(output.failed());
            assert!(!output.succeeded());
        }
    }

    #[test]
    fn malformed_duplicate_or_mixed_records_cannot_salvage_a_valid_prefix() {
        let valid = cargo(diagnostic("E0277", "error"));
        for text in [
            format!("{valid}\n{{\"reason\":"),
            format!("{valid}\n{{\"example\":{{\"level\":\"error\"}}}}"),
            format!("{valid} {valid}"),
            format!("{valid}\n{}", diagnostic("E0308", "error")),
            "{\"reason\":\"build-finished\",\"success\":false,\"success\":true}".to_owned(),
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"level\":\"warning\",\"message\":\"failed\",\"code\":null}}".to_owned(),
            "{\"$message_type\":\"diagnostic\",\"level\":\"error\",\"message\":\"failed\",\"code\":{\"code\":\"E0277\",\"code\":\"E0308\"}}".to_owned(),
            "{\"reason\":\"compiler-message\",\"$message_type\":\"diagnostic\",\"message\":\"conflicting shapes\",\"level\":\"error\"}".to_owned(),
        ] {
            assert!(inspect(&text).is_err(), "{text}");
            let output = ToolOutput {
                text,
                is_error: Some(false),
                exit_code: Some(0),
            };
            assert!(!output.succeeded());
            assert!(failure_diagnostics(&output).is_empty());
        }
    }

    #[test]
    fn unrelated_json_and_nested_diagnostic_examples_do_not_become_errors() {
        for value in [
            json!({"example": diagnostic("E0277", "error")}),
            json!({"schema":"ee.response.v2","success":true,"data":{}}),
            json!({"reason":"some-other-producer","message":"error[E0277]: quoted"}),
            json!({"message":{"unexpected":"other producer"},"code":false,"reason":{}}),
        ] {
            assert!(inspect(&value.to_string()).expect("other family").is_none());
        }
    }

    #[test]
    fn record_and_byte_bounds_are_enforced_after_the_diagnostic_output_cap() {
        let text = (0..8)
            .map(|number| cargo(diagnostic(&format!("E{number:04}"), "error")))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parsed(&text).diagnostics.len(), MAX_DIAGNOSTICS_PER_FAILURE);
        assert!(inspect(&format!("{text}\n{{")).is_err());
        let artifact = "{\"reason\":\"compiler-artifact\"}";
        let exact = vec![artifact; MAX_COMPILER_RECORDS].join("\n");
        assert!(inspect(&exact).is_ok());
        assert!(inspect(&format!("{exact}\n{artifact}")).is_err());
        let oversized = format!(
            "{}{}",
            diagnostic("E0277", "error"),
            " ".repeat(MAX_SCANNED_OUTPUT_BYTES)
        );
        assert!(inspect(&oversized).is_err());
    }

    #[test]
    fn fatal_named_lints_and_internal_compiler_errors_use_message_signatures() {
        for (code, level) in [
            ("unused_variables", "error"),
            ("clippy::needless_borrow", "error"),
            ("", "error: internal compiler error"),
        ] {
            let result = parsed(&diagnostic(code, level).to_string());
            assert!(result.failed);
            assert!(!result.allows_completion);
            assert_eq!(result.diagnostics.len(), 1);
            assert_eq!(result.diagnostics[0].canonical_code, None);
            assert!(
                result.diagnostics[0]
                    .layered_key()
                    .key
                    .starts_with("rustc:tmpl:blake3:")
            );
        }
        assert!(
            parsed(&diagnostic("unused_variables", "warning").to_string())
                .diagnostics
                .is_empty()
        );
    }

    fn pyright_diagnostic(severity: &str, message: &str) -> Value {
        json!({
            "file": "/private/project/module.py",
            "severity": severity,
            "message": message,
            "rule": "reportArgumentType",
            "range": {
                "start": {"line": 2, "character": 0},
                "end": {"line": 2, "character": 5}
            }
        })
    }

    fn pyright_report(diagnostics: Vec<Value>) -> Value {
        let count = |severity: &str| {
            diagnostics
                .iter()
                .filter(|item| item["severity"] == severity)
                .count()
        };
        json!({
            "version": "1.1.400",
            "time": "2026-10-13T12:00:00Z",
            "summary": {
                "filesAnalyzed": 3,
                "errorCount": count("error"),
                "warningCount": count("warning"),
                "informationCount": count("information"),
                "timeInSec": 0.1
            },
            "generalDiagnostics": diagnostics
        })
    }

    #[test]
    fn pyright_errors_reach_import_and_native_log_recall_with_identical_keys() {
        let value = pyright_report(vec![
            pyright_diagnostic("error", "Argument type is incompatible"),
            pyright_diagnostic("warning", "error[E0277]: this is only warning text"),
            pyright_diagnostic("information", "The inferred type is int"),
        ]);
        let text = value.to_string();
        let expected = parsed(&text).diagnostics;
        assert_eq!(expected.len(), 1);
        assert!(expected[0].message_template.starts_with("pyright:"));
        assert_eq!(expected[0].canonical_code, None);
        assert_eq!(super::super::structured_error_diagnostics(&text), expected);
        for exit_code in [None, Some(0), Some(1)] {
            let output = ToolOutput {
                text: text.clone(),
                is_error: Some(false),
                exit_code,
            };
            assert_eq!(failure_diagnostics(&output), expected);
            assert!(output.failed());
            assert!(!output.succeeded());
        }
    }

    #[test]
    fn pyright_completion_requires_analyzed_files_and_independent_process_success() {
        for diagnostics in [
            Vec::new(),
            vec![pyright_diagnostic("warning", "Deprecated annotation")],
        ] {
            let mut value = pyright_report(diagnostics);
            let output = ToolOutput {
                text: value.to_string(),
                ..ToolOutput::default()
            };
            assert!(parsed(&output.text).allows_completion);
            assert!(failure_diagnostics(&output).is_empty());
            assert!(!output.succeeded());
            assert!(
                ToolOutput {
                    exit_code: Some(0),
                    ..output.clone()
                }
                .succeeded()
            );
            assert!(
                ToolOutput {
                    is_error: Some(false),
                    ..output.clone()
                }
                .succeeded()
            );
            assert!(
                !ToolOutput {
                    exit_code: Some(1),
                    ..output
                }
                .succeeded()
            );
            value["summary"]["filesAnalyzed"] = json!(0);
            let output = ToolOutput {
                text: value.to_string(),
                is_error: Some(false),
                exit_code: Some(0),
            };
            assert!(!output.succeeded(), "empty analysis is not a repair proof");
        }
    }

    #[test]
    fn pyright_keys_ignore_source_locations_time_and_version_and_redact_messages() {
        let mut first = pyright_report(vec![pyright_diagnostic(
            "error",
            "Argument type is incompatible",
        )]);
        let before = parsed(&first.to_string()).diagnostics;
        first["version"] = json!("1.1.401");
        first["time"] = json!("a later source timestamp");
        first["generalDiagnostics"][0]["file"] = json!("/other/private/location.py");
        first["generalDiagnostics"][0]["range"]["end"]["line"] = json!(100);
        let after = parsed(&first.to_string()).diagnostics;
        assert_eq!(before, after);
        assert!(!format!("{after:?}").contains("/other/private"));
        let credential = format!("ghp_{}", "Q".repeat(36));
        first["generalDiagnostics"][0]["message"] = json!(format!("api_key={credential}"));
        first["generalDiagnostics"][0]["rule"] = json!(format!("api_key={credential}"));
        let redacted = parsed(&first.to_string()).diagnostics;
        assert!(!format!("{redacted:?}").contains(&credential));
        assert!(!format!("{redacted:?}").contains(&credential.to_ascii_lowercase()));
    }

    #[test]
    fn pyright_duplicate_classes_do_not_consume_the_diagnostic_budget() {
        let first = pyright_diagnostic("error", "Argument type is incompatible");
        let second = pyright_diagnostic("error", "Import cannot be resolved");
        let result = parsed(&pyright_report(vec![first.clone(), first, second]).to_string());
        assert_eq!(result.diagnostics.len(), 2);
        assert!(result.failed);
        assert!(!result.allows_completion);
    }

    #[test]
    fn pyright_malformed_reports_cannot_hide_errors_or_certify_repairs() {
        let original = pyright_report(vec![pyright_diagnostic("error", "Invalid argument")]);
        let mut malformed = Vec::new();
        for pointer in [
            "/summary/errorCount",
            "/summary/warningCount",
            "/summary/informationCount",
        ] {
            let mut value = original.clone();
            *value.pointer_mut(pointer).unwrap() = json!(99);
            malformed.push(value);
        }
        for (pointer, replacement) in [
            ("/generalDiagnostics", Value::Null),
            ("/generalDiagnostics/0/severity", json!("future_severity")),
            ("/generalDiagnostics/0/file", json!("")),
            ("/generalDiagnostics/0/message", json!(" ")),
            ("/generalDiagnostics/0/range/end/line", json!(1)),
            ("/generalDiagnostics/0/range/start/character", json!(-1)),
            ("/summary/timeInSec", json!(-0.1)),
            ("/summary/filesAnalyzed", json!(1.5)),
            ("/version", json!("")),
        ] {
            let mut value = original.clone();
            *value.pointer_mut(pointer).unwrap() = replacement;
            malformed.push(value);
        }
        let mut missing = original.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("generalDiagnostics");
        malformed.push(missing);
        let mut conflicting = original.clone();
        conflicting["schema"] = json!("ee.response.v2");
        conflicting["success"] = json!(true);
        malformed.push(conflicting);
        for value in malformed {
            let text = value.to_string();
            assert!(inspect(&text).is_err(), "{text}");
            let output = ToolOutput {
                text,
                is_error: Some(false),
                exit_code: Some(0),
            };
            assert!(failure_diagnostics(&output).is_empty());
            assert!(!output.succeeded());
        }
    }

    #[test]
    fn pyright_rejects_duplicate_fields_and_incomplete_or_concatenated_documents() {
        let valid = pyright_report(Vec::new()).to_string();
        for text in [
            valid.replacen(
                "\"generalDiagnostics\":[]",
                "\"generalDiagnostics\":null,\"generalDiagnostics\":[]",
                1,
            ),
            valid.replacen("\"errorCount\":0", "\"errorCount\":1,\"errorCount\":0", 1),
            format!("{valid}\n{valid}"),
            format!("{valid} trailing text"),
            valid[..valid.len() - 1].to_owned(),
        ] {
            assert!(inspect(&text).is_err(), "{text}");
            assert!(
                !ToolOutput {
                    text,
                    is_error: Some(false),
                    exit_code: Some(0),
                }
                .succeeded()
            );
        }
        let nested =
            json!({"example": pyright_report(vec![pyright_diagnostic("error", "Invalid")])});
        assert!(inspect(&nested.to_string()).unwrap().is_none());
    }

    #[test]
    fn pyright_validates_every_record_after_output_cap_and_bounds_complete_input() {
        let mut records: Vec<_> = (0..8)
            .map(|number| {
                let mut value = pyright_diagnostic("error", "Invalid annotation");
                value["rule"] = json!(format!("reportCase{number}Type"));
                value
            })
            .collect();
        let valid = pyright_report(records.clone()).to_string();
        assert_eq!(
            parsed(&valid).diagnostics.len(),
            MAX_DIAGNOSTICS_PER_FAILURE
        );
        records.last_mut().unwrap()["severity"] = json!("unknown");
        assert!(inspect(&pyright_report(records).to_string()).is_err());
        let too_many = pyright_report(vec![
            pyright_diagnostic("error", "Invalid");
            MAX_COMPILER_RECORDS + 1
        ])
        .to_string();
        assert!(inspect(&too_many).is_err());
        assert!(inspect(&format!("{valid}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES))).is_err());
    }
}
