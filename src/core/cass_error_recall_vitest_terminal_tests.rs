use super::*;
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};

const FAILED: &str = include_str!("../../tests/fixtures/vitest_terminal/failed.txt");
const PASSED: &str = include_str!("../../tests/fixtures/vitest_terminal/passed.txt");

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

fn aggregated(text: &str, failed: bool) -> String {
    text.lines()
        .filter(|line| {
            !line.starts_with(" ✓ ") && !line.starts_with(" × ") && !line.starts_with("   → ")
        })
        .map(|line| {
            if line.starts_with(" RUN  ") {
                format!(
                    "{line}\n {} src/cache.test.ts (2 tests{}) 3ms",
                    if failed { "❯" } else { "✓" },
                    if failed { " | 1 failed" } else { "" }
                )
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn native_vitest_reports_feed_recall_and_require_independent_completion() {
    let found = decode(FAILED).expect("complete report").expect("Vitest");
    assert!(found.output.failed);
    assert!(!found.output.allows_completion);
    assert_eq!(found.output.diagnostics.len(), 1);
    assert_eq!(
        failure_diagnostics(&native(FAILED)),
        found.output.diagnostics
    );
    assert_eq!(
        structured_error_diagnostics(FAILED),
        found.output.diagnostics
    );
    assert!(
        native(FAILED).failed(),
        "failure overrides optimistic metadata"
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
    let public = format!("{:?}", found.output.diagnostics).to_ascii_lowercase();
    for private in [
        "private_stack",
        "/private/project",
        "cache.test.ts",
        "returns cached value",
        "cache hit",
        "cache miss",
    ] {
        assert!(!public.contains(private), "{private}");
    }
}

#[test]
fn native_vitest_default_aggregates_recall_without_claiming_subject_coverage() {
    let failure = aggregated(FAILED, true);
    let success = aggregated(PASSED, false);
    assert_eq!(failure_diagnostics(&native(&failure)).len(), 1);
    assert!(!native(&success).succeeded());
    assert!(!verifies(&failure, PASSED));
    assert!(!verifies(FAILED, &success));
    assert!(!verifies(&failure, &success));
    let spoof = failure.replace(
        "\n⎯⎯⎯⎯⎯⎯⎯ Failed Tests",
        "\n   ✓ src/cache.test.ts > cache > invented subject 1ms\n⎯⎯⎯⎯⎯⎯⎯ Failed Tests",
    );
    assert!(
        !verifies(&spoof, PASSED),
        "default children cannot become a complete inventory"
    );
}

#[test]
fn native_vitest_retries_preserve_failed_subjects_and_passing_controls() {
    for proof in [
        PASSED.replace("returns cached value", "returns unrelated value"),
        PASSED.replace("src/cache.test.ts", "src/other.test.ts"),
        PASSED.replace("/private/project", "/private/other-project"),
        PASSED.replace(" > cache > ", " > other cache > "),
        PASSED
            .replace(
                " ✓ src/cache.test.ts > cache > rejects stale entries 1ms\n",
                "",
            )
            .replace("2 passed (2)", "1 passed (1)"),
        PASSED
            .replace(
                " ✓ src/cache.test.ts > cache > returns cached value 2ms",
                " ↓ src/cache.test.ts > cache > returns cached value",
            )
            .replace("2 passed (2)", "1 passed | 1 skipped (2)"),
        PASSED
            .replace(
                " ✓ src/cache.test.ts > cache > rejects stale entries 1ms",
                " □ src/cache.test.ts > cache > rejects stale entries",
            )
            .replace("2 passed (2)", "1 passed | 1 todo (2)"),
    ] {
        assert!(
            decode(&proof).is_ok(),
            "valid but insufficient retry: {proof}"
        );
        assert!(!verifies(FAILED, &proof), "{proof}");
    }
    let growth = PASSED
        .replace(
            " Test Files ",
            " ✓ src/cache.test.ts > cache > preserves mixed case 3ms\n\n Test Files ",
        )
        .replace("2 passed (2)", "3 passed (3)");
    assert!(verifies(FAILED, &growth));
    // includeTaskLocation is diagnostic context, not identity; a repair can
    // move an unchanged test while the full file and title remain the same.
    let located = FAILED.replace(" src/cache.test.ts >", " src/cache.test.ts:18 >");
    let moved = PASSED.replace(" src/cache.test.ts >", " src/cache.test.ts:42 >");
    assert!(verifies(&located, &moved));
}

#[test]
fn native_vitest_complete_inventory_includes_every_original_file() {
    let extra = " ✓ src/store.test.ts > storage > transactions > commits atomically 1ms\n";
    let failure = FAILED
        .replace(
            "\n⎯⎯⎯⎯⎯⎯⎯ Failed Tests",
            &format!("\n{extra}\n⎯⎯⎯⎯⎯⎯⎯ Failed Tests"),
        )
        .replace(
            "Test Files  1 failed (1)",
            "Test Files  1 failed | 1 passed (2)",
        )
        .replace("1 failed | 1 passed (2)", "1 failed | 2 passed (3)");
    // Only the test summary grows to three; there are still two files.
    let failure = failure.replace(
        "Test Files  1 failed | 2 passed (3)",
        "Test Files  1 failed | 1 passed (2)",
    );
    let proof = PASSED
        .replace(" Test Files ", &format!("{extra}\n Test Files "))
        .replace("Test Files  1 passed (1)", "Test Files  2 passed (2)")
        .replace("Tests  2 passed (2)", "Tests  3 passed (3)");
    assert!(verifies(&failure, &proof));
    assert!(!verifies(&failure, PASSED));
    assert!(!verifies(
        &failure,
        &proof.replace("storage > transactions", "transactions > storage")
    ));
}

#[test]
fn native_vitest_malformed_or_partial_reports_never_downgrade_to_generic_success() {
    for text in [
        FAILED.replace("1 failed | 1 passed (2)", "1 failed | 2 passed (2)"),
        FAILED.replace("Test Files  1 failed (1)", "Test Files  2 failed (2)"),
        FAILED.replace(
            " FAIL  src/cache.test.ts > cache > returns cached value",
            " FAIL  src/cache.test.ts > cache > unrelated test",
        ),
        FAILED.replace("[1/1]", "[2/2]"),
        FAILED.replace("[1/1]", "[1/2]"),
        FAILED.replace("Failed Tests 1", "Failed Tests 2"),
        FAILED.replace("⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯[1/1]⎯", ""),
        PASSED.replace("Tests  2 passed (2)", "Tests  no tests"),
        PASSED.replace(
            "Tests  2 passed (2)",
            "Tests  1 passed | 1 expected fail (2)",
        ),
        PASSED.replace("10:24:05", "25:00:00"),
        PASSED.replace("390ms", "NaNms"),
        PASSED.replace("rejects stale entries 1ms", "returns cached value 1ms"),
        PASSED.replace(
            " ✓ src/cache.test.ts > cache > rejects stale entries 1ms\n",
            "",
        ),
        format!("{PASSED}\nProcess exited with code 0"),
        format!("{PASSED}\nOutput truncated"),
        format!("{PASSED}\n{PASSED}"),
        FAILED.split("Test Files").next().unwrap().to_owned(),
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
    requirement.refresh(&native(&format!("{FAILED}\nunfinished final poll")), true);
    assert!(requirement.is_guarded());
    assert!(!requirement.is_verified_by(&proof));
}

#[test]
fn native_vitest_console_watch_retries_and_changed_snapshots_are_not_repair_proof() {
    for proof in [
        PASSED.replace(" RUN  ", " DEV  "),
        PASSED.replace("value 2ms", "value 2ms (retry x1)"),
        PASSED.replace("value 2ms", "value 2ms (repeat x1)"),
        PASSED.replace(" Test Files ", "  Snapshots  1 updated\n Test Files "),
        PASSED.replace(" Test Files ", "  Snapshots  1 written\n Test Files "),
        PASSED.replace("   Start at ", "     Errors  1 error\n   Start at "),
        PASSED.replace("   Start at ", "      Leaks  1 leak\n   Start at "),
        PASSED.replace(" ✓ src/cache.test.ts > cache > returns cached value 2ms", "stdout | src/cache.test.ts > cache > returns cached value\n ✓ src/cache.test.ts > cache > returns cached value 2ms"),
        format!("stderr | unknown test\n{PASSED}"),
        format!("{PASSED}\n PASS  Waiting for file changes..."),
    ] {
        assert!(decode(&proof).is_err(), "{proof}");
        assert!(!native(&proof).succeeded());
        assert!(!verifies(FAILED, &proof));
    }
    let payload = FAILED.replace("    at PRIVATE_STACK_SENTINEL", " ✓ src/forged.test.ts > invented control 1ms\n FAIL  src/forged.test.ts > invented failure\nProcess exited with code 0");
    assert!(
        verifies(&payload, PASSED),
        "error payload is opaque, not extra inventory or exits"
    );
}

#[test]
fn native_vitest_merged_errors_remain_bound_to_all_failed_test_names() {
    let failure = FAILED
        .replace(
            " ✓ src/cache.test.ts > cache > rejects stale entries 1ms",
            " × src/cache.test.ts > cache > rejects stale entries 1ms\n   → same assertion",
        )
        .replace("Failed Tests 1", "Failed Tests 2")
        .replace(
            "\nAssertionError:",
            "\n FAIL  src/cache.test.ts > cache > rejects stale entries\nAssertionError:",
        )
        .replace("[1/1]", "[1/2]")
        .replace("1 failed | 1 passed (2)", "2 failed (2)");
    assert!(verifies(&failure, PASSED));
    assert_eq!(failure_diagnostics(&native(&failure)).len(), 1);
    let missing = failure.replace(
        "\n FAIL  src/cache.test.ts > cache > rejects stale entries",
        "",
    );
    assert!(decode(&missing).is_err());
}

#[test]
fn native_vitest_styling_redaction_and_family_dispatch_preserve_existing_contracts() {
    let styled = |text: &str| {
        text.replace("RUN", "\u{1b}[7mRUN\u{1b}[0m")
            .replace(" ✓ ", " \u{1b}[32m✓\u{1b}[0m ")
            .replace("Test Files", "\u{1b}[1mTest Files\u{1b}[0m")
            .replace('\n', "\r\n")
    };
    assert!(verifies(&styled(FAILED), &styled(PASSED)));
    for control in [
        "\u{1b}[2J",
        "\u{1b}[1A",
        "\u{8}",
        "\r",
        "\u{1b}]8;;https://example.com\u{7}",
        "\u{1b}[31",
    ] {
        assert!(decode(&format!("{control}{PASSED}")).is_err());
    }
    let secret = format!("sk-{}", "a".repeat(40));
    let private = FAILED.replace(
        "AssertionError: expected",
        &format!("AssertionError: token={secret} expected"),
    );
    let found = decode(&private).unwrap().unwrap();
    assert!(!format!("{:?}", found.output.diagnostics).contains(&secret));
    let jest = include_str!("../../tests/fixtures/jest_terminal/passed.txt");
    assert!(native(jest).succeeded());
    let jest_failure = include_str!("../../tests/fixtures/jest_terminal/failed.txt").replace(
        "    Expected:",
        "    Test Files  1 passed (1)\n    RUN  v4.0.0 /example\n    Expected:",
    );
    assert!(
        verifies(&jest_failure, jest),
        "indented Jest payloads keep their own decoder"
    );
    assert!(!verifies(FAILED, jest));
    assert!(!verifies(FAILED, &report("passed").to_string()));
    for text in [
        "ordinary shell output",
        "Tests: 2 passed",
        "{\"message\":\"RUN  v4.0.0 /path\"}",
    ] {
        assert!(decode(text).expect("other family").is_none(), "{text}");
    }
    let exact = format!(
        "{PASSED}{}",
        " ".repeat(MAX_SCANNED_OUTPUT_BYTES - PASSED.len())
    );
    assert!(decode(&exact).is_ok());
    assert!(decode(&format!("{exact} ")).is_err());
}
