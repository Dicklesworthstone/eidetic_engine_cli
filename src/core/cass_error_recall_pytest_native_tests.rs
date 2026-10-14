use super::*;
use super::super::super::{ToolOutput, failure_diagnostics, structured_error_diagnostics};
use serde_json::{Value, json};

fn native() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/cass/pytest-json-report-native-phases.json"))
        .expect("normalized native pytest-json-report 1.5.0 fixture")
}

fn passed() -> Value {
    let mut report = native();
    report["tests"] = json!([report["tests"][0].clone()]);
    report["summary"] = json!({"collected":1,"total":1,"passed":1});
    report["exitcode"] = json!(0);
    report.as_object_mut().expect("report").remove("collectors");
    report
}

fn optimistic(report: &Value) -> ToolOutput {
    ToolOutput {
        text: report.to_string(),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

#[test]
fn native_fixture_reports_all_failure_phases_but_not_skips_or_expected_failures() {
    let report = native();
    let parsed = inspect(&report.to_string()).expect("valid").expect("pytest");
    assert!(parsed.failed);
    assert!(!parsed.allows_completion);
    assert_eq!(parsed.diagnostics.len(), 3);
    for phase in ["pytest setup:", "pytest call:", "pytest teardown:"] {
        assert!(parsed.diagnostics.iter().any(|d| d.message_template.starts_with(phase)));
    }
    let output = optimistic(&report);
    assert_eq!(failure_diagnostics(&output), parsed.diagnostics);
    assert_eq!(structured_error_diagnostics(&output.text), parsed.diagnostics);
    assert!(output.failed());
    assert!(!output.succeeded());
}

#[test]
fn clean_nonempty_report_permits_but_does_not_supply_process_success() {
    let value = passed();
    let output = ToolOutput { text: value.to_string(), ..ToolOutput::default() };
    assert!(inspect(&output.text).expect("valid").expect("pytest").allows_completion);
    assert!(!output.succeeded());
    assert!(ToolOutput { exit_code: Some(0), ..output.clone() }.succeeded());
    assert!(!ToolOutput { exit_code: Some(1), ..output }.succeeded());
}

#[test]
fn metadata_streams_paths_and_traceback_tails_do_not_become_diagnostic_content() {
    let mut report = native();
    let before = failure_diagnostics(&optimistic(&report));
    report["root"] = json!("/PRIVATE_ROOT_SENTINEL");
    report["environment"] = json!({"error":"PRIVATE_ENVIRONMENT_SENTINEL"});
    report["tests"][1]["metadata"] = json!({"error":"PRIVATE_METADATA_SENTINEL"});
    for (field, value) in [
        ("stdout", json!("error[E0277]: PRIVATE_STDOUT_SENTINEL")),
        ("stderr", json!("PRIVATE_STDERR_SENTINEL")),
        ("traceback", json!([{"message":"PRIVATE_TRACEBACK_SENTINEL"}])),
        ("longrepr", json!("PRIVATE_LONGREPR_SENTINEL")),
    ] {
        report["tests"][1]["call"][field] = value;
    }
    report["tests"][1]["call"]["crash"]["path"] = json!("/PRIVATE_PATH_SENTINEL");
    report["tests"][1]["call"]["crash"]["message"] =
        json!("AssertionError: cache value mismatch\nPRIVATE_SOURCE_LINE_SENTINEL");
    assert_eq!(failure_diagnostics(&optimistic(&report)), before);
    let credential = format!("ghp_{}", "Q".repeat(36));
    report["tests"][1]["call"]["crash"]["message"] = json!(format!("api_key={credential}"));
    let diagnostic = format!("{:?}", failure_diagnostics(&optimistic(&report)));
    assert!(!diagnostic.to_ascii_lowercase().contains(&credential.to_ascii_lowercase()));
    assert!(!diagnostic.contains("PRIVATE_"));
}

#[test]
fn inconsistent_counters_lifecycles_and_envelopes_are_refused_as_a_whole() {
    for (pointer, value) in [
        ("/summary/passed", json!(99)),
        ("/summary/total", json!(99)),
        ("/summary/collected", json!(0)),
        ("/summary/deselected", json!(2)),
        ("/tests/0/nodeid", json!("\n")),
        ("/tests/0/outcome", json!("unexpected")),
        ("/tests/0/call/outcome", json!("failed")),
        ("/tests/0/setup/outcome", json!("skipped")),
        ("/tests/0/teardown/outcome", json!("failed")),
        ("/tests/0/call/duration", json!(-1)),
        ("/root", json!("")),
        ("/duration", json!(-1)),
        ("/exitcode", json!(-1)),
    ] {
        let mut report = passed();
        // deselected is optional in the native source.
        report["summary"]["deselected"] = json!(0);
        *report.pointer_mut(pointer).expect("fixture field") = value;
        assert!(inspect(&report.to_string()).is_err(), "{pointer}");
        assert!(failure_diagnostics(&optimistic(&report)).is_empty());
        assert!(!optimistic(&report).succeeded());
    }
    for key in ["schema", "reason", "$message_type", "testResults", "generalDiagnostics"] {
        let mut report = passed();
        report[key] = Value::Null;
        assert!(inspect(&report.to_string()).is_err(), "{key}");
    }
}

#[test]
fn duplicate_fields_nodes_and_trailing_documents_cannot_salvage_a_valid_prefix() {
    let report = passed();
    let text = report.to_string();
    for malformed in [
        text.replacen("\"exitcode\":0", "\"exitcode\":1,\"exitcode\":0", 1),
        text.replacen("\"total\":1", "\"total\":2,\"total\":1", 1),
        format!("{text}\n{text}"),
        format!("{text}\n{{"),
    ] {
        assert!(inspect(&malformed).is_err());
    }
    let mut duplicate = report;
    duplicate["tests"] = json!([duplicate["tests"][0].clone(), duplicate["tests"][0].clone()]);
    duplicate["summary"] = json!({"total":2,"collected":2,"passed":2});
    assert!(inspect(&duplicate.to_string()).is_err());
    let nested = json!({"example":native()});
    assert!(inspect(&nested.to_string()).expect("other family").is_none());
}

#[test]
fn summaries_empty_selections_and_interrupted_runs_are_not_repair_proof() {
    let base = passed();
    let mut summary_only = base.clone();
    summary_only.as_object_mut().expect("report").remove("tests");
    let mut empty = base.clone();
    empty["tests"] = json!([]);
    empty["summary"] = json!({"total":0,"collected":0});
    let mut interrupted = base.clone();
    interrupted["summary"]["collected"] = json!(10);
    let mut missing_teardown = base.clone();
    missing_teardown["tests"][0].as_object_mut().expect("test").remove("teardown");
    for report in [summary_only, empty, interrupted, missing_teardown] {
        assert!(!inspect(&report.to_string()).expect("valid partial").expect("pytest").allows_completion);
        assert!(!optimistic(&report).succeeded());
    }
    for code in [1, 2, 3, 4, 5, 6, 99] {
        let mut report = base.clone();
        report["exitcode"] = json!(code);
        assert!(!optimistic(&report).succeeded());
    }
}

#[test]
fn collector_failures_override_passing_tests_without_reading_collection_tracebacks() {
    let mut report = passed();
    report["collectors"] = json!([{"nodeid":"test_broken.py","outcome":"failed","longrepr":"PRIVATE_COLLECTION_TRACEBACK"}]);
    let output = inspect(&report.to_string()).expect("valid").expect("pytest");
    assert!(output.failed);
    assert!(!output.allows_completion);
    assert_eq!(output.diagnostics.len(), 1);
    assert_eq!(output.diagnostics[0].message_template, "pytest collection: collection failed");
    assert!(!optimistic(&report).succeeded());
}

#[test]
fn failure_display_cap_never_skips_validation_of_later_nodes() {
    let mut report = native();
    let failure = report["tests"][1].clone();
    let tests: Vec<_> = (0..8).map(|i| {
        let mut test = failure.clone();
        test["nodeid"] = json!(format!("test_case.py::case_{i}"));
        test["call"]["crash"]["message"] = json!(format!("DistinctException{i}: invalid cache"));
        test
    }).collect();
    report["tests"] = json!(tests);
    report["summary"] = json!({"collected":8,"total":8,"failed":8});
    assert_eq!(failure_diagnostics(&optimistic(&report)).len(), MAX_DIAGNOSTICS_PER_FAILURE);
    report["tests"][7]["call"]["duration"] = json!(-1);
    assert!(inspect(&report.to_string()).is_err());
    assert!(failure_diagnostics(&optimistic(&report)).is_empty());
    assert!(inspect(&format!("{}{}", passed(), " ".repeat(MAX_SCANNED_OUTPUT_BYTES))).is_err());
}


#[test]
fn native_finalizer_skips_do_not_erase_failed_calls_or_their_retry_requirements() {
    use super::super::{Proof, Requirement};

    let mut report: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/cass/pytest-json-report-native-finalizer-mask.json"
    )).expect("native skipped/expected-failure finalizers");
    assert_eq!(report["summary"]["failed"], Value::Null);
    let parsed = decode(&report.to_string()).expect("valid native report").expect("pytest");
    assert!(parsed.output.failed);
    assert_eq!(parsed.output.diagnostics.len(), 2);
    assert!(parsed.execution.has_known_failures());
    let required = Requirement::capture(&optimistic(&report), true);
    assert!(required.is_guarded());
    assert!(!required.is_verified_by(&Proof::capture(&optimistic(&report))));

    // Rerun both original nodes, but retain their skipped/xfail labels: even
    // an exit-zero run cannot turn non-execution into a repaired assertion.
    for test in report["tests"].as_array_mut().expect("test nodes") {
        test["call"] = json!({"outcome":"passed","duration":0.0});
    }
    report["exitcode"] = json!(0);
    assert!(optimistic(&report).succeeded());
    assert!(!required.is_verified_by(&Proof::capture(&optimistic(&report))));

    for test in report["tests"].as_array_mut().expect("test nodes") {
        test["outcome"] = json!("passed");
        test["teardown"] = json!({"outcome":"passed","duration":0.0});
    }
    report["summary"] = json!({"total":3,"collected":3,"passed":3});
    assert!(required.is_verified_by(&Proof::capture(&optimistic(&report))));
}
