//! Growing named suites through the production proof and migrated-store paths.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::cass_error_recall::test_coverage::javascript::decode;

fn recount(value: &mut Value) {
    let mut counts = [0_u64; 4];
    let mut failed_files = 0_u64;
    for file in value["testResults"].as_array_mut().unwrap() {
        let mut failed = false;
        for assertion in file["assertionResults"].as_array().unwrap() {
            let category = match assertion["status"].as_str().unwrap() {
                "passed" => 0,
                "failed" => 1,
                "pending" | "skipped" | "disabled" => 2,
                "todo" => 3,
                _ => panic!("invalid test fixture status"),
            };
            counts[category] += 1;
            failed |= category == 1;
        }
        failed_files += u64::from(failed);
        file["status"] = json!(if failed { "failed" } else { "passed" });
    }
    let files = u64::try_from(value["testResults"].as_array().unwrap().len()).unwrap();
    value["numTotalTestSuites"] = json!(files);
    value["numPassedTestSuites"] = json!(files - failed_files);
    value["numFailedTestSuites"] = json!(failed_files);
    value["numTotalTests"] = json!(counts.iter().sum::<u64>());
    for (field, count) in [
        "numPassedTests",
        "numFailedTests",
        "numPendingTests",
        "numTodoTests",
    ]
    .into_iter()
    .zip(counts)
    {
        value[field] = json!(count);
    }
    value["success"] = json!(failed_files == 0);
}

fn add_test(value: &mut Value, title: &str, status: &str) {
    let mut assertion = report("passed")["testResults"][0]["assertionResults"][0].clone();
    assertion["title"] = json!(title);
    assertion["fullName"] = json!(format!("cache {title}"));
    assertion["status"] = json!(status);
    if status == "failed" {
        assertion["failureMessages"] = json!([format!("Invalid {title} result")]);
    }
    value["testResults"][0]["assertionResults"]
        .as_array_mut()
        .unwrap()
        .push(assertion);
    recount(value);
}

fn growth() -> Value {
    let mut value = two_tests("passed");
    add_test(&mut value, "normalizes cache keys", "passed");
    value
}

fn execution(value: &Value) -> super::super::super::Execution {
    decode(&value.to_string()).unwrap().unwrap().execution
}

#[test]
fn javascript_growth_keeps_original_subjects_across_files_and_nested_suite_counts() {
    let failure = two_tests("failed");
    let mut proof = growth();
    let mut new_file = report("passed")["testResults"][0].clone();
    new_file["name"] = json!("/private/project/regression.test.ts");
    proof["testResults"].as_array_mut().unwrap().push(new_file);
    recount(&mut proof);
    assert!(verifies(failure.clone(), proof.clone()));
    proof["testResults"].as_array_mut().unwrap().reverse();
    for file in proof["testResults"].as_array_mut().unwrap() {
        file["assertionResults"].as_array_mut().unwrap().reverse();
        file["startTime"] = json!(8001);
        file["endTime"] = json!(8002);
    }
    // Vitest's nested suite count is not the number of file rows.
    proof["numTotalTestSuites"] = json!(6);
    proof["numPassedTestSuites"] = json!(6);
    assert!(verifies(failure, proof));
}

#[test]
fn javascript_growth_cannot_skip_previously_passing_controls() {
    for with_growth in [false, true] {
        for status in ["pending", "skipped", "disabled", "todo"] {
            let mut proof = if with_growth {
                growth()
            } else {
                two_tests("passed")
            };
            proof["testResults"][0]["assertionResults"][1]["status"] = json!(status);
            recount(&mut proof);
            assert!(output(proof.clone()).succeeded(), "other assertions passed");
            assert!(
                !verifies(two_tests("failed"), proof),
                "{status}, growth={with_growth}"
            );
        }
    }
}

#[test]
fn javascript_original_nonexecuted_subjects_are_retained_not_promoted_to_required_passes() {
    for status in ["pending", "skipped", "disabled", "todo"] {
        let mut failure = two_tests("failed");
        add_test(&mut failure, "future coverage", status);
        let mut proof = growth();
        add_test(&mut proof, "future coverage", status);
        assert!(verifies(failure.clone(), proof));
        assert!(
            !verifies(failure, growth()),
            "nonexecuted inventory disappeared"
        );
    }
}

#[test]
fn javascript_growth_never_hides_new_failures_or_report_level_vetoes() {
    let failure = two_tests("failed");
    let required = execution(&failure);
    let mut new_failure = growth();
    add_test(&mut new_failure, "new regression fails", "failed");
    assert!(!required.verified_by(&execution(&new_failure)));
    assert!(!verifies(failure.clone(), new_failure));
    for (key, veto) in [
        ("success", json!(false)),
        ("wasInterrupted", json!(true)),
        ("snapshot", json!({"failure": true})),
    ] {
        let mut proof = growth();
        proof[key] = veto;
        assert!(!required.verified_by(&execution(&proof)), "{key}");
        assert!(!verifies(failure.clone(), proof));
    }
    assert!(required.verified_by(&execution(&growth())));
}

#[test]
fn javascript_growth_cannot_substitute_file_case_ancestor_boundaries_or_controls() {
    let failure = two_tests("failed");
    let mut renamed_file = growth();
    renamed_file["testResults"][0]["name"] = json!("/private/project/Cache.test.ts");
    let mut alias = growth();
    alias["testResults"][0]["assertionResults"][0]["ancestorTitles"] = json!(["cache returns"]);
    alias["testResults"][0]["assertionResults"][0]["title"] = json!("cached value");
    let mut dropped = growth();
    dropped["testResults"][0]["assertionResults"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    recount(&mut dropped);
    for proof in [renamed_file, alias, dropped] {
        assert!(output(proof.clone()).succeeded());
        assert!(!verifies(failure.clone(), proof));
    }
    // Even an empty originally reported file remains part of the inventory.
    let mut failure = failure;
    let mut empty_file = report("passed")["testResults"][0].clone();
    empty_file["name"] = json!("/private/project/empty.test.ts");
    empty_file["assertionResults"] = json!([]);
    failure["testResults"]
        .as_array_mut()
        .unwrap()
        .push(empty_file.clone());
    recount(&mut failure);
    assert!(!verifies(failure.clone(), growth()));
    let mut proof = growth();
    proof["testResults"]
        .as_array_mut()
        .unwrap()
        .push(empty_file);
    recount(&mut proof);
    assert!(verifies(failure, proof));
}

#[test]
fn javascript_growth_keeps_executed_controls_beyond_the_diagnostic_cap() {
    let mut failure = two_tests("failed");
    let mut proof = growth();
    for name in ["alpha", "beta", "gamma", "delta", "epsilon"] {
        add_test(&mut failure, name, "failed");
        add_test(&mut proof, name, "passed");
    }
    add_test(&mut failure, "late control", "passed");
    add_test(&mut proof, "late control", "passed");
    assert!(execution(&failure).has_known_failures());
    assert!(structured_error_diagnostics(&failure.to_string()).len() <= 4);
    assert!(verifies(failure.clone(), proof.clone()));
    proof["testResults"][0]["assertionResults"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["status"] = json!("todo");
    recount(&mut proof);
    assert!(output(proof.clone()).succeeded());
    assert!(!verifies(failure, proof));
}

#[test]
fn javascript_growth_still_requires_complete_processes_and_undamaged_documents() {
    let failure = output(two_tests("failed"));
    let proof = output(growth());
    let mut required = Requirement::capture(&failure, false);
    assert!(!required.is_verified_by(&Proof::capture(&proof)));
    required.refresh(&failure, true);
    assert!(required.is_verified_by(&Proof::capture(&proof)));
    for text in [
        format!("{}\n{{", proof.text),
        format!("{} trailing", proof.text),
    ] {
        let damaged = ToolOutput {
            text,
            ..proof.clone()
        };
        assert!(!required.is_verified_by(&Proof::capture(&damaged)));
    }
    let no_exit = ToolOutput {
        text: proof.text.clone(),
        ..ToolOutput::default()
    };
    assert!(!required.is_verified_by(&Proof::capture(&no_exit)));
    let failed_exit = ToolOutput {
        exit_code: Some(1),
        ..proof.clone()
    };
    assert!(!required.is_verified_by(&Proof::capture(&failed_exit)));
    required.refresh(
        &ToolOutput {
            text: "{}".to_owned(),
            ..failure
        },
        true,
    );
    assert!(required.is_guarded());
    assert!(!required.is_verified_by(&Proof::capture(&proof)));
}

#[test]
fn javascript_growth_keeps_the_complete_utf8_byte_bound() {
    let failure = output(two_tests("failed"));
    let required = Requirement::capture(&failure, true);
    let mut value = growth();
    value["metadata"] = json!({"note": "é🦀"});
    let text = value.to_string();
    let cap = crate::core::cass_error_recall::MAX_SCANNED_OUTPUT_BYTES;
    let exact = format!("{text}{}", " ".repeat(cap - text.len()));
    let proof = ToolOutput {
        text: exact.clone(),
        exit_code: Some(0),
        is_error: Some(false),
    };
    assert!(required.is_verified_by(&Proof::capture(&proof)));
    for suffix in [" ", "é"] {
        let over = ToolOutput {
            text: format!("{exact}{suffix}"),
            ..proof.clone()
        };
        assert!(!required.is_verified_by(&Proof::capture(&over)));
    }
}

#[test]
fn javascript_growth_records_cited_repairs_and_pack_context_without_changing_sources() -> TestResult
{
    for command in [COMMAND, "jest --json --runInBand"] {
        let failure = two_tests("failed").to_string();
        let case = episode(command, &failure, command, &growth().to_string())?;
        let before = case.db.list_evidence_spans_for_session(&case.session)?;
        let diagnostic = structured_error_diagnostics(&failure).remove(0);
        let report = record_session_error_recall(&case.db, WS, &case.session)?;
        assert_eq!(report.resolved_failures, 1);
        assert_eq!(report.repair_links_recorded, 2);
        assert_eq!(report.incident_cards_recorded, 1);
        let recall = error_recall_report(&case.db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.contains(&case.repair));
        assert_eq!(recall.proof_links, vec![case.proof]);
        let card = crate::core::incident_card::incident_card_id(WS, &case.failure);
        assert!(
            case.db
                .get_search_admitted_evidence_span(&card, WS)?
                .is_some()
        );
        assert!(
            pack_error_recall_query_seed(&case.db, WS, &diagnostic)?.contains("normalized key")
        );
        for source in before {
            let after = case
                .db
                .get_evidence_span(&source.id)?
                .ok_or("source disappeared")?;
            assert_eq!(after.excerpt, source.excerpt);
            assert_eq!(after.content_hash, source.content_hash);
            assert_eq!(
                after.canonical_provenance_uri(),
                source.canonical_provenance_uri()
            );
        }
        assert_eq!(
            record_session_error_recall(&case.db, WS, &case.session)?.incident_cards_recorded,
            0
        );
    }
    Ok(())
}

#[test]
fn javascript_nonexecuted_controls_never_create_durable_repairs_or_cards() -> TestResult {
    let failure = two_tests("failed").to_string();
    for with_growth in [false, true] {
        for status in ["pending", "skipped", "disabled", "todo"] {
            let mut proof = if with_growth {
                growth()
            } else {
                two_tests("passed")
            };
            proof["testResults"][0]["assertionResults"][1]["status"] = json!(status);
            recount(&mut proof);
            let case = episode(COMMAND, &failure, COMMAND, &proof.to_string())?;
            let report = record_session_error_recall(&case.db, WS, &case.session)?;
            assert_eq!(report.failures_seen, 1);
            assert_eq!(report.resolved_failures, 0);
            assert_eq!(report.repair_links_recorded, 0);
            assert_eq!(report.incident_cards_recorded, 0);
        }
    }
    let case = episode(
        COMMAND,
        &failure,
        "vitest run --reporter=json --testNamePattern=cache",
        &growth().to_string(),
    )?;
    assert_eq!(
        record_session_error_recall(&case.db, WS, &case.session)?.resolved_failures,
        0
    );
    Ok(())
}
