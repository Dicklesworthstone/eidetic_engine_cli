#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::cass_error_recall::{ToolOutput, failure_diagnostics, structured_error_diagnostics};
use serde_json::json;

fn report(kind: &str, invocation: Value) -> Value {
    json!({
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {"name": "context-analyzer", "version": "1"}},
            "invocations": [invocation],
            "results": [{
                "ruleId": "resource-leak", "kind": kind,
                "level": if kind == "fail" { "error" } else { "none" },
                "message": {"text": "Unreleased resource"},
                "locations": [{"physicalLocation": {
                    "artifactLocation": {"uri": "src/cache.rs"},
                    "region": {"startLine": 12, "endLine": 12}
                }}]
            }]
        }]
    })
}

fn invocation() -> Value {
    json!({
        "executionSuccessful": true,
        "commandLine": "analyzer --strict src/cache.rs",
        "arguments": ["--strict", "src/cache.rs"],
        "environmentVariables": {"MODE": "strict", "LANG": "C"},
        "workingDirectory": {"uri": "file:///repo/"},
        "executableLocation": {"uri": "file:///tools/analyzer"}
    })
}

fn decoded(value: &Value) -> Decoded {
    decode(&value.to_string())
        .expect("valid report")
        .expect("SARIF")
}

fn tool(value: &Value, exit: i64) -> ToolOutput {
    ToolOutput {
        text: value.to_string(),
        exit_code: Some(exit),
        is_error: None,
    }
}

#[test]
fn reported_analyzer_scope_is_required_even_when_outer_command_is_unchanged() {
    use super::super::{Proof, Requirement};

    let failure = report("fail", invocation());
    let pass = report("pass", invocation());
    let required = Requirement::capture(&tool(&failure, 1), true);
    assert!(required.is_guarded());
    assert!(required.is_verified_by(&Proof::capture(&tool(&pass, 0))));
    for (field, changed) in [
        ("commandLine", json!("analyzer --lenient src/cache.rs")),
        ("arguments", json!(["--lenient", "src/cache.rs"])),
        ("environmentVariables", json!({"MODE": "lenient", "LANG": "C"})),
        ("workingDirectory", json!({"uri": "file:///different-repo/"})),
        ("executableLocation", json!({"uri": "file:///other/analyzer"})),
    ] {
        let mut changed_invocation = invocation();
        changed_invocation[field] = changed;
        let changed = report("pass", changed_invocation);
        assert!(
            tool(&changed, 0).succeeded(),
            "still a completed analysis: {field}"
        );
        assert!(
            !required.is_verified_by(&Proof::capture(&tool(&changed, 0))),
            "not evidence for the original invocation: {field}"
        );
    }
}

#[test]
fn real_working_directory_is_supported_without_rewriting_relative_locations() {
    let failure = decoded(&report("fail", invocation()));
    let pass = decoded(&report("pass", invocation()));
    assert!(failure.execution.has_known_failures());
    assert!(pass.output.allows_completion);
    assert!(failure.execution.verified_by(&pass.execution));
    let diagnostics = failure.output.diagnostics;
    assert_eq!(
        diagnostics,
        failure_diagnostics(&tool(&report("fail", invocation()), 1))
    );
    assert_eq!(
        diagnostics,
        structured_error_diagnostics(&report("fail", invocation()).to_string())
    );
    assert!(!format!("{diagnostics:?}").contains("file:///repo"));
}

#[test]
fn argument_boundaries_and_presence_cannot_collapse_to_the_same_command() {
    for (left, right) in [
        (json!(["one two"]), json!(["one", "two"])),
        (json!([""]), json!([])),
        (json!(["a", "b"]), json!(["b", "a"])),
    ] {
        let mut before = invocation();
        before["arguments"] = left;
        let mut after = before.clone();
        after["arguments"] = right;
        let required = decoded(&report("fail", before));
        assert!(required.execution.has_known_failures());
        assert!(
            !required
                .execution
                .verified_by(&decoded(&report("pass", after)).execution)
        );
    }
    let mut missing = invocation();
    missing.as_object_mut().unwrap().remove("arguments");
    let required = decoded(&report("fail", missing.clone()));
    assert!(
        required
            .execution
            .verified_by(&decoded(&report("pass", missing.clone())).execution)
    );
    missing["arguments"] = json!([]);
    assert!(
        !required
            .execution
            .verified_by(&decoded(&report("pass", missing)).execution)
    );
}

#[test]
fn environment_order_and_runtime_timestamps_do_not_change_analysis_scope() {
    let before = report("fail", invocation());
    let mut after_invocation = invocation();
    after_invocation["environmentVariables"] =
        serde_json::from_str(r#"{"LANG":"C","MODE":"strict"}"#).unwrap();
    after_invocation["startTimeUtc"] = json!("2026-10-15T01:00:00Z");
    after_invocation["endTimeUtc"] = json!("2026-10-15T01:01:00Z");
    after_invocation["processId"] = json!(1234);
    let after = report("pass", after_invocation);
    assert!(decoded(&before).execution.verified_by(&decoded(&after).execution));
}

#[test]
fn unknown_or_unresolved_execution_metadata_retains_diagnostics_not_proof() {
    for (field, value) in [
        ("arguments", json!(null)),
        ("environmentVariables", json!(null)),
        ("commandLine", json!(null)),
        ("commandLine", json!("")),
        ("workingDirectory", json!(null)),
        ("workingDirectory", json!({"uri": "relative/root"})),
        ("workingDirectory", json!({"uri": "file:///repo/", "index": 0})),
        ("workingDirectory", json!({"uri": "file:///repo/", "uriBaseId": null})),
        ("executableLocation", json!({"uri": "analyzer", "index": null})),
        ("responseFiles", json!([{"uri": "analysis.args"}])),
        ("responseFiles", json!([{"index": 0}])),
        ("executableLocation", json!({"index": 0})),
        ("stdin", json!({"uri": "input.json"})),
    ] {
        let mut observed = invocation();
        observed[field] = value;
        let failure = decoded(&report("fail", observed.clone()));
        let pass = decoded(&report("pass", observed));
        assert_eq!(failure.output.diagnostics.len(), 1, "{field}");
        assert!(!failure.execution.has_known_failures(), "{field}");
        assert!(!pass.output.allows_completion, "{field}");
        assert!(!failure.execution.verified_by(&pass.execution), "{field}");
    }
}

#[test]
fn duplicate_execution_fields_and_escaped_environment_keys_are_not_last_wins() {
    let valid = report("pass", invocation()).to_string();
    for text in [
        valid.replacen("\"arguments\":[", "\"arguments\":[],\"arguments\":[", 1),
        valid.replacen(
            "\"MODE\":\"strict\"",
            "\"MODE\":\"lenient\",\"M\\u004fDE\":\"strict\"",
            1,
        ),
        valid.replacen(
            "\"commandLine\":",
            "\"commandLine\":\"other\",\"commandLine\":",
            1,
        ),
        valid.replacen("\"MODE\":\"strict\"", "\"MODE\":false", 1),
    ] {
        assert_ne!(text, valid, "negative fixture must change original bytes");
        assert!(decode(&text).is_err());
    }
}

#[test]
fn execution_inventory_uses_the_existing_report_budget() {
    let mut small = invocation();
    small["arguments"] = json!(vec!["x"; 8]);
    assert!(decoded(&report("pass", small)).output.allows_completion);
    let mut oversized = invocation();
    oversized["arguments"] = json!(vec!["x"; MAX_RECORDS]);
    assert!(decode(&report("pass", oversized).to_string()).is_err());
}
