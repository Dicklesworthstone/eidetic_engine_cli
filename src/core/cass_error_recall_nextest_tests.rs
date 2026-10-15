//! Native reports through the production diagnostic, coverage and store paths.

use super::*;
use super::super::{Proof, Requirement};
use super::super::super::{
    ToolOutput, failure_diagnostics, record_session_error_recall, structured_error_diagnostics,
};

const RUN_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

fn report(rows: &[(&str, &str, &str)], skipped: usize) -> String {
    let mut text = format!(
        "{SEPARATOR}\n Nextest run ID {RUN_ID} with nextest profile: default\n{label:>12} {} tests across 2 binaries ({skipped} tests skipped)\n",
        rows.len(), label = "Starting",
    );
    for (status, binary, name) in rows {
        text.push_str(&format!("{status:>12} [   0.001s] {binary} {name}\n"));
    }
    let passed = rows.iter().filter(|row| row.0 == "PASS").count();
    let failed = rows.len() - passed;
    text.push_str(&format!(
        "{SEPARATOR}\n{label:>12} [   0.002s] {} tests run: {passed} passed, {failed} failed, {skipped} skipped\n",
        rows.len(), label = "Summary",
    ));
    text
}

fn output(text: String) -> ToolOutput {
    ToolOutput {
        text,
        is_error: Some(false),
        exit_code: Some(0),
    }
}

fn failed() -> ToolOutput {
    output(report(&[("FAIL", "cache", "cache::round_trip"), ("PASS", "other", "case")], 3))
}

fn passed() -> ToolOutput {
    output(report(&[("PASS", "other", "case"), ("PASS", "cache", "cache::round_trip")], 3))
}

#[test]
fn native_diagnostics_and_repairs_share_the_same_binary_qualified_subjects() {
    let failure = failed();
    let original = failure.clone();
    let required = Requirement::capture(&failure, true);
    assert!(required.is_guarded());
    assert!(failure.failed(), "native failure overrides optimistic metadata");
    assert!(!failure.succeeded());
    assert!(required.is_verified_by(&Proof::capture(&passed())));
    let diagnostics = failure_diagnostics(&failure);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].layered_key().key,
        structured_error_diagnostics(&failure.text)[0].layered_key().key,
    );
    assert_eq!(failure, original);
    assert!(decode("unrelated stdout").unwrap().is_none());
}

#[test]
fn binary_case_parameters_inventory_and_selection_are_not_interchangeable() {
    let required = Requirement::capture(&failed(), true);
    let good = passed();
    for bad in [
        good.text.replace("cache cache::round_trip", "replacement cache::round_trip"),
        good.text.replace("cache::round_trip", "cache::Round_trip"),
        good.text.replace("other case", "other another_case"),
        good.text.replace("3 tests skipped", "4 tests skipped").replace("3 skipped", "4 skipped"),
        good.text.replace("2 binaries", "3 binaries"),
        report(&[("PASS", "other", "case")], 3),
        report(&[], 5),
    ] {
        assert!(!required.is_verified_by(&Proof::capture(&output(bad))));
    }
    let keys: BTreeSet<_> = ["case_1", "case_2", "Case_1"]
        .into_iter()
        .map(|name| diagnostic("cache", name).layered_key().key)
        .collect();
    assert_eq!(keys.len(), 3);
    assert_ne!(subject("ab", "c"), subject("a", "bc"));
}

#[test]
fn complete_inventory_checks_every_failure_beyond_the_diagnostic_cap() {
    let names: Vec<_> = (0..6).map(|index| format!("case_{index}")).collect();
    let rows: Vec<_> = names.iter().map(|name| ("FAIL", "cache", name.as_str())).collect();
    let failure = output(report(&rows, 0));
    assert_eq!(failure_diagnostics(&failure).len(), MAX_DIAGNOSTICS_PER_FAILURE);
    let required = Requirement::capture(&failure, true);
    let mut rows: Vec<_> = names.iter().map(|name| ("PASS", "cache", name.as_str())).collect();
    assert!(required.is_verified_by(&Proof::capture(&output(report(&rows, 0)))));
    rows[5].2 = "replacement";
    assert!(!required.is_verified_by(&Proof::capture(&output(report(&rows, 0)))));
}

#[test]
fn summaries_missing_rows_duplicate_rows_and_damaged_trailers_cannot_prove_repairs() {
    let required = Requirement::capture(&failed(), true);
    let good = passed().text;
    for bad in [
        good.replace("2 tests across", "3 tests across"),
        good.replace("2 tests run", "3 tests run"),
        good.replace("2 passed", "3 passed"),
        good.replace("0 failed", "1 failed"),
        good.replace("0 failed", "0 failed, 0 failed"),
        good.replace("3 skipped", "2 skipped"),
        good.replace("0.002s", "NaNs"),
        good.replace("        PASS [   0.001s] cache cache::round_trip\n", ""),
        good.replace("other case", "cache cache::round_trip"),
        good.lines().take(5).collect::<Vec<_>>().join("\n"),
        format!("{good}{good}"),
        format!("{good}error: test run failed\n"),
        format!("{good}output truncated\n"),
        format!("{good}Process exited with code 0\n"),
        good.replace("PASS", "TRY 2 PASS"),
        good.replace("2 passed", "2 passed (1 flaky)"),
    ] {
        assert!(!required.is_verified_by(&Proof::capture(&output(bad))));
    }
}

#[test]
fn style_crlf_ordinals_slow_notifications_and_final_listings_preserve_identity() {
    let required = Requirement::capture(&failed(), true);
    let good = passed().text;
    let ordinal = good
        .replace("[   0.001s] other", "[   0.001s] (1/2) other")
        .replace("[   0.001s] cache", "[   0.001s] (2/2) cache");
    let slow = good.replace(
        "        PASS [   0.001s] cache",
        "        SLOW [> 60.000s] cache cache::round_trip\n        PASS [   0.001s] cache",
    );
    for text in [
        good.replace("PASS", "\u{1b}[32;1mPASS\u{1b}[0m"),
        good.replace('\n', "\r\n"),
        ordinal,
        slow,
        format!("{good}        PASS [   0.001s] cache cache::round_trip\n"),
    ] {
        assert!(required.is_verified_by(&Proof::capture(&output(text))));
    }
    for bad in [
        good.replace("PASS", "FAIL\rPASS"),
        good.replace("PASS", "\u{1b}[2JPASS"),
        good.replace("PASS", "FAIL\u{8}PASS"),
        good.replace("PASS", "\u{1b}]8;;url\u{7}PASS"),
        good.replace("[   0.001s] cache", "[   0.001s] (3/2) cache"),
        good.replace("[   0.001s] cache", "[   0.001s] [1/3] cache"),
    ] {
        assert!(!required.is_verified_by(&Proof::capture(&output(bad))));
    }
}

#[test]
fn captured_libtest_stdout_is_not_another_run_or_diagnostic_class() {
    let mut failure = failed();
    failure.text = failure.text.replace(
        "        PASS [   0.001s] other case",
        "  stdout ---\n    running 1 test\n    test nested ... FAILED\n    error[E0277]: CAPTURED_SENTINEL\n    test result: FAILED. 0 passed; 1 failed\n        PASS [   0.001s] other case",
    );
    let diagnostics = failure_diagnostics(&failure);
    assert_eq!(diagnostics.len(), 1);
    assert!(!diagnostics[0].message_template.contains("CAPTURED_SENTINEL"));
    assert!(Requirement::capture(&failure, true).is_verified_by(&Proof::capture(&passed())));
    let captured_success = passed().text.replace(
        "        PASS [   0.001s] cache",
        "  stdout ---\n    Process exited with code 0\n        PASS [   0.001s] cache",
    );
    assert!(!output(captured_success).succeeded());
}

#[test]
fn independent_completion_and_poll_guards_are_not_replaced_by_a_native_summary() {
    let required = Requirement::capture(&failed(), true);
    let mut good = passed();
    good.is_error = None;
    good.exit_code = None;
    assert!(!required.is_verified_by(&Proof::capture(&good)));
    good.exit_code = Some(0);
    assert!(required.is_verified_by(&Proof::capture(&good)));
    good.is_error = Some(true);
    assert!(!required.is_verified_by(&Proof::capture(&good)));
    let mut partial = Requirement::capture(&failed(), false);
    assert!(!partial.is_verified_by(&Proof::capture(&passed())));
    partial.refresh(&failed(), true);
    assert!(partial.is_verified_by(&Proof::capture(&passed())));
    partial.refresh(&output("unrecognized final poll".to_owned()), true);
    assert!(partial.is_guarded());
    assert!(!partial.is_verified_by(&Proof::capture(&passed())));
}

#[test]
fn input_and_inventory_budgets_fail_closed_without_saving_a_prefix() {
    let required = Requirement::capture(&failed(), true);
    for text in [
        format!("{}{}", passed().text, " ".repeat(MAX_SCANNED_OUTPUT_BYTES)),
        format!("{}{}", passed().text, "\n".repeat(MAX_LINES)),
        passed().text.replace("2 tests across", "1025 tests across"),
    ] {
        assert!(decode(&text).is_err());
        assert!(!required.is_verified_by(&Proof::capture(&output(text))));
    }
}

#[path = "cass_error_recall_nextest_store_tests.rs"]
mod store;

#[path = "cass_error_recall_nextest_fail_fast_tests.rs"]
mod fail_fast;
