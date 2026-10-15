//! A repaired suite may add regressions, but must not lose its old coverage.

use super::*;
use crate::core::cass_error_recall::ToolOutput;
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};
use serde_json::{Value, json};

const FAILED: &str = "tests/test_parser.py::test_existing[unicode]";
const CONTROL: &str = "tests/test_parser.py::test_control";
const ADDED: &str = "tests/test_parser.py::test_regression";

fn report(results: &[(&str, &str)]) -> Value {
    let mut summary = json!({"collected":results.len(),"total":results.len()});
    let tests: Vec<_> = results
        .iter()
        .map(|(node, outcome)| {
            let count = summary[*outcome].as_u64().unwrap_or(0);
            summary[*outcome] = json!(count + 1);
            let call = match *outcome {
                "failed" => json!({"duration":0.001,"outcome":"failed",
                    "crash":{"message":"AssertionError: parser lost a Unicode character"}}),
                "skipped" | "xfailed" => json!({"duration":0.001,"outcome":"skipped"}),
                _ => json!({"duration":0.001,"outcome":"passed"}),
            };
            json!({"nodeid":node,"outcome":outcome,
                "setup":{"duration":0.001,"outcome":"passed"},"call":call,
                "teardown":{"duration":0.001,"outcome":"passed"}})
        })
        .collect();
    let failed = results.iter().any(|(_, outcome)| *outcome == "failed");
    json!({"created":1788000000.0,"duration":0.01,"exitcode":u8::from(failed),
        "root":"/workspace/project","summary":summary,"tests":tests})
}

fn output(report: &Value) -> ToolOutput {
    ToolOutput {
        text: report.to_string(),
        is_error: Some(false),
        exit_code: report["exitcode"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok()),
    }
}

fn verifies(failure: &Value, proof: &Value) -> bool {
    Requirement::capture(&output(failure), true).is_verified_by(&Proof::capture(&output(proof)))
}

#[test]
fn pytest_growth_learns_a_fix_that_adds_regression_tests() {
    let failure = report(&[(FAILED, "failed"), (CONTROL, "passed")]);
    let proof = report(&[(ADDED, "passed"), (CONTROL, "passed"), (FAILED, "passed")]);
    assert!(verifies(&failure, &proof));
    let mut reordered = proof.clone();
    reordered["tests"].as_array_mut().unwrap().reverse();
    assert!(verifies(&failure, &reordered));
}

#[test]
fn pytest_growth_cannot_drop_even_a_previously_passing_test() {
    let failure = report(&[(FAILED, "failed"), (CONTROL, "passed")]);
    let proof = report(&[(FAILED, "passed"), (ADDED, "passed")]);
    assert!(!verifies(&failure, &proof));
}

#[test]
fn pytest_growth_needs_each_failed_parameter_to_really_pass() {
    let failure = report(&[(FAILED, "failed"), (CONTROL, "passed")]);
    for outcome in ["skipped", "xfailed", "xpassed"] {
        let proof = report(&[(FAILED, outcome), (CONTROL, "passed"), (ADDED, "passed")]);
        assert!(!verifies(&failure, &proof), "{outcome}");
    }
    let proof = report(&[
        ("tests/test_parser.py::test_existing[ascii]", "passed"),
        (CONTROL, "passed"),
        (ADDED, "passed"),
    ]);
    assert!(!verifies(&failure, &proof));
}

#[test]
fn pytest_growth_rejects_a_new_failing_regression_even_after_the_old_fix() {
    let failure = report(&[(FAILED, "failed"), (CONTROL, "passed")]);
    let proof = report(&[(FAILED, "passed"), (CONTROL, "passed"), (ADDED, "failed")]);
    assert!(!verifies(&failure, &proof));
    // The execution matcher is conservative even without the outer process gate.
    let failure = decode(&failure.to_string()).unwrap().unwrap();
    let proof = decode(&proof.to_string()).unwrap().unwrap();
    assert!(!failure.execution.verified_by(&proof.execution));
}

#[test]
fn pytest_growth_still_binds_the_original_project_root() {
    let failure = report(&[(FAILED, "failed"), (CONTROL, "passed")]);
    let mut proof = report(&[(FAILED, "passed"), (CONTROL, "passed"), (ADDED, "passed")]);
    proof["root"] = json!("/workspace/other-project");
    assert!(!verifies(&failure, &proof));
}

#[test]
fn pytest_growth_refuses_duplicate_subjects_and_incomplete_reports() {
    let failure = report(&[(FAILED, "failed"), (CONTROL, "passed")]);
    let duplicate = report(&[(FAILED, "passed"), (CONTROL, "passed"), (FAILED, "passed")]);
    assert!(decode(&duplicate.to_string()).is_err());
    assert!(!verifies(&failure, &duplicate));
    let mut incomplete = report(&[(FAILED, "passed"), (CONTROL, "passed"), (ADDED, "passed")]);
    incomplete["tests"][2]
        .as_object_mut()
        .unwrap()
        .remove("teardown");
    assert!(!verifies(&failure, &incomplete));
    let mut stopped = report(&[(FAILED, "passed"), (CONTROL, "passed"), (ADDED, "passed")]);
    stopped["summary"]["collected"] = json!(4);
    assert!(!verifies(&failure, &stopped));
}

#[test]
fn pytest_growth_keeps_every_failure_beyond_the_diagnostic_display_cap() {
    let names: Vec<_> = (0..MAX_DIAGNOSTICS_PER_FAILURE + 3)
        .map(|index| format!("tests/test_parser.py::test_existing[{index}]"))
        .collect();
    let failed: Vec<_> = names.iter().map(|name| (name.as_str(), "failed")).collect();
    let mut passed: Vec<_> = names.iter().map(|name| (name.as_str(), "passed")).collect();
    passed.push((ADDED, "passed"));
    let failure = report(&failed);
    assert!(verifies(&failure, &report(&passed)));
    passed[0].1 = "skipped";
    assert!(!verifies(&failure, &report(&passed)));
}

#[test]
fn pytest_growth_still_requires_independent_completed_process_evidence() {
    let failure = report(&[(FAILED, "failed"), (CONTROL, "passed")]);
    let proof = report(&[(FAILED, "passed"), (CONTROL, "passed"), (ADDED, "passed")]);
    let required = Requirement::capture(&output(&failure), true);
    for exit_code in [None, Some(1)] {
        let unverified = ToolOutput {
            is_error: None,
            exit_code,
            ..output(&proof)
        };
        assert!(!required.is_verified_by(&Proof::capture(&unverified)));
    }
    assert!(required.is_verified_by(&Proof::capture(&output(&proof))));
}
