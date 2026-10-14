use super::*;
use crate::core::cass_error_recall::{ToolOutput, failure_diagnostics, structured_error_diagnostics};
use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};
use serde_json::{Value, json};

fn report(outcome: &str) -> Value {
    let stage = |outcome| json!({"duration":0.001,"outcome":outcome});
    let mut test = json!({
        "nodeid":"tests/test_cache.py::test_lookup[normalized]", "lineno":4,
        "outcome":outcome, "setup":stage("passed"), "call":stage("passed"),
        "teardown":stage("passed"), "metadata":{"private":"PRIVATE_METADATA"}
    });
    let failed = matches!(outcome, "failed" | "error");
    match outcome {
        "failed" => {
            test["call"]["outcome"] = json!("failed");
            test["call"]["crash"] = json!({
                "path":"PRIVATE_SOURCE", "lineno":5,
                "message":"AssertionError: cache key was not normalized\nPRIVATE_FRAME"
            });
        }
        "error" => {
            test["setup"]["outcome"] = json!("failed");
            test["setup"]["crash"] = json!({"message":"RuntimeError: cache setup unavailable"});
            test.as_object_mut().unwrap().remove("call");
        }
        "skipped" | "xfailed" => {
            test["call"]["outcome"] = json!("skipped");
        }
        _ => {}
    }
    let mut summary = json!({"collected":1,"total":1});
    summary[outcome] = json!(1);
    json!({
        "created":1788000000.0, "duration":0.005, "exitcode":u8::from(failed),
        "root":"/workspace/project", "summary":summary, "tests":[test],
        "environment":{"private":"PRIVATE_ENVIRONMENT"}
    })
}

fn output(value: &Value) -> ToolOutput {
    ToolOutput { text: value.to_string(), is_error: Some(false), exit_code: Some(0) }
}

fn parsed(value: &Value) -> Decoded {
    decode(&value.to_string()).expect("valid pytest report").expect("pytest family")
}

fn verifies(failure: &Value, success: &Value) -> bool {
    Requirement::capture(&output(failure), true).is_verified_by(&Proof::capture(&output(success)))
}

#[test]
fn pytest_failed_call_reaches_both_cass_and_public_diagnostic_paths() {
    let value = report("failed");
    let decoded = parsed(&value);
    assert!(decoded.output.failed);
    assert!(!decoded.output.allows_completion);
    assert_eq!(decoded.output.diagnostics.len(), 1);
    assert_eq!(failure_diagnostics(&output(&value)), decoded.output.diagnostics);
    assert_eq!(structured_error_diagnostics(&value.to_string()), decoded.output.diagnostics);
    assert!(output(&value).failed());
    assert!(!output(&value).succeeded());
    let public = format!("{:?}", decoded.output.diagnostics).to_ascii_lowercase();
    assert!(public.contains("pytest call:"));
    for private in ["private_", "/workspace/project", "test_lookup", "test_cache.py"] {
        assert!(!public.contains(private));
    }
}

#[test]
fn pytest_setup_and_teardown_errors_have_distinct_reusable_fingerprints() {
    let setup = report("error");
    let mut teardown = report("passed");
    teardown["exitcode"] = json!(1);
    teardown["summary"] = json!({"collected":1,"total":1,"error":1});
    teardown["tests"][0]["outcome"] = json!("error");
    teardown["tests"][0]["teardown"] = setup["tests"][0]["setup"].clone();
    for value in [&setup, &teardown] {
        let decoded = parsed(value);
        assert!(decoded.output.failed);
        assert_eq!(decoded.output.diagnostics.len(), 1);
        assert!(verifies(value, &report("passed")));
    }
    assert_ne!(parsed(&setup).output.diagnostics, parsed(&teardown).output.diagnostics);
    // A failed teardown cannot be concealed by a passed call or optimistic exit.
    teardown["exitcode"] = json!(0);
    assert!(!output(&teardown).succeeded());
    teardown["tests"][0]["outcome"] = json!("passed");
    assert!(decode(&teardown.to_string()).is_err());
}

#[test]
fn pytest_expected_failures_skips_and_unexpected_passes_do_not_verify_repairs() {
    for outcome in ["skipped", "xfailed", "xpassed"] {
        let value = report(outcome);
        let decoded = parsed(&value);
        assert!(!decoded.output.failed);
        assert!(decoded.output.diagnostics.is_empty());
        assert!(!decoded.output.allows_completion);
        assert!(!verifies(&report("failed"), &value));
    }
    // Real pytest marks an xfailed call as skipped even when it has a crash.
    let mut xfailed = report("xfailed");
    xfailed["tests"][0]["call"]["crash"] = report("failed")["tests"][0]["call"]["crash"].clone();
    assert!(parsed(&xfailed).output.diagnostics.is_empty());
}

#[test]
fn pytest_reports_need_independent_process_completion() {
    let success = report("passed");
    assert!(output(&success).succeeded());
    assert!(!ToolOutput { text: success.to_string(), ..ToolOutput::default() }.succeeded());
    assert!(!ToolOutput { exit_code: Some(1), ..output(&success) }.succeeded());
    for exitcode in 1..=5 {
        let mut value = success.clone();
        value["exitcode"] = json!(exitcode);
        assert!(!output(&value).succeeded());
    }
    let mut optimistic = report("failed");
    optimistic["exitcode"] = json!(0);
    assert!(output(&optimistic).failed());
}

#[test]
fn pytest_summary_only_empty_and_fail_fast_runs_never_prove_coverage() {
    let failure = report("failed");
    let mut summary_only = report("passed");
    summary_only.as_object_mut().unwrap().remove("tests");
    assert!(!output(&summary_only).succeeded());
    assert!(!verifies(&failure, &summary_only));
    let mut empty = report("passed");
    empty["summary"] = json!({"collected":0,"total":0});
    empty["tests"] = json!([]);
    assert!(!output(&empty).succeeded());
    let mut stopped = failure.clone();
    stopped["summary"]["collected"] = json!(2);
    assert_eq!(parsed(&stopped).output.diagnostics.len(), 1);
    assert!(!verifies(&stopped, &report("passed")));
    let mut incomplete = failure.clone();
    incomplete["tests"][0].as_object_mut().unwrap().remove("teardown");
    assert!(!verifies(&incomplete, &report("passed")));
}

#[test]
fn pytest_collection_errors_veto_an_otherwise_clean_retry() {
    let mut value = report("passed");
    value["collectors"] = json!([
        {"nodeid":"","outcome":"passed"},
        {"nodeid":"tests/test_other.py","outcome":"failed","longrepr":"PRIVATE_TRACEBACK"}
    ]);
    let decoded = parsed(&value);
    assert!(decoded.output.failed);
    assert_eq!(decoded.output.diagnostics.len(), 1);
    assert!(!output(&value).succeeded());
    assert!(!verifies(&report("failed"), &value));
    assert!(!format!("{:?}", decoded.output.diagnostics).to_lowercase().contains("private_traceback"));
}

#[test]
fn pytest_retry_binds_root_node_ids_parameters_and_complete_selected_inventory() {
    let failure = report("failed");
    assert!(verifies(&failure, &report("passed")));
    for (pointer, replacement) in [
        ("/root", json!("/workspace/other")),
        ("/tests/0/nodeid", json!("tests/test_cache.py::test_lookup[raw]")),
        ("/tests/0/nodeid", json!("tests/Test_cache.py::test_lookup[normalized]")),
        ("/tests/0/nodeid", json!("tests/test_other.py::test_lookup[normalized]")),
    ] {
        let mut success = report("passed");
        *success.pointer_mut(pointer).unwrap() = replacement;
        assert!(output(&success).succeeded());
        assert!(!verifies(&failure, &success));
    }
    let mut changed = report("passed");
    changed["created"] = json!(1900000000);
    changed["duration"] = json!(99);
    assert!(verifies(&failure, &changed));
}

#[test]
fn pytest_all_failed_subjects_remain_guarded_beyond_the_display_cap() {
    let many = |failed: bool, skip_last: bool| {
        let mut value = report(if failed { "failed" } else { "passed" });
        let tests = (0..6).map(|index| {
            let status = if failed { "failed" } else if skip_last && index == 5 { "skipped" } else { "passed" };
            let mut test = report(status)["tests"][0].clone();
            test["nodeid"] = json!(format!("tests/test_cache.py::test_lookup[{index}]"));
            test
        }).collect::<Vec<_>>();
        value["tests"] = json!(tests);
        value["summary"] = if failed { json!({"collected":6,"total":6,"failed":6}) }
            else if skip_last { json!({"collected":6,"total":6,"passed":5,"skipped":1}) }
            else { json!({"collected":6,"total":6,"passed":6}) };
        value
    };
    let failure = many(true, false);
    assert!(parsed(&failure).output.diagnostics.len() <= MAX_DIAGNOSTICS_PER_FAILURE);
    assert!(verifies(&failure, &many(false, false)));
    assert!(!verifies(&failure, &many(false, true)));
    let mut later_bad = failure.clone();
    later_bad["tests"][5]["setup"]["duration"] = json!(-1);
    assert!(decode(&later_bad.to_string()).is_err());
    let mut reordered = many(false, false);
    reordered["tests"].as_array_mut().unwrap().reverse();
    assert!(verifies(&failure, &reordered));
    assert!(!verifies(&failure, &report("passed")));
}

#[test]
fn pytest_rejects_counter_and_stage_contradictions() {
    for (pointer, replacement) in [
        ("/summary/failed", json!(0)),
        ("/summary/collected", json!(0)),
        ("/summary/total", json!(2)),
        ("/exitcode", json!(6)),
        ("/created", json!(-1)),
        ("/tests/0/nodeid", json!("")),
        ("/tests/0/call/outcome", json!("running")),
        ("/tests/0/setup/outcome", json!("failed")),
        ("/tests/0/outcome", json!("passed")),
        ("/tests/0/call/crash/message", json!("")),
    ] {
        let mut value = report("failed");
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(decode(&value.to_string()).is_err(), "{pointer}");
        assert!(!output(&value).succeeded());
        assert!(structured_error_diagnostics(&value.to_string()).is_empty());
    }
    let mut overflow = report("failed");
    overflow["summary"]["passed"] = json!(u64::MAX);
    assert!(decode(&overflow.to_string()).is_err());
    let mut duplicate = report("failed");
    let row = duplicate["tests"][0].clone();
    duplicate["tests"].as_array_mut().unwrap().push(row);
    duplicate["summary"] = json!({"collected":2,"total":2,"failed":2});
    assert!(decode(&duplicate.to_string()).is_err());
}

#[test]
fn pytest_duplicate_fields_and_trailing_data_never_salvage_a_valid_prefix() {
    let valid = report("passed").to_string();
    for text in [
        valid.replacen("\"exitcode\":0", "\"exitcode\":1,\"exitcode\":0", 1),
        valid.replacen("\"outcome\":\"passed\"", "\"outcome\":\"failed\",\"outcome\":\"passed\"", 1),
        format!("{valid} {valid}"),
        format!("{valid}\noutput truncated"),
        valid[..valid.len() - 1].to_owned(),
        "{\"exitcode\":null,\"tests\":null}".to_owned(),
    ] {
        assert!(decode(&text).is_err());
        assert!(!ToolOutput { text, ..output(&report("passed")) }.succeeded());
    }
}

#[test]
fn pytest_learning_keeps_privacy_boundaries_and_source_sized_limits() {
    let mut value = report("failed");
    let secret = format!("ghp_{}", "Q".repeat(36));
    value["tests"][0]["call"]["crash"]["message"] = json!(format!("api_key={secret}"));
    let diagnostics = parsed(&value).output.diagnostics;
    assert!(!format!("{diagnostics:?}").to_lowercase().contains(&secret.to_lowercase()));
    let text = value.to_string();
    let exact = format!("{text}{}", " ".repeat(MAX_SCANNED_OUTPUT_BYTES - text.len()));
    assert!(decode(&exact).is_ok());
    assert!(decode(&format!("{exact} ")).is_err());
    for field in ["schema", "reason", "$message_type", "testResults", "generalDiagnostics", "results"] {
        let mut mixed = report("passed");
        mixed[field] = Value::Null;
        assert!(decode(&mixed.to_string()).is_err());
    }
    for value in [json!({"example":report("failed")}), json!({"schema":"ee.response.v2","success":true})] {
        assert!(decode(&value.to_string()).unwrap().is_none());
    }
}

#[test]
fn pytest_partial_poll_requirements_cannot_downgrade_to_generic_command_success() {
    let failed = output(&report("failed"));
    let proof = Proof::capture(&output(&report("passed")));
    let mut requirement = Requirement::capture(&failed, false);
    assert!(requirement.is_guarded());
    assert!(!requirement.is_verified_by(&proof));
    requirement.refresh(&failed, true);
    assert!(requirement.is_verified_by(&proof));
    for text in ["{}", "unrelated output", "{"] {
        requirement.refresh(&ToolOutput { text:text.into(), ..ToolOutput::default() }, true);
        assert!(requirement.is_guarded());
        assert!(!requirement.is_verified_by(&proof));
    }
}
