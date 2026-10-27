//! Coverage-preserving growth through the native TAP and durable recall paths.
//! The two growth fixtures were captured with Node 22.16.0; only the absolute
//! fixture root, measured durations and blank-line spacing are normalized.
//! No backend is mocked.

#![allow(clippy::expect_used)]

use super::super::super::{Proof, Requirement};
use super::*;
use crate::core::cass_error_recall::{ToolOutput, failure_diagnostics};

const NATIVE_FAILED: &str = include_str!("../../tests/fixtures/cass_node_tap/growth_failed.tap");
const NATIVE_GROWN: &str = include_str!("../../tests/fixtures/cass_node_tap/growth_passed.tap");

fn output(text: &str) -> ToolOutput {
    ToolOutput {
        text: text.to_owned(),
        is_error: Some(false),
        exit_code: Some(0),
    }
}

fn parsed(text: &str) -> super::super::Decoded {
    decode(text)
        .expect("complete TAP report")
        .expect("Node TAP family")
}

fn verifies(failure: &str, proof: &str) -> bool {
    Requirement::capture(&output(failure), true).is_verified_by(&Proof::capture(&output(proof)))
}

// Small complete flat reports isolate status and membership changes. Native
// nested producer output independently exercises hierarchy and real framing.
fn report(rows: &[(&str, &str)]) -> String {
    let mut text = "TAP version 13\n".to_owned();
    let mut counts = [0; 4];
    for (index, (name, status)) in rows.iter().enumerate() {
        let (result, directive, category) = match *status {
            "pass" => ("ok", "", 0),
            "fail" => ("not ok", "", 1),
            "skip" => ("ok", " # SKIP", 2),
            "todo" => ("ok", " # TODO", 3),
            _ => panic!("invalid fixture status"),
        };
        counts[category] += 1;
        text.push_str(&format!(
            "# Subtest: {name}\n{result} {} - {name}{directive}\n  ---\n  duration_ms: 1\n  type: 'test'\n",
            index + 1,
        ));
        if *status == "fail" {
            // Distinct messages keep the four-diagnostic cap independently
            // observable; proof coverage still includes every failed subject.
            text.push_str(&format!(
                "  failureType: 'testCodeFailure'\n  error: '{name} cache revision was stale'\n  code: 'ERR_ASSERTION'\n",
            ));
        }
        text.push_str("  ...\n");
    }
    text.push_str(&format!(
        "1..{}\n# tests {}\n# suites 0\n# pass {}\n# fail {}\n# cancelled 0\n# skipped {}\n# todo {}\n# duration_ms 1\n",
        rows.len(), rows.len(), counts[0], counts[1], counts[2], counts[3],
    ));
    text
}

fn flat_failure() -> String {
    report(&[("repair", "fail"), ("control", "pass")])
}

fn flat_growth() -> String {
    report(&[
        ("new regression", "pass"),
        ("control", "pass"),
        ("repair", "pass"),
    ])
}

#[test]
fn complete_flat_and_native_nested_suites_accept_added_regression_tests() {
    for (failure, proof) in [
        (flat_failure(), flat_growth()),
        (NATIVE_FAILED.to_owned(), NATIVE_GROWN.to_owned()),
    ] {
        let required = parsed(&failure);
        let retry = parsed(&proof);
        assert!(required.execution.has_known_failures());
        assert!(retry.output.allows_completion);
        assert!(retry.execution.inventory.len() > required.execution.inventory.len());
        assert!(required.execution.verified_by(&retry.execution));
        assert!(verifies(&failure, &proof));
    }
}

#[test]
fn growing_inventory_never_replaces_original_names_parents_or_node_kinds() {
    for (failure, proof) in [
        (
            flat_failure(),
            flat_growth().replace("control", "replacement"),
        ),
        (flat_failure(), flat_growth().replace("repair", "Repair")),
        (
            NATIVE_FAILED.to_owned(),
            NATIVE_GROWN.replace("ledger", "different ledger"),
        ),
        (
            NATIVE_FAILED.to_owned(),
            NATIVE_GROWN.replace("saved revision", "another revision"),
        ),
        (
            NATIVE_FAILED.to_owned(),
            NATIVE_GROWN
                .replace("type: 'suite'", "type: 'test'")
                .replace("# tests 3", "# tests 4")
                .replace("# suites 1", "# suites 0")
                .replace("# pass 3", "# pass 4"),
        ),
    ] {
        assert!(
            output(&proof).succeeded(),
            "clean output with different subjects"
        );
        assert!(!verifies(&failure, &proof));
    }
}

#[test]
fn prior_passes_cannot_become_skipped_or_todo_even_without_suite_growth() {
    for status in ["skip", "todo"] {
        for grow in [false, true] {
            let mut rows = vec![("repair", "pass"), ("control", status)];
            if grow {
                rows.push(("new regression", "pass"));
            }
            let proof = report(&rows);
            assert!(
                output(&proof).succeeded(),
                "process success does not prove retained execution"
            );
            let required = parsed(&flat_failure());
            assert!(!required.execution.verified_by(&parsed(&proof).execution));
            assert!(!verifies(&flat_failure(), &proof));
        }
        let marker = if status == "skip" { "SKIP" } else { "TODO" };
        let counter = if status == "skip" { "skipped" } else { "todo" };
        let proof = NATIVE_GROWN
            .replace(
                "ok 2 - control still runs\n",
                &format!("ok 2 - control still runs # {marker}\n"),
            )
            .replace("# pass 3", "# pass 2")
            .replace(&format!("# {counter} 0"), &format!("# {counter} 1"));
        assert!(output(&proof).succeeded());
        assert!(!verifies(NATIVE_FAILED, &proof));
    }
}

#[test]
fn original_nonexecuted_subjects_must_remain_but_need_not_become_passes() {
    let failure = report(&[
        ("repair", "fail"),
        ("known skip", "skip"),
        ("known todo", "todo"),
    ]);
    for status in ["skip", "pass"] {
        let proof = report(&[
            ("repair", "pass"),
            ("known skip", status),
            ("known todo", "todo"),
            ("new regression", "pass"),
        ]);
        assert!(verifies(&failure, &proof));
        assert!(!verifies(
            &failure,
            &proof.replace("known todo", "different todo")
        ));
    }
}

#[test]
fn remaining_or_new_failures_veto_even_the_direct_execution_matcher() {
    for rows in [
        vec![
            ("repair", "pass"),
            ("control", "pass"),
            ("new regression", "fail"),
        ],
        vec![
            ("repair", "fail"),
            ("control", "pass"),
            ("new regression", "pass"),
        ],
        vec![
            ("repair", "todo"),
            ("control", "pass"),
            ("new regression", "pass"),
        ],
        vec![
            ("repair", "skip"),
            ("control", "pass"),
            ("new regression", "pass"),
        ],
    ] {
        let proof = report(&rows);
        assert!(
            !parsed(&flat_failure())
                .execution
                .verified_by(&parsed(&proof).execution)
        );
        assert!(!verifies(&flat_failure(), &proof));
    }
}

#[test]
fn every_failure_is_required_even_after_diagnostic_extraction_is_capped() {
    let names = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
    let failed_rows: Vec<_> = names.iter().map(|name| (*name, "fail")).collect();
    let failure = report(&failed_rows);
    assert_eq!(failure_diagnostics(&output(&failure)).len(), 4);
    let mut rows: Vec<_> = names.iter().map(|name| (*name, "pass")).collect();
    rows.push(("new regression", "pass"));
    assert!(verifies(&failure, &report(&rows)));
    rows[5].1 = "todo";
    assert!(!verifies(&failure, &report(&rows)));
    rows[5] = ("replacement", "pass");
    assert!(!verifies(&failure, &report(&rows)));
}

#[test]
fn growth_keeps_independent_process_completion_and_poll_refusals() {
    let required = Requirement::capture(&output(NATIVE_FAILED), true);
    let proof = output(NATIVE_GROWN);
    for (is_error, exit_code) in [(None, None), (Some(true), Some(0)), (None, Some(1))] {
        let changed = ToolOutput {
            is_error,
            exit_code,
            ..proof.clone()
        };
        assert!(!required.is_verified_by(&Proof::capture(&changed)));
    }
    let mut pending = Requirement::capture(&output(NATIVE_FAILED), false);
    let proof = Proof::capture(&proof);
    assert!(!pending.is_verified_by(&proof));
    pending.refresh(&output(NATIVE_FAILED), true);
    assert!(pending.is_verified_by(&proof));
    pending.refresh(&output("TAP version 13\n# Subtest: incomplete"), true);
    assert!(pending.is_guarded());
    assert!(!pending.is_verified_by(&proof));
    pending.refresh(&output("unknown final poll"), true);
    assert!(!pending.is_verified_by(&proof));
}

#[test]
fn larger_counts_do_not_salvage_duplicate_incomplete_or_contradictory_reports() {
    for changed in [
        NATIVE_GROWN.replace("    1..3", "    1..4"),
        NATIVE_GROWN.replace("# tests 3", "# tests 4"),
        NATIVE_GROWN.replace("# pass 3", "# pass 4"),
        NATIVE_GROWN.replace("new regression", "control still runs"),
        NATIVE_GROWN.lines().take(20).collect::<Vec<_>>().join("\n"),
        format!("{NATIVE_GROWN}Process exited with code 0\n"),
        format!("{NATIVE_GROWN}{NATIVE_GROWN}"),
    ] {
        assert!(decode(&changed).is_err());
        assert!(!verifies(NATIVE_FAILED, &changed));
    }
}

#[test]
fn native_growth_reaches_persisted_repairs_cards_and_pack_context_idempotently() -> TestResult {
    let episode = episode(NATIVE_FAILED, NATIVE_GROWN, COMMAND)?;
    let before = episode
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    let result = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(result.failures_seen, 1);
    assert_eq!(result.resolved_failures, 1);
    assert_eq!(result.repair_links_recorded, 2);
    assert_eq!(result.incident_cards_recorded, 1);
    let decoded = parsed(NATIVE_FAILED);
    let diagnostic = decoded
        .output
        .diagnostics
        .first()
        .ok_or("missing diagnostic")?;
    let recall = error_recall_report(&episode.db, WS, diagnostic)?;
    let card = crate::core::incident_card::incident_card_id(WS, &evidence_id(2));
    assert!(recall.exact);
    assert!(recall.helpful_repairs.contains(&evidence_id(3)));
    assert!(recall.helpful_repairs.contains(&card));
    assert_eq!(recall.proof_links, vec![evidence_id(5)]);
    let links = episode
        .db
        .list_error_repair_links(WS, &diagnostic.layered_key().key)?;
    assert_eq!(links.len(), 3);
    assert!(
        links
            .iter()
            .all(|link| link.evidence_ref.as_deref() == Some(evidence_id(2).as_str()))
    );
    let evidence = recalled_repair_evidence(&episode.db, WS, &recall)?;
    assert!(
        evidence
            .iter()
            .any(|item| item.evidence_id == evidence_id(5) && item.text.is_none())
    );
    let seed = pack_error_recall_query_seed(&episode.db, WS, diagnostic)?;
    assert!(seed.contains("prior fix:") && seed.contains("stored revision"));
    for source in before {
        let after = episode
            .db
            .get_evidence_span(&source.id)?
            .ok_or("missing source")?;
        assert_eq!(after.excerpt, source.excerpt);
        assert_eq!(after.content_hash, source.content_hash);
        assert_eq!(
            after.canonical_provenance_uri(),
            source.canonical_provenance_uri()
        );
    }
    let rerun = record_session_error_recall(&episode.db, WS, &episode.session)?;
    assert_eq!(rerun.resolved_failures, 1);
    assert_eq!(rerun.incident_cards_recorded, 0);
    assert_eq!(
        episode
            .db
            .list_error_repair_links(WS, &diagnostic.layered_key().key)?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn skipped_controls_or_changed_commands_do_not_persist_helpful_repair_links() -> TestResult {
    let skipped = NATIVE_GROWN
        .replace(
            "ok 2 - control still runs\n",
            "ok 2 - control still runs # SKIP\n",
        )
        .replace("# pass 3", "# pass 2")
        .replace("# skipped 0", "# skipped 1");
    for (proof, command) in [
        (skipped.as_str(), COMMAND),
        (
            NATIVE_GROWN,
            "node --test --test-reporter=tap other.test.cjs",
        ),
    ] {
        let episode = episode(NATIVE_FAILED, proof, command)?;
        let result = record_session_error_recall(&episode.db, WS, &episode.session)?;
        assert_eq!(result.failures_seen, 1);
        assert_eq!(result.resolved_failures, 0);
        assert_eq!(result.repair_links_recorded, 0);
        assert_eq!(result.incident_cards_recorded, 0);
        let decoded = parsed(NATIVE_FAILED);
        let diagnostic = decoded
            .output
            .diagnostics
            .first()
            .ok_or("missing diagnostic")?;
        let recall = error_recall_report(&episode.db, WS, diagnostic)?;
        assert!(recall.helpful_repairs.is_empty() && recall.proof_links.is_empty());
    }
    Ok(())
}
