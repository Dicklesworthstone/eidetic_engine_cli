//! Preserve normally executed Python controls, not merely listed node IDs.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use crate::core::cass_error_recall::test_coverage::{Proof, Requirement, python};
use crate::core::cass_error_recall::{
    ToolOutput, record_session_error_recall, structured_error_diagnostics,
};
use crate::core::error_diagnosis::{error_recall_report, pack_error_recall_query_seed};
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug)]
enum Format {
    Json,
    Pytest,
    Unittest,
}

const FORMATS: [Format; 3] = [Format::Json, Format::Pytest, Format::Unittest];
const FAILED: [(&str, &str); 2] = [("test_cache", "failed"), ("test_control", "passed")];
const PASSED: [(&str, &str); 2] = [("test_cache", "passed"), ("test_control", "passed")];

impl Format {
    fn command(self) -> &'static str {
        match self {
            Self::Json => "pytest --json-report",
            Self::Pytest => "pytest -v",
            Self::Unittest => "python -m unittest -v",
        }
    }

    fn render(self, rows: &[(&str, &str)]) -> String {
        match self {
            Self::Json => json_report(rows).to_string(),
            Self::Pytest => pytest_report(rows),
            Self::Unittest => unittest_report(rows),
        }
    }

    fn directly_verifies(self, failure: &str, proof: &str) -> bool {
        match self {
            Self::Json => super::decode(failure)
                .unwrap()
                .unwrap()
                .execution
                .verified_by(&super::decode(proof).unwrap().unwrap().execution),
            Self::Pytest | Self::Unittest => python::decode(failure)
                .unwrap()
                .unwrap()
                .execution
                .verified_by(&python::decode(proof).unwrap().unwrap().execution),
        }
    }
}

fn json_report(rows: &[(&str, &str)]) -> Value {
    let count = |status: &str| rows.iter().filter(|(_, value)| *value == status).count();
    let failed = count("failed") > 0;
    let tests = rows
        .iter()
        .map(|(name, outcome)| {
            let mut call = json!({"duration": 0.01, "outcome": "passed"});
            match *outcome {
                "failed" => {
                    call["outcome"] = json!("failed");
                    call["crash"] = json!({"message": "AssertionError: cache mismatch"});
                }
                "skipped" | "xfailed" => call["outcome"] = json!("skipped"),
                "passed" | "xpassed" => {}
                _ => panic!("invalid fixture outcome"),
            }
            json!({
                "nodeid": format!("tests/test_cache.py::{name}"), "outcome": outcome,
                "setup": {"duration": 0.01, "outcome": "passed"},
                "call": call, "teardown": {"duration": 0.01, "outcome": "passed"},
            })
        })
        .collect::<Vec<_>>();
    json!({
        "created": 1, "duration": 0.1, "exitcode": if failed { 1 } else { 0 },
        "root": "/workspace", "tests": tests, "collectors": [],
        "summary": {"collected": rows.len(), "total": rows.len(),
            "passed": count("passed"), "failed": count("failed"),
            "skipped": count("skipped"), "xfailed": count("xfailed"), "xpassed": count("xpassed")},
    })
}

fn pytest_report(rows: &[(&str, &str)]) -> String {
    let mut text = format!(
        "================ test session starts ================\ncollected {} items\n\n",
        rows.len()
    );
    let mut counts = BTreeMap::<&str, u64>::new();
    for (name, status) in rows {
        let label = match *status {
            "passed" => "PASSED",
            "failed" => "FAILED",
            "skipped" => "SKIPPED",
            "xfailed" => "XFAIL",
            "xpassed" => "XPASS",
            _ => panic!("fixture status"),
        };
        text.push_str(&format!("tests/test_cache.py::{name} {label}\n"));
        *counts.entry(status).or_default() += 1;
    }
    text.push_str("================ short test summary info ================\n");
    for (name, status) in rows {
        if *status == "failed" {
            text.push_str(&format!(
                "FAILED tests/test_cache.py::{name} - AssertionError: cache mismatch\n"
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

fn unittest_report(rows: &[(&str, &str)]) -> String {
    let mut text = String::new();
    let mut counts = BTreeMap::<&str, u64>::new();
    for (name, status) in rows {
        let (label, counter) = match *status {
            "passed" => ("ok", None),
            "failed" => ("FAIL", Some("failures")),
            "skipped" => ("skipped 'not exercised'", Some("skipped")),
            "xfailed" => ("expected failure", Some("expected failures")),
            "xpassed" => ("unexpected success", Some("unexpected successes")),
            _ => panic!("fixture status"),
        };
        text.push_str(&format!(
            "{name} (tests.test_cache.Cache.{name}) ... {label}\n"
        ));
        if let Some(counter) = counter {
            *counts.entry(counter).or_default() += 1;
        }
    }
    let failed = counts.contains_key("failures") || counts.contains_key("unexpected successes");
    text.push_str(&format!("\n----------------------------------------------------------------------\nRan {} tests in 0.001s\n\n{}", rows.len(), if failed { "FAILED" } else { "OK" }));
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

fn output(text: String, exit: i64) -> ToolOutput {
    ToolOutput {
        text,
        exit_code: Some(exit),
        is_error: Some(false),
    }
}

fn verifies(failure: &str, proof: &str) -> bool {
    let required = Requirement::capture(&output(failure.to_owned(), 1), true);
    assert!(required.is_guarded());
    required.is_verified_by(&Proof::capture(&output(proof.to_owned(), 0)))
}

#[test]
fn python_controls_must_pass_normally_with_and_without_added_regressions() {
    for format in FORMATS {
        let failure = format.render(&FAILED);
        assert!(verifies(&failure, &format.render(&PASSED)), "{format:?}");
        for growth in [false, true] {
            let mut good = PASSED.to_vec();
            if growth {
                good.push(("test_regression", "passed"));
            }
            assert!(verifies(&failure, &format.render(&good)));
            for status in ["skipped", "xfailed", "xpassed"] {
                let mut rows = good.clone();
                rows[1].1 = status;
                let proof = format.render(&rows);
                assert!(!verifies(&failure, &proof), "{format:?} {status} {growth}");
                assert!(!format.directly_verifies(&failure, &proof));
                if status != "xpassed" || !matches!(format, Format::Unittest) {
                    assert!(
                        output(proof, 0).succeeded(),
                        "independent clean process is insufficient"
                    );
                }
            }
        }
    }
}

#[test]
fn python_original_nonexecuted_subjects_stay_in_inventory_without_becoming_controls() {
    for format in FORMATS {
        for status in ["skipped", "xfailed"] {
            let mut before = FAILED.to_vec();
            before.push(("test_future", status));
            let mut after = PASSED.to_vec();
            after.push(("test_future", status));
            after.push(("test_regression", "passed"));
            let failure = format.render(&before);
            assert!(verifies(&failure, &format.render(&after)));
            after.remove(2);
            assert!(!verifies(&failure, &format.render(&after)));
        }
    }
}

#[test]
fn python_control_identity_includes_case_module_and_class_not_just_test_counts() {
    for format in FORMATS {
        let failure = format.render(&FAILED);
        for replacement in ["test_Control", "test_other"] {
            let proof = format.render(&[
                ("test_cache", "passed"),
                (replacement, "passed"),
                ("test_regression", "passed"),
            ]);
            assert!(!verifies(&failure, &proof));
        }
        let proof = format.render(&PASSED);
        let alias = if matches!(format, Format::Unittest) {
            proof.replace("tests.test_cache.Cache", "tests.test_cache.Other")
        } else {
            proof.replace("tests/test_cache.py", "tests/test_other.py")
        };
        assert!(!verifies(&failure, &alias));
        let mut reordered = PASSED.to_vec();
        reordered.reverse();
        assert!(verifies(&failure, &format.render(&reordered)));
    }
}

#[test]
fn python_controls_remain_required_beyond_the_diagnostic_display_cap() {
    for format in FORMATS {
        let names = (0..8)
            .map(|i| format!("test_failure_{i}"))
            .collect::<Vec<_>>();
        let mut before = names
            .iter()
            .map(|name| (name.as_str(), "failed"))
            .collect::<Vec<_>>();
        before.push(("test_control", "passed"));
        let mut after = names
            .iter()
            .map(|name| (name.as_str(), "passed"))
            .collect::<Vec<_>>();
        after.push(("test_control", "passed"));
        after.push(("test_regression", "passed"));
        let failure = format.render(&before);
        assert!(structured_error_diagnostics(&failure).len() <= 4);
        assert!(verifies(&failure, &format.render(&after)));
        after[8].1 = "skipped";
        assert!(!verifies(&failure, &format.render(&after)));
    }
}

#[test]
fn python_controls_do_not_weaken_process_completion_poll_or_family_guards() {
    for format in FORMATS {
        let failed = output(format.render(&FAILED), 1);
        let passed = output(format.render(&PASSED), 0);
        let good = Proof::capture(&passed);
        let mut required = Requirement::capture(&failed, false);
        assert!(!required.is_verified_by(&good));
        required.refresh(&failed, true);
        assert!(required.is_verified_by(&good));
        let unfinished = ToolOutput {
            text: passed.text.clone(),
            ..ToolOutput::default()
        };
        assert!(!required.is_verified_by(&Proof::capture(&unfinished)));
        let nonzero = ToolOutput {
            exit_code: Some(1),
            ..passed.clone()
        };
        assert!(!required.is_verified_by(&Proof::capture(&nonzero)));
        for other in FORMATS {
            if std::mem::discriminant(&format) != std::mem::discriminant(&other) {
                assert!(
                    !required.is_verified_by(&Proof::capture(&output(other.render(&PASSED), 0)))
                );
            }
        }
        required.refresh(&output("{}".to_owned(), 0), true);
        assert!(required.is_guarded());
        assert!(!required.is_verified_by(&good));
    }
}

#[test]
fn pytest_control_proof_requires_all_phases_and_unambiguous_full_reports() {
    let failure = Format::Json.render(&FAILED);
    let original = json_report(&PASSED);
    let mut missing = original.clone();
    missing["tests"][1]
        .as_object_mut()
        .unwrap()
        .remove("teardown");
    assert!(!verifies(&failure, &missing.to_string()));
    let mut hidden_failure = original.clone();
    hidden_failure["tests"][1]["outcome"] = json!("skipped");
    hidden_failure["tests"][1]["call"] =
        json!({"duration": 0.01, "outcome": "failed", "crash": {"message": "control failed"}});
    hidden_failure["tests"][1]["teardown"]["outcome"] = json!("skipped");
    hidden_failure["summary"]["passed"] = json!(1);
    hidden_failure["summary"]["skipped"] = json!(1);
    assert!(!verifies(&failure, &hidden_failure.to_string()));
    let proof = original.to_string();
    for damaged in [
        format!("{proof}\n{{"),
        proof.replacen("\"exitcode\":0", "\"exitcode\":1,\"exitcode\":0", 1),
    ] {
        assert!(!verifies(&failure, &damaged));
    }
}

fn store_episode(format: Format, proof: &str, expected_repairs: u32) -> TestResult {
    const WS: &str = "wsp_01234567890123456789012345";
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    db.insert_workspace(
        WS,
        &CreateWorkspaceInput {
            path: "/tmp/python-control-recall".to_owned(),
            name: None,
        },
    )?;
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(8200)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: WS.to_owned(),
            cass_session_id: "/sessions/python-controls.jsonl".to_owned(),
            source_path: None,
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(b"python-controls").to_hex()),
            metadata_json: None,
        },
    )?;
    let failed = format.render(&FAILED);
    let command = format.command();
    let rows = [
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":command}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"fail","content":failed,"is_error":true}),
        ),
        (
            "message",
            "assistant",
            json!({"type":"text","text":"The cache test failed because reads used inconsistent keys. I fixed the cache lookup by normalizing keys before reads and writes in src/cache.py."}),
        ),
        (
            "tool_call",
            "assistant",
            json!({"type":"tool_use","id":"retry","name":"Bash","input":{"command":command}}),
        ),
        (
            "tool_result",
            "user",
            json!({"type":"tool_result","tool_use_id":"retry","content":proof,"is_error":false}),
        ),
    ];
    let id = |line: u32| {
        crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(8300 + u128::from(line)))
            .to_string()
    };
    for (index, (kind, role, block)) in rows.iter().enumerate() {
        let line = u32::try_from(index + 1)?;
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        db.insert_evidence_span(
            &id(line),
            &CreateEvidenceSpanInput {
                workspace_id: WS.to_owned(),
                session_id: session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{session}:{line}"),
                span_kind: (*kind).to_owned(),
                start_line: line,
                end_line: line,
                start_byte: None,
                end_byte: None,
                role: Some((*role).to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
    }
    let before = db.list_evidence_spans_for_session(&session)?;
    let report = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(report.failures_seen, 1, "{format:?}");
    assert_eq!(report.resolved_failures, expected_repairs, "{format:?}");
    assert_eq!(
        report.incident_cards_recorded, expected_repairs,
        "{format:?}"
    );
    assert_eq!(report.repair_links_recorded, 2 * expected_repairs);
    let diagnostic = structured_error_diagnostics(&failed)
        .into_iter()
        .next()
        .ok_or("missing diagnostic")?;
    let recall = error_recall_report(&db, WS, &diagnostic)?;
    if expected_repairs == 0 {
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
    } else {
        assert!(recall.helpful_repairs.contains(&id(3)));
        assert_eq!(recall.proof_links, vec![id(5)]);
        let card = crate::core::incident_card::incident_card_id(WS, &id(2));
        assert!(db.get_search_admitted_evidence_span(&card, WS)?.is_some());
        assert!(pack_error_recall_query_seed(&db, WS, &diagnostic)?.contains("normalizing keys"));
    }
    for source in before {
        let after = db
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
        record_session_error_recall(&db, WS, &session)?.incident_cards_recorded,
        0
    );
    Ok(())
}

#[test]
fn python_full_control_coverage_produces_cited_durable_repairs_and_pack_context() -> TestResult {
    for format in FORMATS {
        let proof = format.render(&[
            ("test_cache", "passed"),
            ("test_control", "passed"),
            ("test_regression", "passed"),
        ]);
        store_episode(format, &proof, 1)?;
    }
    Ok(())
}

#[test]
fn python_skipped_or_expected_controls_never_produce_durable_repair_advice() -> TestResult {
    for format in FORMATS {
        for status in ["skipped", "xfailed", "xpassed"] {
            let proof = format.render(&[
                ("test_cache", "passed"),
                ("test_control", status),
                ("test_regression", "passed"),
            ]);
            store_episode(format, &proof, 0)?;
        }
    }
    Ok(())
}
