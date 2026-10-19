//! Native TypeScript compiler diagnostics, without a compiler subprocess.
//!
//! `tsc --pretty false` emits located or global `error TS<code>` records.
//! A message chain is indented beneath its diagnostic, not another error.
//! Validate the complete report before returning any fingerprints. Paths,
//! positions and quoted type/identifier values are never part of the key.
//! This decoder grants no completion authority: only the existing completed
//! invocation/context check may connect a failed run to its later repair.

use std::collections::BTreeSet;

use super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};
use crate::core::error_recall::{DiagnosticTool, canonical_message_template};

const MAX_RECORDS: usize = 256;

struct Header<'a> {
    code: &'a str,
    message: &'a str,
}

fn positive_decimal(text: &str, max_digits: usize) -> bool {
    !text.is_empty()
        && text.len() <= max_digits
        && !text.starts_with('0')
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && text.parse::<u32>().is_ok_and(|value| value > 0)
}

fn source_path(path: &str) -> bool {
    !path.is_empty()
        && path.trim() == path
        && !path.chars().any(char::is_control)
        && !path.contains(['\'', '"', '`', '<', '>'])
        && [
            ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".json",
        ]
        .iter()
        .any(|extension| path.ends_with(*extension))
}

/// Probe only the current top-level record, never a diagnostic-looking string
/// nested inside JSON, a code fence, a message chain or explanatory prose.
fn header(line: &str) -> Result<Option<Header<'_>>, ()> {
    if line.starts_with(char::is_whitespace) {
        return Ok(None);
    }
    let diagnostic = if line.starts_with("error TS") {
        line
    } else {
        let Some((location, diagnostic)) = line.split_once("): ") else {
            return Ok(None);
        };
        let Some((path, position)) = location.rsplit_once('(') else {
            return Ok(None);
        };
        if !source_path(path) || !diagnostic.starts_with("error TS") {
            return Ok(None);
        }
        let (row, column) = position.split_once(',').ok_or(())?;
        if !positive_decimal(row, 10) || !positive_decimal(column, 10) {
            return Err(());
        }
        diagnostic
    };
    let (code, message) = diagnostic
        .strip_prefix("error TS")
        .and_then(|body| body.split_once(": "))
        .ok_or(())?;
    if !positive_decimal(code, 6) || message.trim().is_empty() || line.chars().any(char::is_control)
    {
        return Err(());
    }
    Ok(Some(Header { code, message }))
}

/// TS diagnostic arguments use single/double quotes as well as backticks.
/// Mask those values before the shared template canonicalizer. An unmatched
/// quote withholds its entire suffix rather than exposing a partial value.
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

fn canonical(header: &Header<'_>) -> CanonicalDiagnostic {
    let message = crate::policy::redact_secret_like_content(header.message).content;
    CanonicalDiagnostic {
        // Reuse the existing storage/tool vocabulary, but keep native codes
        // disjoint from shell exit codes and other compilers' diagnostics.
        tool: DiagnosticTool::Shell,
        canonical_code: Some(format!("tsc:TS{}", header.code)),
        message_template: canonical_message_template(&format!(
            "TypeScript: {}",
            mask_literals(&message),
        )),
    }
}

/// `None` is another producer, not successful TypeScript output. Every `Some`
/// contains at least one real error. Malformed claimed reports are refused as
/// a whole, including errors after the presentation/diagnostic cap.
pub(super) fn decode(text: &str) -> Result<Option<Vec<CanonicalDiagnostic>>, ()> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = lines.next() else {
        return Ok(None);
    };
    let Some(first) = header(first)? else {
        return Ok(None);
    };
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    let mut keys = BTreeSet::from([first.code]);
    let mut diagnostics = vec![canonical(&first)];
    let mut records = 1;
    for line in lines {
        if let Some(next) = header(line)? {
            records += 1;
            if records > MAX_RECORDS {
                return Err(());
            }
            if keys.insert(next.code) && diagnostics.len() < MAX_DIAGNOSTICS_PER_FAILURE {
                diagnostics.push(canonical(&next));
            }
        } else if !line.starts_with("  ") || line.chars().any(char::is_control) {
            // Plain TypeScript only adds indented message-chain text. A shell
            // trailer, mixed producer or truncated next record is not that.
            return Err(());
        }
    }
    Ok(Some(diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics, symptom_line,
    };

    const ERROR: &str =
        "src/widget.ts(7,9): error TS2322: Type 'string' is not assignable to type 'number'.";

    fn optimistic(text: &str) -> ToolOutput {
        ToolOutput {
            text: text.to_owned(),
            is_error: Some(false),
            exit_code: Some(0),
        }
    }

    #[test]
    fn native_records_reach_import_interactive_diagnosis_and_card_symptoms() {
        let output = optimistic(ERROR);
        let imported = failure_diagnostics(&output);
        assert_eq!(imported.len(), 1);
        assert_eq!(imported, structured_error_diagnostics(ERROR));
        assert_eq!(imported[0].layered_key().key, "shell:tsc:TS2322");
        assert_eq!(
            imported[0].message_template,
            "typescript: type <id> is not assignable to type <id>."
        );
        let symptom = symptom_line(&output).expect("native symptom");
        assert!(symptom.contains("tsc:TS2322"));
        assert!(!symptom.contains("src/widget.ts"));
        assert!(output.failed());
        assert!(!output.succeeded());
    }

    #[test]
    fn global_and_cross_platform_locations_keep_the_same_native_key() {
        for location in [
            "",
            "src/widget.ts(7,9): ",
            "/private/another project/(generated)/widget.tsx(200,10): ",
            "C:\\private\\widget.mts(3,1): ",
            "app/[id]/page.cts(1,1): ",
            "tsconfig.json(1,2): ",
        ] {
            let text = format!("{location}error TS2322: Changed diagnostic argument.");
            let diagnostics = structured_error_diagnostics(&text);
            assert_eq!(diagnostics.len(), 1, "{location}");
            assert_eq!(diagnostics[0].layered_key().key, "shell:tsc:TS2322");
        }
    }

    #[test]
    fn message_chains_and_duplicate_codes_are_not_extra_error_classes() {
        let text = format!(
            "{ERROR}\n  Type 'Nested' is missing a required field.\n  error TS9999: quoted example\n{ERROR}\nerror TS18003: No inputs were found in config file 'tsconfig.json'.\n"
        );
        let diagnostics = structured_error_diagnostics(&text);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[1].layered_key().key, "shell:tsc:TS18003");
        assert!(
            diagnostics
                .iter()
                .all(|item| !item.message_template.contains("Nested"))
        );
        assert_eq!(
            structured_error_diagnostics(&text.replace('\n', "\r\n")),
            diagnostics
        );
    }

    #[test]
    fn quoted_paths_identifiers_and_values_do_not_fragment_native_errors() {
        let before = "error TS2307: Cannot find module '/private/first/module' or its corresponding type declarations.";
        let after = "error TS2307: Cannot find module 'C:\\other\\module' or its corresponding type declarations.";
        assert_eq!(
            structured_error_diagnostics(before),
            structured_error_diagnostics(after)
        );
        assert_eq!(
            mask_literals("'secret' and \"other\" and `name`"),
            "<id> and <id> and <id>"
        );
        assert_eq!(mask_literals("'unfinished private value"), "<id>");
    }

    #[test]
    fn quiet_stdout_does_not_supply_process_completion() {
        assert!(decode("").expect("quiet").is_none());
        assert!(!ToolOutput::default().succeeded());
        assert!(optimistic("").succeeded());
        assert!(
            !ToolOutput {
                text: ERROR.to_owned(),
                ..ToolOutput::default()
            }
            .succeeded()
        );
    }

    #[test]
    fn json_fences_and_explanations_do_not_become_native_compiler_reports() {
        for text in [
            format!("Example diagnostic:\n{ERROR}"),
            format!("```text\n{ERROR}\n```"),
            format!("  {ERROR}"),
            serde_json::json!({"example": ERROR}).to_string(),
            serde_json::json!([ERROR]).to_string(),
            format!("$ echo '{ERROR}'"),
        ] {
            assert!(decode(&text).expect("not TypeScript").is_none(), "{text}");
            assert!(structured_error_diagnostics(&text).is_empty(), "{text}");
        }
    }

    #[test]
    fn malformed_codes_positions_and_messages_refuse_the_entire_report() {
        for invalid in [
            "error TS: Missing code.",
            "error TS02322: Leading zero.",
            "error TS2322",
            "error TS2322: ",
            "error TS9999999: Too many digits.",
            "error TS２３２２: Non-ASCII code.",
            "src/a.ts(0,1): error TS2322: Bad position.",
            "src/a.ts(1,-1): error TS2322: Bad position.",
            "src/a.ts(1,1,2): error TS2322: Bad position.",
            "src/a.ts(4294967296,1): error TS2322: Overflow.",
        ] {
            for text in [invalid.to_owned(), format!("{ERROR}\n{invalid}")] {
                assert!(decode(&text).is_err(), "{text}");
                assert!(failure_diagnostics(&optimistic(&text)).is_empty());
                assert!(structured_error_diagnostics(&text).is_empty());
                assert!(!optimistic(&text).succeeded());
            }
        }
    }

    #[test]
    fn mixed_or_truncated_trailers_cannot_salvage_a_valid_prefix() {
        for tail in [
            "Exit code 0",
            "Process exited with code 0",
            "Found 0 errors.",
            "{\"success\":true}",
            "src/other.ts(1,1): error TS",
            "another producer succeeded",
            "\u{1b}[0m",
        ] {
            let text = format!("{ERROR}\n{tail}");
            assert!(decode(&text).is_err(), "{tail}");
            assert!(structured_error_diagnostics(&text).is_empty());
            assert!(!optimistic(&text).succeeded());
        }
    }

    #[test]
    fn every_record_is_validated_after_the_diagnostic_cap() {
        let text = (1000..1010)
            .map(|code| format!("error TS{code}: Compiler diagnostic.\n"))
            .collect::<String>();
        assert_eq!(
            structured_error_diagnostics(&text).len(),
            MAX_DIAGNOSTICS_PER_FAILURE
        );
        let invalid = format!("{text}error TSx: malformed final diagnostic\n");
        assert!(structured_error_diagnostics(&invalid).is_empty());
        assert!(!optimistic(&invalid).succeeded());
    }

    #[test]
    fn record_and_byte_limits_refuse_without_panics_or_partial_results() {
        let exact = format!("{ERROR}\n").repeat(MAX_RECORDS);
        assert_eq!(structured_error_diagnostics(&exact).len(), 1);
        let too_many = format!("{exact}{ERROR}\n");
        assert!(decode(&too_many).is_err());
        let too_large = format!("{ERROR}\n  {}", "界".repeat(MAX_SCANNED_OUTPUT_BYTES));
        assert!(decode(&too_large).is_err());
        assert!(!optimistic(&too_large).succeeded());
    }

    #[test]
    fn function_signatures_in_messages_do_not_replace_the_source_location() {
        let text =
            "src/handler.ts(4,2): error TS2322: Type '(x: string): number' is not assignable.";
        let diagnostics = structured_error_diagnostics(text);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].layered_key().key, "shell:tsc:TS2322");
    }

    #[test]
    fn secret_values_are_redacted_before_template_construction() {
        let secret = format!("ghp_{}", "Q".repeat(36));
        let text = format!("error TS2345: Invalid argument api_key={secret}");
        let diagnostics = structured_error_diagnostics(&text);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            !format!("{diagnostics:?}")
                .to_ascii_lowercase()
                .contains(&secret.to_ascii_lowercase())
        );
    }

    #[test]
    fn existing_rust_and_python_envelopes_do_not_enter_the_typescript_decoder() {
        for text in [
            "error[E0277]: a Rust compiler failure",
            "Traceback (most recent call last):\n  File \"x.py\", line 1\nValueError: bad",
            "test result: FAILED. 0 passed; 1 failed; 0 ignored",
        ] {
            assert!(decode(text).expect("different producer").is_none());
        }
        assert_eq!(
            failure_diagnostics(&optimistic("error[E0277]: missing trait"))[0]
                .layered_key()
                .key,
            "rustc:E0277"
        );
    }
}
