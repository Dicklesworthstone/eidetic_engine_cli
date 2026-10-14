#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::cass_error_recall::{
    ToolOutput, failure_diagnostics, structured_error_diagnostics,
};
use serde_json::json;

fn finding(kind: &str, rule: &str, uri: &str, line: u64) -> Value {
    json!({
        "ruleId": rule,
        "kind": kind,
        "level": if kind == "fail" { "error" } else { "none" },
        "message": {"text": "Unreleased resource in `acquire`"},
        "locations": [{"physicalLocation": {
            "artifactLocation": {"uri": uri},
            "region": {"startLine": line, "endLine": line}
        }}]
    })
}

fn report(results: Vec<Value>) -> Value {
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {
                "name": "fixture-analyzer", "version": "1.0.0",
                "rules": [{"id": "resource-leak", "defaultConfiguration": {"level": "error"}}]
            }},
            "invocations": [{"executionSuccessful": true}],
            "results": results
        }]
    })
}

fn failed() -> Value {
    report(vec![finding("fail", "resource-leak", "src/cache.rs", 12)])
}

fn passed() -> Value {
    report(vec![finding("pass", "resource-leak", "src/cache.rs", 12)])
}

fn decoded(value: &Value) -> Decoded {
    decode(&value.to_string())
        .expect("valid SARIF subset")
        .expect("SARIF report")
}

fn output(value: &Value, exit_code: Option<i64>) -> ToolOutput {
    ToolOutput {
        text: value.to_string(),
        is_error: None,
        exit_code,
    }
}

#[test]
fn shared_reader_and_import_use_the_same_exact_sarif_class() {
    let value = failed();
    let parsed = decoded(&value);
    assert!(parsed.output.failed);
    assert!(!parsed.output.allows_completion);
    assert!(parsed.execution.has_known_failures());
    assert_eq!(parsed.output.diagnostics.len(), 1);
    let expected = &parsed.output.diagnostics;
    assert_eq!(&failure_diagnostics(&output(&value, Some(1))), expected);
    assert_eq!(&structured_error_diagnostics(&value.to_string()), expected);
    assert!(expected[0].layered_key().key.starts_with("shell:sarif:"));
    assert!(!expected[0].message_template.contains("acquire"));
}

#[test]
fn rule_identity_survives_message_path_line_and_version_changes() {
    let original = decoded(&failed()).output.diagnostics.remove(0);
    let mut changed = failed();
    changed["runs"][0]["tool"]["driver"]["version"] = json!("2.0.0");
    changed["runs"][0]["results"][0]["message"]["text"] = json!("Changed diagnostic wording");
    changed["runs"][0]["results"][0]["locations"][0]["physicalLocation"] = json!({
        "artifactLocation": {"uri": "another/private.rs"},
        "region": {"startLine": 900, "endLine": 901}
    });
    assert_eq!(
        original.layered_key(),
        decoded(&changed).output.diagnostics[0].layered_key()
    );
    for (pointer, value) in [
        ("/runs/0/tool/driver/name", "different-tool"),
        ("/runs/0/results/0/ruleId", "different-rule"),
    ] {
        let mut changed = failed();
        *changed.pointer_mut(pointer).unwrap() = json!(value);
        assert_ne!(
            original.layered_key(),
            decoded(&changed).output.diagnostics[0].layered_key()
        );
    }
}

#[test]
fn rule_index_default_severity_and_nonfailure_defaults_are_resolved() {
    let mut value = failed();
    let row = &mut value["runs"][0]["results"][0];
    row.as_object_mut().unwrap().remove("ruleId");
    row.as_object_mut().unwrap().remove("level");
    row["ruleIndex"] = json!(0);
    assert_eq!(
        decoded(&value).output.diagnostics,
        decoded(&failed()).output.diagnostics
    );
    value["runs"][0]["results"][0]["kind"] = json!("pass");
    let pass = decoded(&value);
    assert!(!pass.output.failed);
    assert!(pass.output.allows_completion);
    assert!(decoded(&failed()).execution.verified_by(&pass.execution));
}

#[test]
fn only_explicit_complete_same_subject_pass_can_verify_a_repair() {
    use super::super::{Proof, Requirement};

    let failed = output(&failed(), Some(1));
    let passed = output(&passed(), Some(0));
    assert!(failed.failed());
    assert!(passed.succeeded());
    let required = Requirement::capture(&failed, true);
    assert!(required.is_guarded());
    assert!(required.is_verified_by(&Proof::capture(&passed)));
    assert!(!Requirement::capture(&failed, false).is_verified_by(&Proof::capture(&passed)));
    for exit in [None, Some(1)] {
        let mut incomplete = passed.clone();
        incomplete.exit_code = exit;
        assert!(!required.is_verified_by(&Proof::capture(&incomplete)));
    }
    let mut refreshing = Requirement::capture(&failed, true);
    refreshing.refresh(&output(&report(Vec::new()), Some(0)), true);
    assert!(!refreshing.is_verified_by(&Proof::capture(&passed)));
}

#[test]
fn omission_suppression_and_baseline_absence_never_mean_fixed() {
    let required = decoded(&failed()).execution;
    let mut variants = vec![report(Vec::new())];
    for (field, value) in [
        ("suppressions", json!([{"kind": "external", "status": "accepted"}])),
        ("baselineState", json!("absent")),
        ("kind", json!("notApplicable")),
        ("kind", json!("open")),
        ("kind", json!("review")),
    ] {
        let mut value_report = passed();
        value_report["runs"][0]["results"][0][field] = value;
        variants.push(value_report);
    }
    for value in variants {
        let parsed = decoded(&value);
        assert!(!required.verified_by(&parsed.execution));
        assert!(parsed.output.diagnostics.is_empty());
    }
    for field in ["suppressions", "baselineState"] {
        let mut value = failed();
        value["runs"][0]["results"][0][field] = if field == "suppressions" {
            json!([{"kind": "inSource"}])
        } else {
            json!("absent")
        };
        assert!(decoded(&value).output.diagnostics.is_empty());
    }
}

#[test]
fn warnings_notes_and_nested_examples_are_not_error_fingerprints() {
    for level in ["warning", "note", "none"] {
        let mut value = failed();
        value["runs"][0]["results"][0]["level"] = json!(level);
        assert!(decoded(&value).output.diagnostics.is_empty());
    }
    let mut value = passed();
    value["runs"][0]["tool"]["driver"]["rules"][0]["fullDescription"] =
        json!({"text": "error[E0277]: DO_NOT_EXTRACT_DESCRIPTION"});
    value["runs"][0]["results"][0]["fixes"] =
        json!([{"description": {"text": "DO_NOT_EXTRACT_FIX"}}]);
    value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["snippet"] =
        json!({"text": "error[E0308]: DO_NOT_EXTRACT_SOURCE"});
    assert!(failure_diagnostics(&output(&value, Some(0))).is_empty());
    let mut value = failed();
    value["runs"][0]["tool"]["driver"]["name"] = json!("PRIVATE_PRODUCER_SENTINEL");
    value["runs"][0]["results"][0]["ruleId"] = json!("PRIVATE_RULE_SENTINEL");
    let diagnostic = decoded(&value).output.diagnostics.remove(0);
    assert!(!format!("{diagnostic:?}").contains("PRIVATE_"));
}

#[test]
fn changed_scope_and_unresolved_locations_withhold_proof() {
    let required = decoded(&failed()).execution;
    for (pointer, value) in [
        ("/runs/0/tool/driver/name", json!("other-analyzer")),
        ("/runs/0/tool/driver/version", json!("1.0.1")),
        ("/runs/0/results/0/ruleId", json!("other-rule")),
        ("/runs/0/results/0/locations/0/physicalLocation/artifactLocation/uri", json!("other/cache.rs")),
        ("/runs/0/results/0/locations/0/physicalLocation/region/startLine", json!(11)),
    ] {
        let mut value_report = passed();
        *value_report.pointer_mut(pointer).unwrap() = value;
        assert!(!required.verified_by(&decoded(&value_report).execution));
    }
    let mut unresolved = passed();
    unresolved["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
        ["uriBaseId"] = json!("%SRCROOT%");
    assert!(!required.verified_by(&decoded(&unresolved).execution));
    unresolved = passed();
    unresolved["runs"][0]["originalUriBaseIds"] = json!({});
    assert!(!required.verified_by(&decoded(&unresolved).execution));
    unresolved = passed();
    unresolved["runs"][0]["results"][0]["locations"] = json!([]);
    assert!(!required.verified_by(&decoded(&unresolved).execution));
}

#[test]
fn every_failed_subject_must_be_rechecked_even_beyond_diagnostic_cap() {
    let failures: Vec<_> = (0..6)
        .map(|index| finding("fail", &format!("rule-{index}"), "src/cache.rs", index + 1))
        .collect();
    let required = decoded(&report(failures));
    assert_eq!(required.output.diagnostics.len(), MAX_DIAGNOSTICS_PER_FAILURE);
    let mut passes: Vec<_> = (0..6)
        .map(|index| finding("pass", &format!("rule-{index}"), "src/cache.rs", index + 1))
        .collect();
    assert!(required.execution.verified_by(&decoded(&report(passes.clone())).execution));
    passes.pop();
    assert!(!required.execution.verified_by(&decoded(&report(passes)).execution));
}

#[test]
fn diagnostics_are_deterministic_under_duplicates_and_result_reordering() {
    let mut rows = vec![
        finding("fail", "b", "b.rs", 2),
        finding("fail", "a", "a.rs", 1),
        finding("fail", "b", "c.rs", 3),
    ];
    rows[2]["message"]["text"] = json!("A different wording");
    let expected = decoded(&report(rows.clone())).output.diagnostics;
    assert_eq!(expected.len(), 2);
    rows.reverse();
    assert_eq!(decoded(&report(rows)).output.diagnostics, expected);
}

#[test]
fn unsuccessful_or_missing_invocations_and_notifications_veto_completion() {
    let required = decoded(&failed()).execution;
    for invocations in [
        json!([]),
        json!(null),
        json!([{"executionSuccessful": false}]),
        json!([{"executionSuccessful": true, "toolExecutionNotifications": [{"level": "error"}]}]),
        json!([{"executionSuccessful": true, "toolConfigurationNotifications": [{"level": "error"}]}]),
    ] {
        let mut value = passed();
        value["runs"][0]["invocations"] = invocations;
        let parsed = decoded(&value);
        assert!(!parsed.output.allows_completion);
        assert!(!required.verified_by(&parsed.execution));
    }
    let mut value = passed();
    value["runs"][0].as_object_mut().unwrap().remove("invocations");
    assert!(!decoded(&value).output.allows_completion);
}

#[test]
fn malformed_or_unsupported_reports_cannot_salvage_a_valid_prefix() {
    let valid = failed().to_string();
    for text in [
        format!("{valid}\n{{}}"),
        format!("{valid}\n{{"),
        valid.replacen("\"version\":\"2.1.0\"", "\"version\":\"2.1.0\",\"version\":\"2.1.0\"", 1),
        valid.replacen("\"level\":\"error\"", "\"level\":\"error\",\"level\":\"none\"", 1),
    ] {
        assert!(decode(&text).is_err());
        assert!(structured_error_diagnostics(&text).is_empty());
    }
    for value in [json!(null), json!(99)] {
        let mut malformed = failed();
        malformed["runs"][0]["results"][0]["ruleIndex"] = value;
        if malformed["runs"][0]["results"][0]["ruleIndex"].is_null() {
            // Null optional index is absent, while a claimed external rule
            // reference (including null) is unsupported and cannot fall back.
            malformed["runs"][0]["results"][0]["rule"] = json!(null);
        }
        assert!(decode(&malformed.to_string()).is_err());
    }
    let mut mismatch = failed();
    mismatch["runs"][0]["results"][0]["ruleIndex"] = json!(0);
    mismatch["runs"][0]["results"][0]["ruleId"] = json!("different-rule");
    assert!(decode(&mismatch.to_string()).is_err());
    for field in ["externalPropertyFileReferences", "policies"] {
        let mut external = failed();
        external["runs"][0][field] = if field == "policies" { json!([{}]) } else { json!({}) };
        assert!(decode(&external.to_string()).is_err());
    }
    let mut override_report = failed();
    override_report["runs"][0]["invocations"][0]["ruleConfigurationOverrides"] = json!([{}]);
    assert!(decode(&override_report.to_string()).is_err());
}

#[test]
fn bounded_input_and_late_invalid_records_fail_as_a_whole() {
    let valid = failed().to_string();
    let padded = format!("{valid:width$}", width = MAX_SCANNED_OUTPUT_BYTES);
    assert!(decode(&padded).is_ok());
    assert!(decode(&format!("{padded} ")).is_err());
    let too_many = report(vec![finding("fail", "resource-leak", "src/cache.rs", 1); MAX_RECORDS]);
    assert!(decode(&too_many.to_string()).is_err());
    let mut rows: Vec<_> = (0..6)
        .map(|index| finding("fail", &format!("rule-{index}"), "src/cache.rs", index + 1))
        .collect();
    rows[5]["level"] = json!("invalid");
    assert!(decode(&report(rows).to_string()).is_err());
    assert!(decode(r#"{"schema":"some.other.report","results":[]}"#).unwrap().is_none());
}

#[test]
fn secondary_coordinates_are_not_discarded_when_matching_subjects() {
    let mut failure = failed();
    failure["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"] = json!({
        "startLine": 12, "endLine": 12, "byteOffset": 100, "byteLength": 10
    });
    let required = decoded(&failure).execution;
    let mut proof = failure.clone();
    proof["runs"][0]["results"][0]["kind"] = json!("pass");
    proof["runs"][0]["results"][0]["level"] = json!("none");
    assert!(required.verified_by(&decoded(&proof).execution));
    proof["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["startLine"] = json!(11);
    assert!(!required.verified_by(&decoded(&proof).execution));
    proof["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["byteOffset"] = json!(u64::MAX);
    assert!(!required.verified_by(&decoded(&proof).execution));
}
