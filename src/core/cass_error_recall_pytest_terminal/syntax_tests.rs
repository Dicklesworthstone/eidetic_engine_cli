use super::*;
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};
use crate::core::cass_error_recall::{
    ToolOutput, failure_diagnostics, structured_error_diagnostics,
};

fn report(summary: &str) -> String {
    format!(
        "  File \"/private/project/queue.py\", line 7\n    private_source_sentinel(:\n                            ^\n{summary}\n"
    )
}

fn optimistic(text: &str) -> ToolOutput {
    ToolOutput {
        text: text.to_owned(),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

#[test]
fn native_parser_errors_reach_public_and_import_diagnostics_without_a_traceback() {
    for summary in [
        "SyntaxError: invalid syntax",
        "IndentationError: expected an indented block after 'if' statement on line 1",
        "TabError: inconsistent use of tabs and spaces in indentation",
    ] {
        let text = report(summary);
        let decoded = inspect(&text).expect("standalone CPython parser failure");
        assert_eq!(decoded.diagnostics, vec![diagnostic(summary)]);
        assert!(decoded.failed && !decoded.allows_completion);
        assert!(decoded.execution.is_command_failure());
        assert_eq!(structured_error_diagnostics(&text), decoded.diagnostics);
        for exit_code in [None, Some(0), Some(1)] {
            let output = ToolOutput {
                exit_code,
                ..optimistic(&text)
            };
            assert_eq!(failure_diagnostics(&output), decoded.diagnostics);
            assert!(output.failed());
            assert!(!output.succeeded());
            assert_eq!(output.text, text);
        }
    }
}

#[test]
fn source_and_caret_are_optional_but_never_fingerprint_payloads() {
    let expected = diagnostic("SyntaxError: invalid syntax");
    for body in ["", "    print(\n", "    print(\n         ^\n"] {
        let text = format!("  File \"<string>\", line 1\n{body}SyntaxError: invalid syntax\n");
        assert_eq!(inspect(&text).unwrap().diagnostics, vec![expected.clone()]);
    }
    let text = report("SyntaxError: invalid syntax").replace(
        "private_source_sentinel(:",
        "SyntaxError: fake_secret error[E0308]: printed code",
    );
    let decoded = inspect(&text).unwrap();
    assert_eq!(decoded.diagnostics, vec![expected]);
    let projection = format!("{:?} {:?}", decoded.diagnostics, decoded.symptom);
    for private in [
        "/private/project",
        "private_source_sentinel",
        "fake_secret",
        "E0308",
    ] {
        assert!(!projection.contains(private));
    }
}

#[test]
fn imported_syntax_errors_and_runtime_syntax_exceptions_share_identity() {
    let summary = "SyntaxError: invalid syntax";
    let standalone = report(summary);
    let runtime = format!(
        "{}\n  File \"other.py\", line 80, in <module>\n    compile(bad_source)\n{summary}\n",
        super::super::HEADER,
    );
    assert_eq!(
        structured_error_diagnostics(&standalone),
        structured_error_diagnostics(&runtime)
    );
    // A quoted traceback header in displayed source must not steal dispatch.
    let source = standalone.replace("private_source_sentinel(:", super::super::HEADER);
    assert_eq!(
        structured_error_diagnostics(&source),
        vec![diagnostic(summary)]
    );
}

#[test]
fn source_locations_styles_and_line_endings_do_not_change_error_identity() {
    let plain = report("SyntaxError: invalid syntax");
    let expected = structured_error_diagnostics(&plain);
    for text in [
        plain
            .replace("/private/project/queue.py", "λ/路径.py")
            .replace("line 7", "line 991"),
        plain.replace('\n', "\r\n"),
        format!("\n\n{plain}\n"),
        format!("\u{1b}[31m{plain}\u{1b}[0m"),
        plain.replace("SyntaxError", "\u{1b}[1mSyntaxError\u{1b}[0m"),
    ] {
        assert_eq!(structured_error_diagnostics(&text), expected, "{text:?}");
        assert!(!optimistic(&text).succeeded());
    }
}

#[test]
fn malformed_reports_never_leave_a_plausible_prefix_diagnostic() {
    let good = report("SyntaxError: invalid syntax");
    for text in [
        good.replace("line 7", "line 0"),
        good.replace("line 7", "line -1"),
        good.replace("line 7", "line 18446744073709551616"),
        good.replace("line 7", "line 7, in function"),
        good.replace("/private/project/queue.py", ""),
        good.replace("                            ^", "    invalid caret row"),
        good.replace("SyntaxError", "RuntimeError"),
        good.replace("SyntaxError: invalid syntax", "SyntaxError: "),
        good.replace("SyntaxError: invalid syntax", "OK"),
        format!("{good}private note\n"),
        format!("{good}Process exited with code 0\n"),
        format!("{good}{good}"),
        good.replace("SyntaxError", "\u{1b}[2JSyntaxError"),
        good.replace("SyntaxError", "\u{8}SyntaxError"),
    ] {
        let decoded = inspect(&text).expect("claimed parser report");
        assert!(decoded.failed && !decoded.allows_completion);
        assert!(decoded.diagnostics.is_empty(), "{text:?}");
        assert!(decoded.symptom.is_none());
        assert!(failure_diagnostics(&optimistic(&text)).is_empty());
        assert!(!optimistic(&text).succeeded());
    }
}

#[test]
fn incomplete_summaries_and_over_budget_reports_never_become_diagnostics() {
    let good = report("SyntaxError: invalid syntax");
    for (end, _) in good.char_indices() {
        if let Some(decoded) = inspect(&good[..end]) {
            assert!(decoded.diagnostics.is_empty(), "prefix {end}");
            assert!(!decoded.allows_completion);
        }
    }
    for text in [
        format!("{good}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES)),
        format!("{good}{}", "\n".repeat(MAX_LINES)),
        format!("  File \"{}", "λ".repeat(MAX_SCANNED_OUTPUT_BYTES)),
    ] {
        let decoded = inspect(&text).expect("bounded claimed report");
        assert!(decoded.diagnostics.is_empty());
        assert!(!optimistic(&text).succeeded());
    }
}

#[test]
fn bare_messages_quoted_examples_and_non_python_frames_are_not_parser_reports() {
    let native = report("SyntaxError: invalid syntax");
    for text in [
        "SyntaxError: an example\n".to_owned(),
        "ordinary stdout\n".to_owned(),
        format!("example follows:\n{native}"),
        format!("    {native}"),
        serde_json::json!({"example":native}).to_string(),
        "File \"code.py\", line 1\nSyntaxError: invalid syntax\n".to_owned(),
    ] {
        assert!(inspect(&text).is_none(), "{text:?}");
    }
}

#[test]
fn syntax_diagnostics_redact_summary_secrets_before_generating_keys() {
    let secret = format!("ghp_{}", "Q".repeat(36));
    let summary = format!("SyntaxError: api_key={secret}");
    let safe = crate::policy::redact_secret_like_content(&summary).content;
    let decoded = inspect(&report(&summary)).unwrap();
    assert_eq!(
        decoded.diagnostics,
        inspect(&report(&safe)).unwrap().diagnostics
    );
    let projected = format!("{:?} {:?}", decoded.diagnostics, decoded.symptom);
    assert!(!projected.contains(&secret));
    assert!(!projected.contains(&secret.to_ascii_lowercase()));
}

#[test]
fn a_parser_error_cannot_downgrade_an_existing_executed_test_requirement() {
    let syntax = optimistic(&report("SyntaxError: invalid syntax"));
    assert!(!Requirement::capture(&syntax, true).is_guarded());
    let tests = optimistic(
        "=== test session starts ===\ncollected 1 item\ntests/test_queue.py::test_drain FAILED [100%]\n=== 1 failed in 0.01s ===\n",
    );
    let mut required = Requirement::capture(&tests, true);
    assert!(required.is_guarded());
    required.refresh(&syntax, true);
    assert!(required.is_guarded());
    let green = optimistic("queue ready\n");
    assert!(green.succeeded());
    assert!(!required.is_verified_by(&Proof::capture(&green)));
}
