//! Repairs may add regression coverage, but may not replace original subjects.
//! Reuse the real migrated-store episode and the public recall/pack adapters.

use super::*;

fn growing_report() -> String {
    report(
        &[
            ("PASS", "other", "case"),
            ("PASS", "cache", "cache::round_trip"),
            ("PASS", "cache", "regression::flushes_pending_writes"),
        ],
        3,
    )
}

fn verifies(failure: &ToolOutput, proof: &ToolOutput) -> bool {
    Requirement::capture(failure, true).is_verified_by(&Proof::capture(proof))
}

#[test]
fn a_complete_failure_can_learn_from_a_clean_growing_regression_suite() {
    let failure = failed();
    let proof = output(growing_report());
    let before = (failure.clone(), proof.clone());
    assert!(proof.succeeded());
    assert!(verifies(&failure, &proof));
    let required = decode(&failure.text).unwrap().unwrap();
    let verified = decode(&proof.text).unwrap().unwrap();
    assert_eq!(required.execution.inventory.len(), 2);
    assert_eq!(verified.execution.inventory.len(), 3);
    assert!(required.execution.verified_by(&verified.execution));
    assert_eq!((failure, proof), before);
}

#[test]
fn growth_can_add_a_binary_but_cannot_change_profile_or_filter_posture() {
    let grown = growing_report()
        .replace("2 binaries", "3 binaries")
        .replace("cache regression::", "regression regression::");
    assert!(verifies(&failed(), &output(grown.clone())));
    for changed in [
        grown.replace("profile: default", "profile: ci"),
        grown.replace("3 binaries", "1 binary"),
        grown.replace("3 tests skipped", "4 tests skipped").replace("3 skipped", "4 skipped"),
        // Counters alone cannot authorize a different selection without growth.
        passed().text.replace("2 binaries", "3 binaries"),
    ] {
        assert!(!verifies(&failed(), &output(changed)));
    }
}

#[test]
fn more_passing_rows_cannot_replace_an_original_failure_or_control() {
    let grown = growing_report();
    for changed in [
        grown.replace("cache cache::round_trip", "foreign cache::round_trip"),
        grown.replace("cache::round_trip", "cache::Round_trip"),
        grown.replace("cache::round_trip", "cache::round_trip_2"),
        grown.replace("other case", "other replacement_control"),
    ] {
        let proof = output(changed);
        assert!(proof.succeeded(), "this is a clean but different inventory");
        assert!(!verifies(&failed(), &proof));
    }
}

#[test]
fn named_skips_cannot_hide_an_original_subject_behind_a_new_passing_test() {
    for (binary, name, other_binary, other_name) in [
        ("cache", "cache::round_trip", "other", "case"),
        ("other", "case", "cache", "cache::round_trip"),
    ] {
        let text = report(
            &[("PASS", other_binary, other_name), ("PASS", "cache", "regression")],
            3,
        );
        let text = text.replacen(
            "        PASS",
            &format!("        SKIP [         ] {binary} {name}\n        PASS"),
            1,
        );
        let proof = output(text);
        assert!(proof.succeeded());
        assert!(!verifies(&failed(), &proof));
    }
}

#[test]
fn a_new_regression_failure_vetoes_the_direct_matcher_and_the_public_proof() {
    let text = report(
        &[
            ("PASS", "other", "case"),
            ("PASS", "cache", "cache::round_trip"),
            ("FAIL", "cache", "regression"),
        ],
        3,
    );
    let required = decode(&failed().text).unwrap().unwrap();
    let retry = decode(&text).unwrap().unwrap();
    assert!(!required.execution.verified_by(&retry.execution));
    assert!(!verifies(&failed(), &output(text)));
}

#[test]
fn growth_preserves_every_failed_subject_beyond_the_diagnostic_cap() {
    let names: Vec<_> = (0..6).map(|index| format!("case_{index}")).collect();
    let failed_rows: Vec<_> = names.iter().map(|name| ("FAIL", "cache", name.as_str())).collect();
    let failure = output(report(&failed_rows, 0));
    assert_eq!(failure_diagnostics(&failure).len(), MAX_DIAGNOSTICS_PER_FAILURE);
    let mut passed_rows: Vec<_> = names.iter().map(|name| ("PASS", "cache", name.as_str())).collect();
    passed_rows.push(("PASS", "cache", "new_regression"));
    assert!(verifies(&failure, &output(report(&passed_rows, 0))));
    passed_rows[5].2 = "replacement";
    assert!(!verifies(&failure, &output(report(&passed_rows, 0))));
}

#[test]
fn fail_fast_unknown_subjects_keep_the_exact_selection_requirement() {
    let failure = output(super::super::fail_fast::failure_report());
    let retry = output(super::super::fail_fast::retry_report());
    assert!(verifies(&failure, &retry));
    let expanded = output(report(
        &[
            ("PASS", "cache", "cache::round_trip"),
            ("PASS", "other", "case"),
            ("PASS", "other", "previously_unstarted"),
            ("PASS", "cache", "new_regression"),
        ],
        3,
    ));
    assert!(expanded.succeeded());
    assert!(!verifies(&failure, &expanded));
}

#[test]
fn growth_retains_process_completion_poll_and_whole_report_guards() {
    let proof = output(growing_report());
    for (is_error, exit_code) in [(None, None), (Some(true), Some(0)), (None, Some(1))] {
        let rejected = ToolOutput { is_error, exit_code, ..proof.clone() };
        assert!(!verifies(&failed(), &rejected));
    }
    for text in [
        proof.text.replace("3 tests across", "4 tests across"),
        proof.text.replace("3 passed", "4 passed"),
        proof.text.replace("regression::flushes_pending_writes", "cache::round_trip"),
        format!("{}output truncated\n", proof.text),
        proof.text.lines().take(6).collect::<Vec<_>>().join("\n"),
    ] {
        assert!(!verifies(&failed(), &output(text)));
    }
    let mut pending = Requirement::capture(&failed(), false);
    let verified = Proof::capture(&proof);
    assert!(!pending.is_verified_by(&verified));
    pending.refresh(&failed(), true);
    assert!(pending.is_verified_by(&verified));
    pending.refresh(&output("unknown final poll".to_owned()), true);
    assert!(pending.is_guarded());
    assert!(!pending.is_verified_by(&verified));
}

#[test]
fn growing_suite_records_repair_card_proof_and_pack_context_without_rewriting_sources() -> TestResult {
    let (db, session, ids) = episode(&growing_report(), COMMAND)?;
    let before = db.list_evidence_spans_for_session(&session)?;
    let recorded = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(recorded.failures_seen, 1);
    assert_eq!(recorded.resolved_failures, 1);
    assert_eq!(recorded.repair_links_recorded, 2);
    assert_eq!(recorded.incident_cards_recorded, 1);
    let diagnostic = structured_error_diagnostics(&failed().text).remove(0);
    let recall = error_recall_report(&db, WS, &diagnostic)?;
    assert!(recall.exact);
    assert!(recall.helpful_repairs.contains(&ids[2]));
    let card = crate::core::incident_card::incident_card_id(WS, &ids[1]);
    assert!(recall.helpful_repairs.contains(&card));
    assert_eq!(recall.proof_links, vec![ids[4].clone()]);
    assert!(pack_error_recall_query_seed(&db, WS, &diagnostic)?.contains("flushing"));
    let key = diagnostic.layered_key().key;
    let links = db.list_error_repair_links(WS, &key)?;
    assert_eq!(links.len(), 3);
    assert!(links.iter().all(|link| link.evidence_ref.as_deref() == Some(ids[1].as_str())));
    for source in before {
        let after = db.get_evidence_span(&source.id)?.ok_or("source disappeared")?;
        assert_eq!(source.excerpt, after.excerpt);
        assert_eq!(source.content_hash, after.content_hash);
        assert_eq!(source.canonical_provenance_uri(), after.canonical_provenance_uri());
    }
    let rerun = record_session_error_recall(&db, WS, &session)?;
    assert_eq!(rerun.resolved_failures, 1);
    assert_eq!(rerun.incident_cards_recorded, 0);
    assert_eq!(db.list_error_repair_links(WS, &key)?.len(), 3);
    Ok(())
}

#[test]
fn growing_suite_does_not_persist_repairs_from_another_invocation_or_missing_control() -> TestResult {
    for (proof, command) in [
        (growing_report(), "cargo nextest run --lib"),
        (growing_report().replace("other case", "other replacement"), COMMAND),
    ] {
        let (db, session, _) = episode(&proof, command)?;
        let recorded = record_session_error_recall(&db, WS, &session)?;
        assert_eq!(recorded.failures_seen, 1);
        assert_eq!(recorded.resolved_failures, 0);
        assert_eq!(recorded.repair_links_recorded, 0);
        assert_eq!(recorded.incident_cards_recorded, 0);
        let diagnostic = structured_error_diagnostics(&failed().text).remove(0);
        let recall = error_recall_report(&db, WS, &diagnostic)?;
        assert!(recall.helpful_repairs.is_empty());
        assert!(recall.proof_links.is_empty());
    }
    Ok(())
}
