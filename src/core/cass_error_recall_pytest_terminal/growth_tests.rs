//! Native named test inventories can grow when the repair adds regressions.

use super::*;
use crate::core::cass_error_recall::ToolOutput;
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};

const PYTEST_FAILED: &str = "tests/test_parser.py::test_existing[unicode]";
const PYTEST_CONTROL: &str = "tests/test_parser.py::test_control";
const PYTEST_ADDED: &str = "tests/test_parser.py::test_regression";
const UNIT_FAILED: &str = "test_existing (tests.test_parser.ParserTests.test_existing)";
const UNIT_CONTROL: &str = "test_control (tests.test_parser.ParserTests.test_control)";
const UNIT_ADDED: &str = "test_regression (tests.test_parser.ParserTests.test_regression)";

fn pytest_report(results: &[(&str, &str)]) -> String {
    let mut text = format!(
        "================ test session starts ================\ncollected {} items\n\n",
        results.len()
    );
    let mut counts = BTreeMap::<&str, usize>::new();
    for (node, outcome) in results {
        text.push_str(&format!("{node} {outcome}\n"));
        let label = match *outcome {
            "PASSED" => "passed",
            "FAILED" => "failed",
            "ERROR" => "error",
            "SKIPPED" => "skipped",
            "XFAIL" => "xfailed",
            "XPASS" => "xpassed",
            _ => panic!("unsupported fixture outcome"),
        };
        *counts.entry(label).or_default() += 1;
    }
    text.push_str("================ short test summary info ================\n");
    for (node, outcome) in results {
        if matches!(*outcome, "FAILED" | "ERROR") {
            text.push_str(&format!(
                "{outcome} {node} - AssertionError: parser mismatch\n"
            ));
        }
    }
    let counts = counts
        .iter()
        .map(|(name, count)| format!("{count} {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    text.push_str(&format!(
        "================ {counts} in 0.01s ================\n"
    ));
    text
}

fn unittest_report(results: &[(&str, &str)]) -> String {
    let mut text = String::new();
    let mut counts = BTreeMap::<&str, usize>::new();
    for (node, outcome) in results {
        text.push_str(&format!("{node} ... {outcome}\n"));
        let label = match *outcome {
            "ok" => continue,
            "FAIL" => "failures",
            "ERROR" => "errors",
            "skipped 'not exercised'" => "skipped",
            "expected failure" => "expected failures",
            "unexpected success" => "unexpected successes",
            _ => panic!("unsupported fixture outcome"),
        };
        *counts.entry(label).or_default() += 1;
    }
    let failed = counts.contains_key("failures")
        || counts.contains_key("errors")
        || counts.contains_key("unexpected successes");
    text.push_str(&format!(
        "\n----------------------------------------------------------------------\nRan {} tests in 0.001s\n\n{}",
        results.len(),
        if failed { "FAILED" } else { "OK" }
    ));
    if !counts.is_empty() {
        let counts = counts
            .iter()
            .map(|(name, count)| format!("{name}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        text.push_str(&format!(" ({counts})"));
    }
    text.push('\n');
    text
}

fn output(text: &str, exit_code: i64) -> ToolOutput {
    ToolOutput {
        text: text.to_owned(),
        is_error: Some(false),
        exit_code: Some(exit_code),
    }
}

fn verifies(failure: &str, proof: &str) -> bool {
    Requirement::capture(&output(failure, 1), true)
        .is_verified_by(&Proof::capture(&output(proof, 0)))
}

#[test]
fn native_pytest_growth_verifies_a_fix_with_added_regressions() {
    let failure = pytest_report(&[(PYTEST_FAILED, "FAILED"), (PYTEST_CONTROL, "PASSED")]);
    let proof = pytest_report(&[
        (PYTEST_ADDED, "PASSED"),
        (PYTEST_CONTROL, "PASSED"),
        (PYTEST_FAILED, "PASSED"),
    ]);
    assert!(verifies(&failure, &proof));
    // Terminal colors do not change the exact decoded subjects.
    assert!(verifies(
        &failure,
        &proof.replace("PASSED", "\x1b[32mPASSED\x1b[0m")
    ));
}

#[test]
fn native_unittest_growth_verifies_a_fix_with_added_regressions() {
    let failure = unittest_report(&[(UNIT_FAILED, "FAIL"), (UNIT_CONTROL, "ok")]);
    let proof = unittest_report(&[
        (UNIT_ADDED, "ok"),
        (UNIT_CONTROL, "ok"),
        (UNIT_FAILED, "ok"),
    ]);
    assert!(verifies(&failure, &proof));
}

#[test]
fn native_python_growth_cannot_drop_a_previously_passing_control() {
    let failure = pytest_report(&[(PYTEST_FAILED, "FAILED"), (PYTEST_CONTROL, "PASSED")]);
    let proof = pytest_report(&[(PYTEST_FAILED, "PASSED"), (PYTEST_ADDED, "PASSED")]);
    assert!(!verifies(&failure, &proof));
    let failure = unittest_report(&[(UNIT_FAILED, "FAIL"), (UNIT_CONTROL, "ok")]);
    let proof = unittest_report(&[(UNIT_FAILED, "ok"), (UNIT_ADDED, "ok")]);
    assert!(!verifies(&failure, &proof));
}

#[test]
fn native_python_growth_requires_normal_passes_for_every_failed_subject() {
    let failure = pytest_report(&[(PYTEST_FAILED, "FAILED"), (PYTEST_CONTROL, "PASSED")]);
    for outcome in ["SKIPPED", "XFAIL", "XPASS", "FAILED", "ERROR"] {
        let proof = pytest_report(&[
            (PYTEST_FAILED, outcome),
            (PYTEST_CONTROL, "PASSED"),
            (PYTEST_ADDED, "PASSED"),
        ]);
        assert!(!verifies(&failure, &proof), "{outcome}");
    }
    let failure = unittest_report(&[(UNIT_FAILED, "FAIL"), (UNIT_CONTROL, "ok")]);
    for outcome in [
        "skipped 'not exercised'",
        "expected failure",
        "unexpected success",
        "FAIL",
        "ERROR",
    ] {
        let proof = unittest_report(&[
            (UNIT_FAILED, outcome),
            (UNIT_CONTROL, "ok"),
            (UNIT_ADDED, "ok"),
        ]);
        assert!(!verifies(&failure, &proof), "{outcome}");
    }
}

#[test]
fn native_python_growth_rejects_new_failures_at_the_execution_boundary() {
    for (failure, proof) in [
        (
            pytest_report(&[(PYTEST_FAILED, "FAILED"), (PYTEST_CONTROL, "PASSED")]),
            pytest_report(&[
                (PYTEST_FAILED, "PASSED"),
                (PYTEST_CONTROL, "PASSED"),
                (PYTEST_ADDED, "FAILED"),
            ]),
        ),
        (
            unittest_report(&[(UNIT_FAILED, "FAIL"), (UNIT_CONTROL, "ok")]),
            unittest_report(&[
                (UNIT_FAILED, "ok"),
                (UNIT_CONTROL, "ok"),
                (UNIT_ADDED, "FAIL"),
            ]),
        ),
    ] {
        assert!(!verifies(&failure, &proof));
        let failure = decode(&failure).unwrap().unwrap();
        let proof = decode(&proof).unwrap().unwrap();
        assert!(!failure.execution.verified_by(&proof.execution));
    }
}

#[test]
fn native_python_growth_binds_module_class_parameters_and_producer() {
    let pytest_failure = pytest_report(&[(PYTEST_FAILED, "FAILED"), (PYTEST_CONTROL, "PASSED")]);
    let pytest_proof = pytest_report(&[
        (PYTEST_FAILED, "PASSED"),
        (PYTEST_CONTROL, "PASSED"),
        (PYTEST_ADDED, "PASSED"),
    ]);
    for replacement in [
        "tests/test_other.py::test_existing[unicode]",
        "tests/test_parser.py::test_existing[ascii]",
    ] {
        assert!(!verifies(
            &pytest_failure,
            &pytest_proof.replace(PYTEST_FAILED, replacement)
        ));
    }
    let unit_failure = unittest_report(&[(UNIT_FAILED, "FAIL"), (UNIT_CONTROL, "ok")]);
    let unit_proof = unittest_report(&[
        (UNIT_FAILED, "ok"),
        (UNIT_CONTROL, "ok"),
        (UNIT_ADDED, "ok"),
    ]);
    for replacement in ["tests.test_other", "tests.test_parser.OtherTests"] {
        assert!(!verifies(
            &unit_failure,
            &unit_proof.replace("tests.test_parser.ParserTests", replacement)
        ));
    }
    assert!(!verifies(&pytest_failure, &unit_proof));
    assert!(!verifies(&unit_failure, &pytest_proof));
}

#[test]
fn native_python_growth_cannot_infer_coverage_from_quiet_or_incomplete_runs() {
    let failure = pytest_report(&[(PYTEST_FAILED, "FAILED"), (PYTEST_CONTROL, "PASSED")]);
    assert!(!verifies(&failure, "... [100%]\n3 passed in 0.01s\n"));
    let proof = pytest_report(&[
        (PYTEST_FAILED, "PASSED"),
        (PYTEST_CONTROL, "PASSED"),
        (PYTEST_ADDED, "PASSED"),
    ]);
    assert!(!verifies(
        &failure,
        &proof.replace("collected 3 items", "collected 4 items")
    ));
    let duplicate = proof.replace(PYTEST_ADDED, PYTEST_CONTROL);
    assert!(!verifies(&failure, &duplicate));
    let failure = unittest_report(&[(UNIT_FAILED, "FAIL"), (UNIT_CONTROL, "ok")]);
    let quiet = "...\n----------------------------------------------------------------------\nRan 3 tests in 0.001s\n\nOK\n";
    assert!(!verifies(&failure, quiet));
    let proof = unittest_report(&[
        (UNIT_FAILED, "ok"),
        (UNIT_CONTROL, "ok"),
        (UNIT_ADDED, "ok"),
    ]);
    assert!(!verifies(
        &failure,
        &proof.replace("Ran 3 tests", "Ran 4 tests")
    ));
    assert!(!verifies(
        &failure,
        &proof.replace(UNIT_ADDED, UNIT_CONTROL)
    ));
}

#[test]
fn native_python_growth_needs_independent_process_completion() {
    for (failure, proof) in [
        (
            pytest_report(&[(PYTEST_FAILED, "FAILED"), (PYTEST_CONTROL, "PASSED")]),
            pytest_report(&[
                (PYTEST_FAILED, "PASSED"),
                (PYTEST_CONTROL, "PASSED"),
                (PYTEST_ADDED, "PASSED"),
            ]),
        ),
        (
            unittest_report(&[(UNIT_FAILED, "FAIL"), (UNIT_CONTROL, "ok")]),
            unittest_report(&[
                (UNIT_FAILED, "ok"),
                (UNIT_CONTROL, "ok"),
                (UNIT_ADDED, "ok"),
            ]),
        ),
    ] {
        let required = Requirement::capture(&output(&failure, 1), true);
        for exit_code in [None, Some(1)] {
            let unknown = ToolOutput {
                is_error: None,
                exit_code,
                ..output(&proof, 0)
            };
            assert!(!required.is_verified_by(&Proof::capture(&unknown)));
        }
        assert!(required.is_verified_by(&Proof::capture(&output(&proof, 0))));
    }
}
