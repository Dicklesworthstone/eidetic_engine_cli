//! Bounded Rust compiler JSON diagnostics and Cargo JSONL completion checks.
//!
//! Cargo's compiler-message embeds a rustc diagnostic. Use its typed level,
//! code and message, never rendered output, children, artifacts or examples.
//! A build-finished success is not a process/test success: it only permits the
//! caller to consider its independent completion observation. Any error wins.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};

const MAX_COMPILER_RECORDS: usize = 256;

#[derive(Debug, Default)]
pub(super) struct CompilerOutput {
    pub diagnostics: Vec<CanonicalDiagnostic>,
    pub failed: bool,
    pub allows_completion: bool,
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
/// or mixed streams are errors, never a salvaged prefix of diagnostics. Typed
/// deserialization rejects duplicate discriminator, level, code and status
/// fields while tolerating unrelated future producer metadata.
pub(super) fn inspect(text: &str) -> Result<Option<CompilerOutput>, ()> {
    if !text.trim_start().starts_with('{') {
        return Ok(None);
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    // Identify the family before interpreting generic field names like code
    // or message. An unrelated producer may legitimately use other shapes.
    // Only the first record is probed; the selected compiler stream below is
    // then validated completely, including trailing records and separators.
    #[derive(Deserialize)]
    struct Family {
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
                    let code = diagnostic.code.as_ref().map(|code| code.code.as_str()).filter(
                        |code| {
                            code.len() == 5
                                && code.starts_with('E')
                                && code.as_bytes()[1..].iter().all(u8::is_ascii_digit)
                        },
                    );
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
    output.allows_completion = !output.failed && (!cargo || finished == Some(true));
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
            assert!(result.diagnostics[0].layered_key().key.starts_with("rustc:tmpl:blake3:"));
        }
        assert!(parsed(&diagnostic("unused_variables", "warning").to_string()).diagnostics.is_empty());
    }
}
