use super::*;
use crate::core::cass_error_recall::{ToolOutput, failure_diagnostics, structured_error_diagnostics};
use serde_json::{Value, json};

fn report(status: &str) -> Value {
    let failed = status == "failed";
    json!({
        "numTotalTestSuites": 1, "numPassedTestSuites": u64::from(!failed),
        "numFailedTestSuites": u64::from(failed), "numPendingTestSuites": 0,
        "numTotalTests": 1, "numPassedTests": u64::from(status == "passed"),
        "numFailedTests": u64::from(failed),
        "numPendingTests": u64::from(matches!(status, "pending" | "skipped" | "disabled")),
        "numTodoTests": u64::from(status == "todo"),
        "startTime": 1000, "success": !failed,
        "numRuntimeErrorTestSuites": 0, "wasInterrupted": false,
        "testResults": [{
            "name": "/private/project/cache.test.ts",
            "status": if failed { "failed" } else { "passed" },
            "message": "", "startTime": 1001, "endTime": 1002,
            "assertionResults": [{
                "ancestorTitles": ["cache"], "fullName": "cache returns cached value",
                "title": "returns cached value", "status": status,
                "failureMessages": if failed {
                    vec!["Expected cache hit, received cache miss\n    at PRIVATE_STACK_SENTINEL".to_owned()]
                } else { Vec::<String>::new() },
                "meta": {"private": "PRIVATE_META_SENTINEL"}
            }]
        }],
        "coverageMap": {"private": "PRIVATE_COVERAGE_SENTINEL"}
    })
}

fn output(value: Value) -> ToolOutput {
    ToolOutput { text: value.to_string(), is_error: Some(false), exit_code: Some(0) }
}

fn decoded(value: &Value) -> CompilerOutput {
    inspect(&value.to_string()).expect("valid report").expect("JavaScript report")
}

#[test]
fn javascript_assertion_failures_reach_canonical_recall_despite_optimistic_wrapper() {
    let value = report("failed");
    let found = decoded(&value);
    assert!(found.failed);
    assert!(!found.allows_completion);
    assert_eq!(found.diagnostics.len(), 1);
    assert!(found.diagnostics[0].message_template.starts_with("javascript-test:"));
    assert_eq!(failure_diagnostics(&output(value.clone())), found.diagnostics);
    assert_eq!(structured_error_diagnostics(&value.to_string()), found.diagnostics);
    assert!(output(value.clone()).failed());
    assert!(!output(value).succeeded());
    let public = format!("{:?}", found.diagnostics).to_ascii_lowercase();
    for private in ["private_stack", "private_meta", "private_coverage", "/private/project", "returns cached value"] {
        assert!(!public.contains(private));
    }
}

#[test]
fn javascript_clean_report_requires_independent_process_completion() {
    let value = report("passed");
    assert!(decoded(&value).allows_completion);
    assert!(output(value.clone()).succeeded());
    assert!(!ToolOutput { text: value.to_string(), ..ToolOutput::default() }.succeeded());
    assert!(!ToolOutput { exit_code: Some(1), ..output(value) }.succeeded());
}

#[test]
fn javascript_pending_skipped_disabled_todo_and_empty_runs_do_not_prove_success() {
    for status in ["pending", "skipped", "disabled", "todo"] {
        assert!(!decoded(&report(status)).allows_completion);
        assert!(!output(report(status)).succeeded());
    }
    let mut empty = report("passed");
    for name in ["numTotalTestSuites", "numPassedTestSuites", "numTotalTests", "numPassedTests"] {
        empty[name] = json!(0);
    }
    empty["testResults"] = json!([]);
    assert!(!decoded(&empty).allows_completion);
    assert!(!output(empty).succeeded());
}

#[test]
fn javascript_interruption_snapshot_failure_and_false_success_are_vetoes() {
    for (key, value) in [
        ("wasInterrupted", json!(true)),
        ("snapshot", json!({"failure": true})),
        ("success", json!(false)),
    ] {
        let mut bad = report("passed");
        bad[key] = value;
        assert!(decoded(&bad).failed);
        assert!(!output(bad).succeeded());
    }
    let mut optimistic = report("failed");
    optimistic["success"] = json!(true);
    assert!(decoded(&optimistic).failed);
    assert!(!output(optimistic).succeeded());
}

#[test]
fn javascript_nested_describe_suite_counts_are_not_file_counts() {
    let mut value = report("passed");
    value["numTotalTestSuites"] = json!(3);
    value["numPassedTestSuites"] = json!(3);
    assert!(decoded(&value).allows_completion);
    value["numPassedTestSuites"] = json!(2);
    value["numPendingTestSuites"] = json!(1);
    assert!(!decoded(&value).allows_completion);
}

#[test]
fn javascript_runtime_file_errors_are_recorded_without_invented_assertions() {
    let mut value = report("failed");
    value["numTotalTests"] = json!(0);
    value["numFailedTests"] = json!(0);
    value["numRuntimeErrorTestSuites"] = json!(1);
    value["testResults"][0]["assertionResults"] = json!([]);
    value["testResults"][0]["message"] = json!("Cannot find module cache-adapter\nPRIVATE_FRAME");
    let found = decoded(&value);
    assert!(found.failed);
    assert_eq!(found.diagnostics.len(), 1);
    assert!(!found.diagnostics[0].message_template.contains("private_frame"));
}

#[test]
fn javascript_rejects_inconsistent_counts_outcomes_and_identity() {
    let original = report("failed");
    for (pointer, replacement) in [
        ("/numFailedTests", json!(0)),
        ("/numTotalTests", json!(2)),
        ("/numFailedTestSuites", json!(0)),
        ("/numPassedTests", json!(u64::MAX)),
        ("/numRuntimeErrorTestSuites", json!(2)),
        ("/testResults/0/status", json!("passed")),
        ("/testResults/0/name", json!("")),
        ("/testResults/0/endTime", json!(0)),
        ("/testResults/0/assertionResults/0/status", json!("running")),
        ("/testResults/0/assertionResults/0/fullName", json!("different name")),
        ("/testResults/0/assertionResults/0/title", json!("")),
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(inspect(&value.to_string()).is_err(), "{pointer}");
        assert!(!output(value.clone()).succeeded());
        assert!(structured_error_diagnostics(&value.to_string()).is_empty());
    }
}

#[test]
fn javascript_duplicate_fields_null_claims_and_trailing_bytes_cannot_salvage_a_report() {
    let valid = report("passed").to_string();
    for text in [
        valid.replacen("\"success\":true", "\"success\":false,\"success\":true", 1),
        valid.replacen("\"status\":\"passed\"", "\"status\":\"failed\",\"status\":\"passed\"", 1),
        valid.replacen("\"numPassedTests\":1", "\"numPassedTests\":null,\"numPassedTests\":1", 1),
        valid[..valid.len() - 1].to_owned(),
        format!("{valid}\n{valid}"),
        format!("{valid} trailing error"),
        "{\"testResults\":null}".to_owned(),
        "{\"numTotalTests\":null,\"numFailedTests\":null}".to_owned(),
    ] {
        assert!(inspect(&text).is_err(), "{text}");
        assert!(!ToolOutput { text, is_error: Some(false), exit_code: Some(0) }.succeeded());
    }
}

#[test]
fn javascript_foreign_envelopes_are_rejected_but_nested_examples_are_not_mined() {
    for field in ["schema", "reason", "$message_type", "generalDiagnostics", "results"] {
        let mut value = report("passed");
        value[field] = Value::Null;
        assert!(inspect(&value.to_string()).is_err());
        assert!(!output(value).succeeded());
    }
    for value in [json!({"example": report("failed")}), json!({"success":true}), json!([report("failed")])] {
        assert!(inspect(&value.to_string()).unwrap().is_none());
    }
}

#[test]
fn javascript_duplicate_names_and_files_are_ambiguous() {
    let mut value = report("failed");
    let file = value["testResults"][0].clone();
    value["testResults"].as_array_mut().unwrap().push(file);
    assert!(inspect(&value.to_string()).is_err());
    let mut value = report("failed");
    let assertion = value["testResults"][0]["assertionResults"][0].clone();
    value["testResults"][0]["assertionResults"].as_array_mut().unwrap().push(assertion);
    assert!(inspect(&value.to_string()).is_err());
}

#[test]
fn javascript_diagnostic_cap_does_not_skip_tail_validation() {
    let mut value = report("failed");
    let template = value["testResults"][0]["assertionResults"][0].clone();
    let assertions = ["alpha", "beta", "gamma", "delta", "epsilon"].into_iter().map(|name| {
        let mut assertion = template.clone();
        assertion["title"] = json!(name);
        assertion["fullName"] = json!(format!("cache {name}"));
        assertion["failureMessages"] = json!([format!("Invalid {name} result")]);
        assertion
    }).collect::<Vec<_>>();
    value["testResults"][0]["assertionResults"] = json!(assertions);
    value["numTotalTests"] = json!(5);
    value["numFailedTests"] = json!(5);
    assert_eq!(decoded(&value).diagnostics.len(), MAX_DIAGNOSTICS_PER_FAILURE);
    value["testResults"][0]["assertionResults"][4]["status"] = json!("running");
    assert!(inspect(&value.to_string()).is_err());
}

#[test]
fn javascript_redacts_before_fingerprinting_and_enforces_the_byte_bound() {
    let mut value = report("failed");
    let secret = format!("ghp_{}", "Q".repeat(36));
    value["testResults"][0]["assertionResults"][0]["failureMessages"] = json!([format!("api_key={secret}")]);
    let found = decoded(&value);
    assert!(!format!("{:?}", found.diagnostics).to_lowercase().contains(&secret.to_lowercase()));
    let text = value.to_string();
    let exact = format!("{text}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES - text.len()));
    assert!(inspect(&exact).is_ok());
    assert!(inspect(&format!("{exact} ")).is_err());
}

#[path = "cass_error_recall_js_coverage_tests.rs"]
mod coverage;
