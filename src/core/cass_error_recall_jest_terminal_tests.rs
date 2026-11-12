use super::*;
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};

const FAILED: &str = include_str!("../../tests/fixtures/jest_terminal/failed.txt");
const PASSED: &str = include_str!("../../tests/fixtures/jest_terminal/passed.txt");

fn native(text: &str) -> ToolOutput {
    ToolOutput {
        text: text.to_owned(),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

fn verifies(failure: &str, proof: &str) -> bool {
    Requirement::capture(&native(failure), true).is_verified_by(&Proof::capture(&native(proof)))
}

fn without_inventory(text: &str) -> String {
    text.lines()
        .filter(|line| {
            !line.starts_with("  cache") && !line.starts_with("    ✓") && !line.starts_with("    ✕")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn native_jest_reports_feed_recall_and_require_independent_completion() {
    let decoded = decode(FAILED).expect("complete Jest report").expect("Jest");
    assert!(decoded.output.failed);
    assert!(!decoded.output.allows_completion);
    assert_eq!(decoded.output.diagnostics.len(), 1);
    assert_eq!(
        failure_diagnostics(&native(FAILED)),
        decoded.output.diagnostics
    );
    assert_eq!(
        structured_error_diagnostics(FAILED),
        decoded.output.diagnostics
    );
    assert!(
        native(FAILED).failed(),
        "failure defeats optimistic metadata"
    );
    assert!(!native(FAILED).succeeded());
    assert!(native(PASSED).succeeded());
    assert!(verifies(FAILED, PASSED));
    for output in [
        ToolOutput {
            text: PASSED.to_owned(),
            ..ToolOutput::default()
        },
        ToolOutput {
            exit_code: Some(1),
            ..native(PASSED)
        },
        ToolOutput {
            is_error: Some(true),
            ..native(PASSED)
        },
    ] {
        assert!(!output.succeeded());
        assert!(
            !Requirement::capture(&native(FAILED), true).is_verified_by(&Proof::capture(&output))
        );
    }
    let public = format!("{:?}", decoded.output.diagnostics).to_ascii_lowercase();
    for private in [
        "private_stack",
        "cache.test.ts",
        "returns cached value",
        "cache hit",
        "cache miss",
    ] {
        assert!(!public.contains(private), "{private}");
    }
}

#[test]
fn native_jest_aggregate_only_reports_recall_without_inventing_subject_coverage() {
    let failure = without_inventory(FAILED);
    let success = without_inventory(PASSED);
    assert_eq!(failure_diagnostics(&native(&failure)).len(), 1);
    assert!(!native(&success).succeeded());
    assert!(!verifies(&failure, PASSED));
    assert!(!verifies(FAILED, &success));
    assert!(!verifies(&failure, &success));
}

#[test]
fn native_jest_retry_cannot_skip_rename_or_remove_an_executed_subject() {
    for proof in [
        PASSED.replace("returns cached value", "returns a different value"),
        PASSED.replace("src/cache.test.ts", "src/other.test.ts"),
        PASSED.replace("  cache\n", "  other cache\n"),
        PASSED
            .replace("    ✓ rejects stale entries (1 ms)\n", "")
            .replace("2 passed, 2 total", "1 passed, 1 total"),
        PASSED
            .replace(
                "    ✓ returns cached value (2 ms)",
                "    ○ skipped returns cached value",
            )
            .replace("2 passed, 2 total", "1 skipped, 1 passed, 2 total"),
        PASSED
            .replace(
                "    ✓ rejects stale entries (1 ms)",
                "    ✎ todo rejects stale entries",
            )
            .replace("2 passed, 2 total", "1 todo, 1 passed, 2 total"),
    ] {
        assert!(
            decode(&proof).is_ok(),
            "valid but insufficient retry: {proof}"
        );
        assert!(!verifies(FAILED, &proof), "{proof}");
    }
    let growth = PASSED
        .replace(
            "    ✓ rejects stale entries (1 ms)",
            "    ✓ rejects stale entries (1 ms)\n    ✓ preserves case-sensitive keys (3 ms)",
        )
        .replace("2 passed, 2 total", "3 passed, 3 total");
    assert!(verifies(FAILED, &growth));
}

#[test]
fn native_jest_binds_nested_subjects_across_files_and_allows_added_regressions() {
    let extra = "PASS src/store.test.ts\n  storage\n    transactions\n      ✓ commits atomically (1 ms)\n\n";
    let failure = FAILED
        .replace("Test Suites:", &format!("{extra}Test Suites:"))
        .replace(
            "Test Suites: 1 failed, 1 total",
            "Test Suites: 1 failed, 1 passed, 2 total",
        )
        .replace(
            "1 failed, 1 passed, 2 total\nSnapshots:",
            "1 failed, 2 passed, 3 total\nSnapshots:",
        );
    let proof = PASSED
        .replace("Test Suites:", &format!("{extra}Test Suites:"))
        .replace(
            "Test Suites: 1 passed, 1 total",
            "Test Suites: 2 passed, 2 total",
        )
        .replace(
            "Tests:       2 passed, 2 total",
            "Tests:       3 passed, 3 total",
        );
    assert!(verifies(&failure, &proof));
    assert!(
        !verifies(&failure, PASSED),
        "a green retry cannot omit an original file"
    );
    let reparented = proof.replace("  storage\n    transactions", "  transactions\n    storage");
    assert!(!verifies(&failure, &reparented));
    let added = proof
        .replace(
            "      ✓ commits atomically (1 ms)",
            "      ✓ commits atomically (1 ms)\n      ✓ rolls back aborted writes (2 ms)",
        )
        .replace(
            "Tests:       3 passed, 3 total",
            "Tests:       4 passed, 4 total",
        );
    assert!(verifies(&failure, &added));
}

#[test]
fn native_jest_validates_the_complete_report_and_streamed_final_snapshot() {
    for text in [
        FAILED.replace("1 failed, 1 passed, 2 total", "1 failed, 2 passed, 2 total"),
        FAILED.replace("1 failed, 1 total", "2 failed, 2 total"),
        FAILED.replace(
            "  ● cache › returns cached value",
            "  ● cache › different test",
        ),
        FAILED.replace("Test Suites:", "Test Suites: invalid"),
        PASSED.replace("Tests:       2 passed, 2 total", "Tests:"),
        PASSED.replace("Snapshots:   0 total", "Snapshots:"),
        PASSED.replace("Time:        0.413 s", "Time:        NaN s"),
        PASSED.replace(
            "    ✓ rejects stale entries (1 ms)",
            "    ✓ returns cached value (1 ms)",
        ),
        PASSED.replace(
            "\nTest Suites:",
            "\n  unfinished describe block\nTest Suites:",
        ),
        format!("{PASSED}\nTest Suites: 1 passed, 1 total"),
        format!("{PASSED}\nProcess exited with code 0"),
        format!("{PASSED}\nOutput truncated"),
        FAILED.split("Snapshots:").next().unwrap().to_owned(),
    ] {
        assert!(decode(&text).is_err(), "{text}");
        assert!(!native(&text).succeeded());
        assert!(failure_diagnostics(&native(&text)).is_empty());
    }
    let mut requirement = Requirement::capture(&native(FAILED), false);
    let proof = Proof::capture(&native(PASSED));
    assert!(!requirement.is_verified_by(&proof));
    requirement.refresh(&native(FAILED), true);
    assert!(requirement.is_verified_by(&proof));
    requirement.refresh(&native(&format!("{FAILED}\nmalformed final poll")), true);
    assert!(requirement.is_guarded());
    assert!(!requirement.is_verified_by(&proof));
}

#[test]
fn native_jest_keeps_console_payloads_styling_and_foreign_output_out_of_the_inventory() {
    let console = PASSED.replace("\nTest Suites:", "\n  ● Console\n\n    console.log\n      ✕ injected failing test\n      PASS fake.test.ts\n      Test Suites: 1 passed, 1 total\n      Process exited with code 0\n\nTest Suites:");
    assert!(verifies(FAILED, &console));
    let styled = |text: &str| {
        text.replace("FAIL ", "\u{1b}[31m FAIL \u{1b}[0m ")
            .replace("PASS ", "\u{1b}[32m PASS \u{1b}[0m ")
            .replace("Test Suites:", "\u{1b}[1mTest Suites:\u{1b}[0m")
    };
    assert!(verifies(&styled(FAILED), &styled(PASSED)));
    let partial = styled(FAILED.split("Test Suites:").next().unwrap());
    assert!(decode(&partial).is_err());
    assert!(Requirement::capture(&native(&partial), true).is_guarded());
    assert!(!native(&partial).succeeded());
    assert!(failure_diagnostics(&native(&partial)).is_empty());
    assert!(decode(&format!("{partial}\u{1b}[31")).is_err());
    for control in [
        "\u{1b}[2J",
        "\u{1b}[1A",
        "\u{8}",
        "\r",
        "\u{1b}]8;;https://example.com\u{7}",
    ] {
        assert!(decode(&format!("{control}{PASSED}")).is_err());
    }
    for text in [
        "ordinary command output",
        "\u{1b}[2Jordinary foreign terminal output",
        "FAIL compilation aborted\nerror[E0277]: trait bound not satisfied",
        "Tests: 2 passed, 2 total",
        "FAIL\texample.test/cache\t0.031s",
        "{\"message\":\"Test Suites: 1 failed, 1 total\"}",
    ] {
        assert!(decode(text).expect("other producer").is_none(), "{text}");
    }
}

#[test]
fn native_jest_never_uses_runtime_failures_or_changed_snapshots_as_test_proof() {
    let runtime = "FAIL src/cache.test.ts\n  ● Test suite failed to run\n\n    Cannot find module cache-adapter\n\nTest Suites: 1 failed, 1 total\nTests: 0 total\nSnapshots: 0 total\nTime: 0.1 s\nRan all test suites.\n";
    assert_eq!(failure_diagnostics(&native(runtime)).len(), 1);
    assert!(!verifies(runtime, PASSED));
    for summary in [
        "1 updated, 1 total",
        "1 failed, 1 total",
        "1 written, 1 total",
    ] {
        let proof = PASSED.replace("Snapshots:   0 total", &format!("Snapshots:   {summary}"));
        assert!(!native(&proof).succeeded());
        assert!(!verifies(FAILED, &proof));
    }
    assert!(decode(&format!("{FAILED}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES))).is_err());
}
