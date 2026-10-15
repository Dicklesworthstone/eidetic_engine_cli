//! Fail-fast progress and producer-native presentation are proof inputs.

use super::*;

pub(super) fn failure_report() -> String {
    format!(
        "{SEPARATOR}\n Nextest run ID {RUN_ID} with nextest profile: default\n    Starting 3 tests across 2 binaries (3 tests skipped)\n        FAIL [   0.001s] cache cache::round_trip\n  Cancelling due to test failure: 1 test still running\n        PASS [   0.001s] other case\n{SEPARATOR}\n     Summary [   0.002s] 2/3 tests run: 1 passed, 1 failed, 3 skipped\nwarning: 1/3 tests were not run due to test failure (run with --no-fail-fast to run all tests, or run with --max-fail)\n        FAIL [   0.001s] cache cache::round_trip\nerror: test run failed\n"
    )
}

pub(super) fn retry_report() -> String {
    report(
        &[
            ("PASS", "cache", "cache::round_trip"),
            ("PASS", "other", "case"),
            ("PASS", "other", "previously_unstarted"),
        ],
        3,
    )
}

#[test]
fn completed_fail_fast_failure_can_be_verified_by_a_full_retry_of_the_same_selection() {
    let failure = output(failure_report());
    let proof = output(retry_report());
    let original = failure.clone();
    assert_eq!(failure_diagnostics(&failure).len(), 1);
    assert!(failure.failed());
    assert!(!failure.succeeded());
    assert!(proof.succeeded());
    let required = Requirement::capture(&failure, true);
    assert!(required.is_guarded());
    assert!(required.is_verified_by(&Proof::capture(&proof)));
    assert_eq!(failure, original);
}

#[test]
fn unstarted_tests_are_not_skips_and_all_observed_subjects_must_survive() {
    let required = Requirement::capture(&output(failure_report()), true);
    let good = retry_report();
    for bad in [
        passed().text,
        good.replace("cache cache::round_trip", "replacement cache::round_trip"),
        good.replace("other case", "other replacement"),
        good.replace("3 tests skipped", "4 tests skipped").replace("3 skipped", "4 skipped"),
        good.replace("2 binaries", "3 binaries"),
        good.replace("        PASS [   0.001s] other previously_unstarted\n", ""),
        good.replace("3 tests run", "2/3 tests run").replace("3 passed", "2 passed"),
    ] {
        assert!(!required.is_verified_by(&Proof::capture(&output(bad))));
    }
}

#[test]
fn signal_timeout_cancel_reason_and_partial_summary_contradictions_abstain() {
    let good = Proof::capture(&output(retry_report()));
    let failed = failure_report();
    for bad in [
        failed.replace("due to test failure", "due to signal"),
        failed.replace("due to test failure", "due to global timeout"),
        failed.replace("  Cancelling due to test failure: 1 test still running\n", ""),
        failed.replace("2/3 tests run", "2/4 tests run"),
        failed.replace("2/3 tests run", "3/3 tests run"),
        failed.replace("1/3 tests were", "2/3 tests were"),
        failed.replace("1 test still running", "4 tests still running"),
        failed.replace("1 passed, 1 failed", "2 passed, 0 failed"),
        failed.replace("  Cancelling due to test failure:", "  Cancelling due to test failure\n  Cancelling due to test failure:"),
        failed.replace("3 tests across", "4 tests across"),
        format!("{failed}warning: 1/3 tests were not run due to signal\n"),
    ] {
        let bad = output(bad);
        assert!(!Requirement::capture(&bad, true).is_verified_by(&good));
        assert!(!bad.succeeded());
    }
}

#[test]
fn fail_fast_coverage_waits_for_final_process_observation_and_never_downgrades() {
    let failure = output(failure_report());
    let proof = Proof::capture(&output(retry_report()));
    let mut required = Requirement::capture(&failure, false);
    assert!(!required.is_verified_by(&proof));
    required.refresh(&failure, true);
    assert!(required.is_verified_by(&proof));
    required.refresh(&output(failure_report().replace("1/3 tests were", "2/3 tests were")), true);
    assert!(required.is_guarded());
    assert!(!required.is_verified_by(&proof));
}

#[test]
fn run_uuid_is_display_only_but_profile_is_part_of_execution_scope() {
    let required = Requirement::capture(&output(failure_report()), true);
    let same_profile = retry_report().replace(RUN_ID, "fedcba98-7654-3210-fedc-ba9876543210");
    assert!(required.is_verified_by(&Proof::capture(&output(same_profile))));
    let other_profile = retry_report().replace("profile: default", "profile: ci");
    assert!(!required.is_verified_by(&Proof::capture(&output(other_profile))));
}

#[test]
fn genuine_header_styling_cannot_bypass_native_failure_or_completion_guards() {
    let styled = |text: String| {
        text.replace(" Nextest run", "\u{1b}[32;1m Nextest run\u{1b}[0m")
            .replace("    Starting", "\u{1b}[32;1m    Starting\u{1b}[0m")
            .replace("3 tests across", "\u{1b}[1m3\u{1b}[0m tests across")
            .replace("        FAIL", "\u{1b}[31;1m        FAIL\u{1b}[0m")
            .replace("        PASS", "\u{1b}[32;1m        PASS\u{1b}[0m")
    };
    let failure = output(styled(failure_report()));
    let proof = output(styled(retry_report()));
    assert_eq!(failure_diagnostics(&failure).len(), 1);
    assert!(!failure.succeeded());
    assert!(Requirement::capture(&failure, true).is_verified_by(&Proof::capture(&proof)));
}

#[test]
fn native_slow_summary_and_final_slow_listing_do_not_invent_an_extra_test() {
    // Shape retained from nextest's published slow-output.ansi: a SLOW
    // notification, terminal PASS, annotated summary, then final SLOW listing.
    let good = passed().text
        .replace(
            "        PASS [   0.001s] cache",
            "        SLOW [> 60.000s] cache cache::round_trip\n        PASS [   0.001s] cache",
        )
        .replace("2 passed", "2 passed (1 slow)");
    let good = format!("{good}        SLOW [  90.001s] cache cache::round_trip\n");
    let required = Requirement::capture(&failed(), true);
    assert!(required.is_verified_by(&Proof::capture(&output(good.clone()))));
    for bad in [
        good.replace("1 slow", "2 slow"),
        good.replace("1 slow", "1 flaky"),
        good.replace("1 slow", "1 leaky"),
        good.replace("SLOW [  90.001s] cache cache::round_trip", "SLOW [  90.001s] cache fabricated"),
    ] {
        assert!(!required.is_verified_by(&Proof::capture(&output(bad))));
    }
}
