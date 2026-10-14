use super::*;
use super::super::{Proof, Requirement};
use crate::core::cass_error_recall::{
    ToolOutput, failure_diagnostics, structured_error_diagnostics,
};

fn verbose(results: &[(&str, &str)], counts: &str) -> String {
    let mut text = format!(
        "================ test session starts ================\ncollecting ... collected {} items\n\n",
        results.len()
    );
    for (node, status) in results {
        text.push_str(&format!("{node} {status} [100%]\n"));
    }
    text.push_str(&format!("\n================ {counts} in 0.01s ================\n"));
    text
}

fn parsed(text: &str) -> Decoded {
    decode(text)
        .expect("valid pytest report")
        .expect("pytest family")
}

fn output(text: String, code: i64) -> ToolOutput {
    ToolOutput {
        text,
        is_error: None,
        exit_code: Some(code),
    }
}

fn failed() -> String {
    verbose(&[("tests/test_queue.py::test_drain", "FAILED")], "1 failed")
}

fn passed() -> String {
    verbose(&[("tests/test_queue.py::test_drain", "PASSED")], "1 passed")
}

#[test]
fn native_failure_flows_through_import_and_public_diagnostic_decoder() {
    let text = failed();
    let decoded = parsed(&text);
    assert!(decoded.failed);
    assert!(decoded.execution.has_known_failures());
    assert!(!decoded.allows_completion);
    assert_eq!(decoded.diagnostics.len(), 1);
    assert!(
        decoded.diagnostics[0]
            .message_template
            .starts_with("pytest test failure ")
    );
    assert_eq!(failure_diagnostics(&output(text.clone(), 1)), decoded.diagnostics);
    assert_eq!(structured_error_diagnostics(&text), decoded.diagnostics);
    let optimistic = output(text, 0);
    assert!(optimistic.failed());
    assert!(!optimistic.succeeded());
}

#[test]
fn exact_completed_retry_verifies_the_failed_inventory() {
    let required = Requirement::capture(&output(failed(), 1), true);
    let proof = Proof::capture(&output(passed(), 0));
    assert!(required.is_guarded());
    assert!(required.is_verified_by(&proof));
    assert!(parsed(&passed()).allows_completion);
}

#[test]
fn pytest_footer_is_not_independent_process_completion() {
    let stdout = ToolOutput {
        text: passed(),
        is_error: None,
        exit_code: None,
    };
    assert!(!stdout.succeeded());
    assert!(
        ToolOutput {
            exit_code: Some(0),
            ..stdout
        }
        .succeeded()
    );
}

#[test]
fn omitted_renamed_or_other_file_tests_cannot_verify_a_repair() {
    let required = Requirement::capture(&output(failed(), 1), true);
    for node in ["tests/test_queue.py::test_other", "other/test_queue.py::test_drain"] {
        let proof = Proof::capture(&output(verbose(&[(node, "PASSED")], "1 passed"), 0));
        assert!(!required.is_verified_by(&proof), "{node}");
    }
}

#[test]
fn skipping_xfailing_or_xpassing_the_failed_subject_is_not_a_repair() {
    let nodes = [
        ("tests/test_queue.py::test_drain", "FAILED"),
        ("tests/test_queue.py::test_other", "PASSED"),
    ];
    let required = Requirement::capture(&output(verbose(&nodes, "1 failed, 1 passed"), 1), true);
    for (status, counter) in [("SKIPPED", "skipped"), ("XFAIL", "xfailed"), ("XPASS", "xpassed")] {
        let text = verbose(
            &[(nodes[0].0, status), (nodes[1].0, "PASSED")],
            &format!("1 passed, 1 {counter}"),
        );
        assert!(
            !required.is_verified_by(&Proof::capture(&output(text, 0))),
            "{status}"
        );
    }
}

#[test]
fn collection_and_progress_counts_must_reconcile_for_proof() {
    let unknown_inventory = failed().replace("collected 1 items", "collected 2 items");
    let decoded = parsed(&unknown_inventory);
    assert!(decoded.failed);
    assert!(!decoded.execution.has_known_failures());
    let required = Requirement::capture(&output(unknown_inventory, 1), true);
    assert!(!required.is_verified_by(&Proof::capture(&output(passed(), 0))));
}

#[test]
fn zero_tests_and_quiet_success_cannot_certify_executed_test_coverage() {
    for text in [
        verbose(&[], "0 passed"),
        verbose(&[], "4 deselected"),
        ". [100%]\n1 passed in 0.01s\n".to_owned(),
    ] {
        assert!(!parsed(&text).allows_completion);
        assert!(!output(text, 0).succeeded());
    }
}

#[test]
fn partial_output_and_failfast_retain_failures_without_repair_proof() {
    let partial = failed()
        .split("================ 1 failed")
        .next()
        .unwrap()
        .to_owned();
    assert!(parsed(&partial).failed);
    assert!(!parsed(&partial).allows_completion);
    assert!(!parsed(&partial).execution.has_known_failures());
    let failfast = failed().replace(
        "================ 1 failed",
        "!!!!!!!! stopping after 1 failures !!!!!!!!\n================ 1 failed",
    );
    assert!(!parsed(&failfast).execution.has_known_failures());
    assert_eq!(parsed(&partial).diagnostics, parsed(&failed()).diagnostics);
}

#[test]
fn quiet_summary_and_verbose_progress_share_one_persistent_class() {
    let quiet = "F [100%]\n================ short test summary info ================\nFAILED tests/test_queue.py::test_drain - AssertionError: queue retained stale state\n1 failed in 0.01s\n";
    let decoded = parsed(quiet);
    assert!(decoded.failed);
    assert!(!decoded.execution.has_known_failures());
    assert_eq!(decoded.diagnostics, parsed(&failed()).diagnostics);
    assert!(
        decoded
            .symptom
            .as_deref()
            .unwrap()
            .contains("queue retained stale state")
    );
}

#[test]
fn parameter_names_with_spaces_unicode_and_same_named_tests_stay_distinct() {
    let nodes = [
        ("tests/test_queue.py::test_drain[with space]", "FAILED"),
        ("tests/test_queue.py::test_drain[lambda_λ]", "FAILED"),
        ("other/test_queue.py::test_drain[with space]", "FAILED"),
    ];
    let decoded = parsed(&verbose(&nodes, "3 failed"));
    assert_eq!(decoded.execution.inventory.len(), 3);
    assert_eq!(decoded.diagnostics.len(), 3);
    let keys: BTreeSet<_> = decoded
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.layered_key().key)
        .collect();
    assert_eq!(keys.len(), 3);
}

#[test]
fn setup_and_teardown_errors_require_the_failed_test_to_run_cleanly() {
    let node = "tests/test_queue.py::test_drain";
    let setup = verbose(&[(node, "ERROR")], "1 error");
    let teardown = format!(
        "================ test session starts ================\ncollected 1 item\n{node} PASSED [100%]\n{node} ERROR [100%]\n================ 1 passed, 1 error in 0.01s ================\n"
    );
    for text in [setup, teardown] {
        let required = Requirement::capture(&output(text, 1), true);
        assert!(required.is_verified_by(&Proof::capture(&output(passed(), 0))));
    }
}

#[test]
fn collection_errors_can_be_recalled_but_do_not_invent_an_executed_inventory() {
    let text = "================ test session starts ================\ncollected 0 items / 1 error\n================ short test summary info ================\nERROR tests/test_queue.py - ImportError: missing queue module\n!!!!!!!! Interrupted: 1 error during collection !!!!!!!!\n================ 1 error in 0.01s ================\n";
    let decoded = parsed(text);
    assert!(decoded.failed);
    assert_eq!(decoded.diagnostics.len(), 1);
    assert!(!decoded.execution.has_known_failures());
}

#[test]
fn contradictory_or_duplicate_terminal_reports_never_certify_success() {
    for text in [
        failed().replace("1 failed in", "1 passed in"),
        format!("{}{}", passed(), passed()),
        format!("{}\n================ 1 passed in 0.02s ================", passed()),
        passed().replace("1 passed in", "1 passed, 1 passed in"),
    ] {
        assert!(!output(text, 0).succeeded());
    }
}

#[test]
fn captured_output_is_not_test_progress_or_diagnostic_text() {
    let text = failed().replace(
        "\n================ 1 failed",
        "\n================ FAILURES ================\nPRIVATE_CAPTURE_SENTINEL\ntests/test_queue.py::test_drain PASSED [100%]\n================ 1 failed",
    );
    let decoded = parsed(&text);
    assert_eq!(decoded.diagnostics, parsed(&failed()).diagnostics);
    assert!(
        !decoded.diagnostics[0]
            .message_template
            .contains("private_capture_sentinel")
    );
    assert!(!decoded.allows_completion);
}

#[test]
fn all_failed_subjects_are_guarded_beyond_the_four_diagnostic_display_cap() {
    let names: Vec<_> = (0..6)
        .map(|n| format!("tests/test_queue.py::test_{n}"))
        .collect();
    let failures: Vec<_> = names.iter().map(|name| (name.as_str(), "FAILED")).collect();
    let text = verbose(&failures, "6 failed");
    assert_eq!(parsed(&text).diagnostics.len(), MAX_DIAGNOSTICS_PER_FAILURE);
    let required = Requirement::capture(&output(text, 1), true);
    let mut repaired: Vec<_> = names.iter().map(|name| (name.as_str(), "PASSED")).collect();
    repaired[5].1 = "SKIPPED";
    let skipped = Proof::capture(&output(verbose(&repaired, "5 passed, 1 skipped"), 0));
    assert!(!required.is_verified_by(&skipped));
    repaired[5].1 = "PASSED";
    let green = Proof::capture(&output(verbose(&repaired, "6 passed"), 0));
    assert!(required.is_verified_by(&green));
}

#[test]
fn duplicate_progress_and_repeated_unknown_plugin_attempts_are_not_proof() {
    let duplicate = passed().replace(
        "================ 1 passed",
        "tests/test_queue.py::test_drain PASSED [100%]\n================ 2 passed",
    );
    assert!(!parsed(&duplicate).allows_completion);
}

#[test]
fn native_guard_survives_loss_of_typed_output_and_can_finish_on_a_later_poll() {
    let mut required = Requirement::capture(&output(failed(), 1), false);
    let proof = Proof::capture(&output(passed(), 0));
    assert!(!required.is_verified_by(&proof));
    required.refresh(&output(failed(), 1), true);
    assert!(required.is_verified_by(&proof));
    required.refresh(&output("ordinary unrelated output".to_owned(), 0), true);
    assert!(required.is_guarded());
    assert!(!required.is_verified_by(&proof));
}

#[test]
fn malformed_trailing_data_control_codes_and_bounds_never_certify_success() {
    for text in [
        format!("{}unrecognized trailer\n", passed()),
        passed().replace("0.01s", "NaNs"),
        passed().replace("0.01s", "0.01s unexpected"),
        passed().replace("PASSED", "\u{1b}[32mPASSED\u{1b}[0m"),
        format!("{}{}", passed(), " ".repeat(MAX_SCANNED_OUTPUT_BYTES)),
    ] {
        assert!(!output(text, 0).succeeded());
    }
}

#[test]
fn native_long_duration_and_deselected_collection_are_supported() {
    let text = passed()
        .replace("collected 1 items", "collected 4 items / 3 deselected / 1 selected")
        .replace("1 passed in 0.01s", "1 passed, 3 deselected in 64.01s (0:01:04)");
    assert!(parsed(&text).allows_completion);
}

#[test]
fn source_secrets_are_redacted_before_persistent_fingerprints_and_symptoms() {
    let secret = "sk-proj-ABCDEF1234567890ABCDEF1234567890";
    let node = format!("tests/test_queue.py::test_drain[api_key={secret}]");
    let text = format!("F [100%]\n================ short test summary info ================\nFAILED {node} - RuntimeError: api_key={secret}\n1 failed in 0.01s\n");
    let decoded = parsed(&text);
    assert!(!format!("{:?}", decoded.diagnostics).contains(secret));
    assert!(!decoded.symptom.unwrap().contains(secret));
}

#[test]
fn unrelated_text_json_and_existing_rust_failures_keep_their_interpretation() {
    for text in [
        "ordinary stdout",
        "example: pytest failed",
        "{\"example\":\"F [100%]\\n1 failed in 0.01s\"}",
    ] {
        assert!(decode(text).expect("unrelated producer").is_none());
    }
    let rust = output("error[E0308]: mismatched types".to_owned(), 1);
    assert_eq!(failure_diagnostics(&rust)[0].layered_key().key, "rustc:E0308");
}
