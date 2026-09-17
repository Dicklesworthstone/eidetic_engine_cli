use super::*;
use crate::core::ask::{
    ASK_CANDIDATE_SCAN_CAP, AskRequest, ask_data_json, evaluate_ask, tokenize_for_ask,
};

fn candidate(id: &str, content: &str) -> AskCandidate {
    AskCandidate {
        memory_id: id.to_owned(),
        content: content.to_owned(),
        confidence: 1.0,
        trust_class: "human_explicit".to_owned(),
        provenance_uri: Some(format!("manual://selection/{id}")),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        team_provenance: None,
    }
}

fn request() -> AskRequest {
    AskRequest {
        question: "Run cargo fmt before release".to_owned(),
        ..AskRequest::default()
    }
}

fn noise(count: usize) -> Vec<AskCandidate> {
    (0..count)
        .map(|index| candidate(&format!("noise-{index:05}"), "The database listens on port 5432."))
        .collect()
}

fn selected_ids(candidates: &[AskCandidate], limit: usize) -> Vec<String> {
    select_candidates(&request(), &tokenize_for_ask(&request().question), candidates, limit)
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.memory_id.clone())
        .collect()
}

#[test]
fn answers_a_direct_hit_after_the_old_scan_limit() {
    let mut candidates = noise(ASK_CANDIDATE_SCAN_CAP + 40);
    candidates.push(candidate("answer", "Run cargo fmt before release."));
    let report = evaluate_ask(&request(), &candidates);
    assert!(!report.abstained);
    assert_eq!(report.citations.len(), 1);
    assert_eq!(report.citations[0].memory_id, "answer");
    assert_eq!(report.candidates_scanned, candidates.len());
    assert!(report.semantic_degraded); // This change does not claim embeddings.
    assert_eq!(selected_ids(&candidates, ASK_CANDIDATE_SCAN_CAP).len(), ASK_CANDIDATE_SCAN_CAP);
}

#[test]
fn large_corpus_permutations_produce_the_same_answer_bytes() {
    let mut candidates = noise(ASK_CANDIDATE_SCAN_CAP + 40);
    candidates.push(candidate("answer", "Run cargo fmt before release."));
    let expected = ask_data_json(&evaluate_ask(&request(), &candidates)).to_string();
    candidates.reverse();
    assert_eq!(ask_data_json(&evaluate_ask(&request(), &candidates)).to_string(), expected);
    candidates.rotate_left(73);
    assert_eq!(ask_data_json(&evaluate_ask(&request(), &candidates)).to_string(), expected);
}

#[test]
fn admission_ties_use_memory_identity_not_input_position() {
    let candidates: Vec<_> = (0..20)
        .rev()
        .map(|i| candidate(&format!("memory-{i:02}"), "Run cargo fmt before release."))
        .collect();
    assert_eq!(selected_ids(&candidates, 3), ["memory-00", "memory-01", "memory-02"]);
}

#[test]
fn repeated_rows_do_not_consume_distinct_memory_slots() {
    let mut candidates = vec![candidate("first", "Run cargo fmt before release."); 600];
    candidates.push(candidate("second", "Run cargo fmt before release."));
    assert_eq!(selected_ids(&candidates, 2), ["first", "second"]);
}

#[test]
fn best_span_admission_does_not_dilute_a_long_answer_source() {
    let mut candidates = noise(4);
    let text = format!("{}\n\nRun cargo fmt before release.", "Unrelated database material. ".repeat(200));
    candidates.push(candidate("long-source", &text));
    assert_eq!(selected_ids(&candidates, 1), ["long-source"]);
}

#[test]
fn admission_does_not_lower_the_answer_floor_to_fill_the_budget() {
    let candidates = noise(ASK_CANDIDATE_SCAN_CAP + 40);
    let report = evaluate_ask(&request(), &candidates);
    assert!(report.abstained);
    assert!(report.citations.is_empty());
    assert!(report.confidence < request().min_confidence);
}

#[test]
fn ambiguous_source_after_the_limit_fails_closed_in_both_orders() {
    let mut candidates = noise(ASK_CANDIDATE_SCAN_CAP + 20);
    candidates.insert(0, candidate("ambiguous", "Run cargo fmt before release."));
    candidates.push(candidate("ambiguous", "Never run cargo fmt before release."));
    for _ in 0..2 {
        let report = evaluate_ask(&request(), &candidates);
        assert!(report.abstained && report.extractiveness_violated);
        assert!(report.citations.is_empty());
        assert!(report.sides.is_none());
        assert!(report.nearest_evidence.is_none());
        assert!(ask_data_json(&report).get("queryAssist").is_none());
        candidates.reverse();
    }
}

#[test]
fn conflicting_citation_metadata_is_not_arbitrarily_attested() {
    let source = candidate("same", "Run cargo fmt before release.");
    for field in 0..5 {
        let mut changed = source.clone();
        match field {
            0 => changed.confidence = 0.6,
            1 => changed.trust_class = "legacy_import".to_owned(),
            2 => changed.provenance_uri = Some("manual://different-source".to_owned()),
            3 => changed.level = "semantic".to_owned(),
            _ => changed.kind = "fact".to_owned(),
        }
        assert_eq!(
            select_candidates(&request(), &[], &[source.clone(), changed], 1).unwrap_err(),
            SelectionError::AmbiguousSource,
        );
    }
}

#[test]
fn invalid_confidence_cannot_poison_float_ordering_or_citations() {
    for confidence in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
        let mut bad = candidate("invalid", "Run cargo fmt before release.");
        bad.confidence = confidence;
        let candidates = [candidate("valid", "Run cargo fmt before release."), bad];
        assert_eq!(
            select_candidates(&request(), &[], &candidates, 1).unwrap_err(),
            SelectionError::InvalidConfidence,
        );
        let report = evaluate_ask(&request(), &candidates);
        assert!(report.abstained && report.extractiveness_violated);
    }
}

#[test]
fn zero_limit_and_empty_input_are_bounded() {
    let candidates = noise(3);
    assert!(selected_ids(&candidates, 0).is_empty());
    assert!(selected_ids(&[], 3).is_empty());
}

#[test]
fn identity_validation_is_not_skipped_for_a_zero_admission_limit() {
    let candidates = [candidate("same", "First body."), candidate("same", "Different body.")];
    assert_eq!(select_candidates(&request(), &[], &candidates, 0).unwrap_err(), SelectionError::AmbiguousSource);
}

#[test]
fn confidence_boundary_values_remain_valid_sources() {
    let mut candidates = noise(2);
    candidates[0].confidence = 0.0;
    candidates[1].confidence = 1.0;
    assert_eq!(selected_ids(&candidates, 10).len(), 2);
}

#[test]
fn citations_keep_exact_utf8_source_offsets_after_admission() {
    let mut candidates = noise(ASK_CANDIDATE_SCAN_CAP + 1);
    let source = "Préambule 🦀.\n\nRun cargo fmt before release.";
    candidates.push(candidate("unicode", source));
    let report = evaluate_ask(&request(), &candidates);
    assert!(!report.abstained);
    let citation = &report.citations[0];
    assert_eq!(citation.memory_id, "unicode");
    assert_eq!(source.get(citation.byte_start..citation.byte_end), Some(citation.text.as_str()));
    assert_eq!(citation.text, "Run cargo fmt before release.");
}

#[test]
fn duplicate_rows_do_not_increase_answer_confidence() {
    let source = candidate("answer", "Run cargo fmt before release.");
    let single = evaluate_ask(&request(), std::slice::from_ref(&source));
    let repeated = evaluate_ask(&request(), &vec![source; ASK_CANDIDATE_SCAN_CAP + 20]);
    assert_eq!(single.confidence, repeated.confidence);
    assert_eq!(single.citations.len(), repeated.citations.len());
    assert_eq!(single.confidence_components.corroboration, repeated.confidence_components.corroboration);
}

fn contradiction(id: &str, source: &str, destination: &str) -> AskContradiction {
    AskContradiction {
        id: id.to_owned(),
        src_memory_id: source.to_owned(),
        dst_memory_id: destination.to_owned(),
        confidence: 0.9,
        source: "human".to_owned(),
    }
}

fn crowded_conflict() -> (AskRequest, Vec<AskCandidate>) {
    let mut request = request();
    request.contradictions = vec![contradiction("edge-1", "anchor", "opposition")];
    let mut candidates: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 4)
        .map(|i| candidate(&format!("support-{i:05}"), "Run cargo fmt before release."))
        .collect();
    candidates.push(candidate("anchor", "Run cargo fmt before release."));
    candidates.push(candidate("opposition", "Formatting is prohibited by the deployment policy."));
    (request, candidates)
}

#[test]
fn preserves_paraphrased_opposition_outside_the_relevance_budget() {
    let (request, candidates) = crowded_conflict();
    let selected = select_candidates(
        &request, &tokenize_for_ask(&request.question), &candidates, ASK_CANDIDATE_SCAN_CAP,
    ).unwrap();
    assert_eq!(selected.len(), ASK_CANDIDATE_SCAN_CAP);
    assert!(selected.iter().any(|candidate| candidate.memory_id == "opposition"));
    let report = evaluate_ask(&request, &candidates);
    assert!(!report.abstained && report.conflict_detected);
    assert!(report.answer_text.is_none());
    let sides = report.sides.as_ref().unwrap();
    assert_eq!(sides.len(), 2);
    assert_eq!(sides[0].citations[0].memory_id, "anchor");
    assert_eq!(sides[1].citations[0].memory_id, "opposition");
    assert_eq!(report.conflict_link.as_ref().unwrap().id, "edge-1");
    for side in sides {
        for citation in &side.citations {
            let original = candidates.iter().find(|source| source.memory_id == citation.memory_id).unwrap();
            assert_eq!(original.content.get(citation.byte_start..citation.byte_end), Some(citation.text.as_str()));
        }
    }
}

#[test]
fn opposition_survives_independent_candidate_and_link_permutations() {
    let (mut request, mut candidates) = crowded_conflict();
    request.contradictions.push(contradiction("edge-0", "missing", "anchor"));
    let expected = ask_data_json(&evaluate_ask(&request, &candidates)).to_string();
    candidates.reverse();
    request.contradictions.reverse();
    assert_eq!(ask_data_json(&evaluate_ask(&request, &candidates)).to_string(), expected);
    candidates.rotate_left(17);
    assert_eq!(ask_data_json(&evaluate_ask(&request, &candidates)).to_string(), expected);
}

#[test]
fn explicit_links_do_not_pull_missing_sources_into_scope() {
    let (mut request, candidates) = crowded_conflict();
    request.contradictions = vec![contradiction("outside", "anchor", "another-workspace")];
    let report = evaluate_ask(&request, &candidates);
    assert!(!report.abstained && !report.conflict_detected);
    assert!(report.citations.iter().all(|citation| citation.memory_id != "another-workspace"));
}

#[test]
fn weak_opposition_is_not_promoted_by_a_strong_link() {
    let (request, mut candidates) = crowded_conflict();
    candidates.iter_mut().find(|candidate| candidate.memory_id == "opposition").unwrap().confidence = 0.4;
    let report = evaluate_ask(&request, &candidates);
    assert!(!report.conflict_detected);
    assert!(report.sides.is_none());
}

#[test]
fn inferred_links_cannot_reserve_an_evidence_slot() {
    let (mut request, candidates) = crowded_conflict();
    for source in ["inferred", "auto", "", "Human"] {
        request.contradictions[0].source = source.to_owned();
        let report = evaluate_ask(&request, &candidates);
        assert!(!report.conflict_detected);
    }
}

#[test]
fn invalid_or_below_floor_link_confidence_is_rejected() {
    let (mut request, candidates) = crowded_conflict();
    for confidence in [f32::NAN, f32::INFINITY, -0.1, 0.54, 1.1] {
        request.contradictions[0].confidence = confidence;
        let report = evaluate_ask(&request, &candidates);
        assert!(!report.conflict_detected);
    }
}

#[test]
fn links_cannot_manufacture_a_query_relevant_anchor() {
    let mut request = request();
    request.contradictions = vec![contradiction("edge", "first", "second")];
    let candidates = [
        candidate("first", "The database listens on port 5432."),
        candidate("second", "A vacuum occurs nightly."),
    ];
    let report = evaluate_ask(&request, &candidates);
    assert!(report.abstained);
    assert!(!report.conflict_detected);
}

#[test]
fn linked_empty_sources_are_not_treated_as_counterevidence() {
    let (request, mut candidates) = crowded_conflict();
    candidates.iter_mut().find(|candidate| candidate.memory_id == "opposition").unwrap().content = " \n\t".to_owned();
    assert!(!evaluate_ask(&request, &candidates).conflict_detected);
}

#[test]
fn one_hop_reservation_does_not_propagate_through_a_link_chain() {
    let mut request = request();
    request.contradictions = vec![
        contradiction("first", "anchor", "opposition"),
        contradiction("second", "opposition", "chained"),
    ];
    let candidates = [
        candidate("anchor", "Run cargo fmt before release."),
        candidate("support", "Run cargo fmt before release."),
        candidate("opposition", "Formatting is prohibited by deployment policy."),
        candidate("chained", "The production exception requires approval."),
    ];
    let selected = select_candidates(&request, &tokenize_for_ask(&request.question), &candidates, 2).unwrap();
    assert_eq!(selected.iter().map(|candidate| candidate.memory_id.as_str()).collect::<Vec<_>>(), ["anchor", "opposition"]);
}

#[test]
fn a_single_slot_budget_keeps_the_query_anchor() {
    let (request, candidates) = crowded_conflict();
    let selected = select_candidates(&request, &tokenize_for_ask(&request.question), &candidates, 1).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].memory_id, "anchor");
}

#[test]
fn empty_sources_do_not_displace_actual_spans_at_zero_threshold() {
    let request = AskRequest { min_confidence: 0.0, ..request() };
    let candidates = [
        candidate("a-empty", " \n"),
        candidate("b-real", "Unrelated but nonempty evidence."),
    ];
    let selected = select_candidates(&request, &[], &candidates, 1).unwrap();
    assert_eq!(selected[0].memory_id, "b-real");
}

#[test]
fn shared_conflict_floor_checks_every_confidence_contributor() {
    for weak in 0..4 {
        let mut values = [1.0, 1.0, 1.0, 1.0];
        values[weak] = 0.54;
        assert_eq!(conflict_score(values[0], values[1], values[2], values[3], 0.55), None);
        values[weak] = 0.55;
        assert_eq!(conflict_score(values[0], values[1], values[2], values[3], 0.55), Some(0.55));
    }
}

#[test]
fn shared_conflict_gate_rejects_non_finite_values() {
    for bad in 0..5 {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut values = [1.0, 1.0, 1.0, 1.0, 0.55];
            values[bad] = invalid;
            assert_eq!(conflict_score(values[0], values[1], values[2], values[3], values[4]), None);
        }
    }
}
