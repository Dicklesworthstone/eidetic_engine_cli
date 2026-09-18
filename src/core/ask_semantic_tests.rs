#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{
    ASK_CANDIDATE_SCAN_CAP, AskContradiction, AskNativeSource, ask_data_json, score_span,
};
use crate::models::EvidenceId;
use crate::pack::PackEntityRef;

const QUESTION: &str = "Which command formats source before shipping?";
const ANSWER: &str = "Run cargo fmt on source before release.";
const DISTRACTOR: &str = "Command formats source before shipping.";
const OPPOSING: &str = "Never run cargo fmt on source before release.";

fn request() -> AskRequest {
    AskRequest {
        question: QUESTION.to_owned(),
        min_confidence: 0.68,
        ..AskRequest::default()
    }
}

fn candidate(id: &str, content: &str) -> AskCandidate {
    AskCandidate {
        memory_id: id.to_owned(),
        content: content.to_owned(),
        confidence: 1.0,
        trust_class: "human_explicit".to_owned(),
        provenance_uri: Some("cass-session://same-conversation#L1".to_owned()),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        team_provenance: None,
    }
}

fn scores() -> SemanticScores<'static> {
    // Exact scalar fixtures test the real scorer and admission/composition;
    // these are not a mock embedder and do not claim neural inference ran.
    SemanticScores {
        by_text: BTreeMap::from([(ANSWER, 1.0), (DISTRACTOR, 0.0), (OPPOSING, 0.9)]),
    }
}

fn report(request: &AskRequest, candidates: &[AskCandidate]) -> AskReport {
    finish_evaluation(request, candidates, Ok(scores())).unwrap()
}

fn selected(request: &AskRequest, candidates: &[AskCandidate], limit: usize) -> Vec<String> {
    let scores = scores();
    selection::select_candidates_with_scorer(
        request,
        &tokenize_for_ask(&request.question),
        candidates,
        limit,
        &|terms, text, confidence, trust| scores.score(terms, text, confidence, trust),
    )
    .unwrap()
    .iter()
    .map(|row| row.memory_id.clone())
    .collect()
}

fn crowded() -> Vec<AskCandidate> {
    let mut rows: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 8)
        .map(|index| candidate(&format!("a-noise-{index:05}"), DISTRACTOR))
        .collect();
    rows.push(candidate("z-answer", ANSWER));
    rows
}

#[test]
fn cosine_does_not_invent_support_for_orthogonal_negative_or_zero_vectors() {
    let query = [1.0, 0.0];
    assert_eq!(cosine(&query, 1.0, &[4.0, 0.0]), Some(1.0));
    assert_eq!(cosine(&query, 1.0, &[0.0, 4.0]), Some(0.0));
    assert_eq!(cosine(&query, 1.0, &[-4.0, 0.0]), Some(0.0));
    assert_eq!(cosine(&query, 1.0, &[0.0, 0.0]), Some(0.0));
}

#[test]
fn malformed_vector_spaces_are_rejected() {
    assert_eq!(vector_norm_squared(&[], 0), None);
    assert_eq!(vector_norm_squared(&[1.0], 2), None);
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(vector_norm_squared(&[value, 0.0], 2), None);
        assert_eq!(cosine(&[1.0, 0.0], 1.0, &[value, 0.0]), None);
    }
    assert_eq!(cosine(&[1.0, 0.0], 1.0, &[1.0]), None);
    assert_eq!(cosine(&[0.0, 0.0], 0.0, &[1.0, 0.0]), None);
    assert_eq!(cosine(&[1.0], f64::NAN, &[1.0]), None);
}

#[test]
fn finite_extreme_vectors_do_not_overflow_or_underflow_the_norm() {
    for value in [f32::MAX, f32::MIN_POSITIVE, f32::from_bits(1)] {
        let query = [value, value];
        let norm = vector_norm_squared(&query, 2).unwrap();
        assert!(norm.is_finite() && norm > 0.0);
        assert_eq!(cosine(&query, norm, &query), Some(1.0));
    }
}

#[test]
fn a_bad_batch_cannot_publish_a_valid_prefix() {
    for vectors in [
        vec![vec![1.0, 0.0]],
        vec![vec![1.0, 0.0], vec![1.0]],
        vec![vec![1.0, 0.0], vec![f32::NAN, 0.0]],
        vec![vec![1.0, 0.0]; 3],
    ] {
        let mut table = BTreeMap::from([("earlier", 0.25)]);
        let before = table.clone();
        assert_eq!(
            append_batch(&mut table, &[ANSWER, DISTRACTOR], &vectors, &[1.0, 0.0], 1.0),
            Err(SemanticFailure::Unavailable)
        );
        assert_eq!(table, before);
    }
}

#[test]
fn valid_batch_retains_only_scalar_support() {
    let mut table = BTreeMap::new();
    append_batch(
        &mut table,
        &[ANSWER, DISTRACTOR],
        &[vec![2.0, 0.0], vec![0.0, 3.0]],
        &[1.0, 0.0],
        1.0,
    )
    .unwrap();
    assert_eq!(table, BTreeMap::from([(ANSWER, 1.0), (DISTRACTOR, 0.0)]));
}

#[test]
fn semantic_and_lexical_paths_use_the_adr_weights() {
    let terms = tokenize_for_ask(QUESTION);
    let table = scores();
    let lexical = score_span(&terms, ANSWER, 1.0, "human_explicit");
    let semantic = table.score(&terms, ANSWER, 1.0, "human_explicit");
    assert!((lexical - 0.448_888_9).abs() < 0.000_001);
    assert!((semantic - 0.69).abs() < 0.000_001);
    assert!(lexical < request().min_confidence);
    assert!(semantic >= request().min_confidence);
    assert!((table.score(&terms, DISTRACTOR, 1.0, "human_explicit") - 0.65).abs() < 0.000_001);
}

#[test]
fn semantic_scoring_preserves_the_trust_tilt() {
    let terms = tokenize_for_ask(QUESTION);
    let table = scores();
    let human = table.score(&terms, ANSWER, 1.0, "human_explicit");
    let transcript = table.score(&terms, ANSWER, 0.5, "cass_evidence");
    assert!((human - transcript - 0.145).abs() < 0.000_001);
    assert!(transcript < human);
}

#[test]
fn semantic_match_survives_the_candidate_cap_before_composition() {
    let rows = crowded();
    let selected = selected(&request(), &rows, ASK_CANDIDATE_SCAN_CAP);
    assert_eq!(selected.len(), ASK_CANDIDATE_SCAN_CAP);
    assert_eq!(selected[0], "z-answer");
    let report = report(&request(), &rows);
    assert!(!report.abstained && !report.semantic_degraded);
    assert_eq!(report.candidates_scanned, rows.len());
    assert_eq!(report.citations.len(), 1);
    assert_eq!(report.citations[0].memory_id, "z-answer");
    assert_eq!(report.citations[0].text, ANSWER);
    assert!(evaluate_ask(&request(), &rows).citations.iter().all(|c| c.memory_id != "z-answer"));
}

#[test]
fn fixed_semantic_scores_produce_identical_output_after_source_permutations() {
    let mut rows = crowded();
    let expected = ask_data_json(&report(&request(), &rows));
    rows.reverse();
    assert_eq!(ask_data_json(&report(&request(), &rows)), expected);
    rows.rotate_left(17);
    assert_eq!(ask_data_json(&report(&request(), &rows)), expected);
}

#[test]
fn semantic_support_does_not_make_one_session_independent_votes() {
    let mut rows = vec![candidate("first", ANSWER)];
    let baseline = report(&request(), &rows);
    rows.extend((0..20).map(|i| candidate(&format!("same-{i}"), ANSWER)));
    let repeated = report(&request(), &rows);
    assert_eq!(repeated.confidence.to_bits(), baseline.confidence.to_bits());
    let mut independent = candidate("independent", ANSWER);
    independent.provenance_uri = Some("cass-session://different-conversation#L1".to_owned());
    rows.push(independent);
    assert!(report(&request(), &rows).confidence > repeated.confidence);
}

#[test]
fn explicit_opposition_is_reserved_for_the_semantic_not_the_lexical_anchor() {
    let mut request = request();
    let mut rows = crowded();
    rows.push(candidate("z-opposition", "Formatting is prohibited by deployment policy."));
    request.contradictions.push(AskContradiction {
        id: "edge".to_owned(),
        src_memory_id: "z-answer".to_owned(),
        dst_memory_id: "z-opposition".to_owned(),
        confidence: 0.9,
        source: "human".to_owned(),
    });
    let ids = selected(&request, &rows, 2);
    assert_eq!(ids, ["z-answer", "z-opposition"]);
    let report = report(&request, &rows);
    assert!(!report.abstained && report.conflict_detected && !report.semantic_degraded);
    assert_eq!(report.conflict_link.as_ref().unwrap().id, "edge");
    let sides = report.sides.unwrap();
    assert_eq!(sides[0].citations[0].memory_id, "z-answer");
    assert_eq!(sides[1].citations[0].memory_id, "z-opposition");
}

#[test]
fn inferred_opposition_uses_semantic_scores_for_both_reservation_and_composition() {
    let mut request = request();
    request.min_confidence = 0.6;
    let mut rows = crowded();
    rows.push(candidate("z-opposition", OPPOSING));
    assert!(selected(&request, &rows, 2).contains(&"z-opposition".to_owned()));
    let report = report(&request, &rows);
    assert!(report.conflict_detected && !report.semantic_degraded);
    assert!(report.conflict_link.is_none());
    assert!(report.sides.unwrap().iter().flat_map(|side| &side.citations).any(|c| c.text == OPPOSING));
}

#[test]
fn native_evidence_keeps_identity_revision_trust_and_exact_utf8_offsets() {
    let mut request = request();
    request.min_confidence = 0.5;
    let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(901));
    let mut source = candidate(&id.to_string(), &format!("Résumé 🦀. {ANSWER}"));
    source.kind = "evidence_span".to_owned();
    source.level = "episodic".to_owned();
    source.confidence = 0.5;
    source.trust_class = "cass_evidence".to_owned();
    let revision = format!("blake3:{}", blake3::hash(source.content.as_bytes()).to_hex());
    request.native_sources.insert(source.memory_id.clone(), AskNativeSource {
        entity: PackEntityRef::EvidenceSpan(id),
        entity_revision: revision.clone(),
        source_memory_ids: Vec::new(),
    });
    let report = report(&request, std::slice::from_ref(&source));
    assert!(!report.abstained && !report.semantic_degraded);
    assert_eq!(report.citations.len(), 1);
    let citation = &report.citations[0];
    assert!(citation.byte_start > 0);
    assert_eq!(source.content.get(citation.byte_start..citation.byte_end), Some(ANSWER));
    let data = ask_data_json(&report);
    assert_eq!(data["citations"][0]["entityRevision"], revision);
    assert_eq!(data["citations"][0]["evidenceId"], source.memory_id);
    assert_eq!(data["citations"][0]["confidence"], 0.5);
    assert_eq!(data["citations"][0]["trustClass"], "cass_evidence");
    assert!(data["citations"][0].get("memoryId").is_none());
}

#[test]
fn invalid_source_identity_fails_closed_even_with_semantic_support() {
    let rows = [candidate("same", ANSWER), candidate("same", OPPOSING)];
    let report = report(&request(), &rows);
    assert!(report.abstained && report.extractiveness_violated);
    assert!(report.citations.is_empty() && report.nearest_evidence.is_none());
    assert!(report.sides.is_none());
}

#[test]
fn invalid_request_threshold_is_not_a_semantic_answer() {
    let rows = [candidate("answer", ANSWER)];
    for threshold in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
        let mut request = request();
        request.min_confidence = threshold;
        let report = report(&request, &rows);
        assert!(report.abstained && report.extractiveness_violated);
        assert!(report.citations.is_empty() && report.nearest_evidence.is_none());
    }
}

#[test]
fn unavailable_semantics_recompute_the_entire_lexical_answer() {
    let rows = crowded();
    let expected = evaluate_ask(&request(), &rows);
    let actual = finish_evaluation(&request(), &rows, Err(SemanticFailure::Unavailable)).unwrap();
    assert_eq!(ask_data_json(&actual), ask_data_json(&expected));
    assert_eq!(actual.candidates_scanned, expected.candidates_scanned);
    assert!(actual.semantic_degraded);
}

#[test]
fn cancellation_is_not_reported_as_missing_evidence() {
    let rows = [candidate("answer", ANSWER)];
    assert!(finish_evaluation(&request(), &rows, Err(SemanticFailure::Cancelled)).is_err());
}

#[test]
fn lexical_scorer_callback_retains_existing_output() {
    let mut rows = crowded();
    rows.push(candidate("opposing", OPPOSING));
    let expected = evaluate_ask(&request(), &rows);
    let actual = evaluate_ask_scored(&request(), &rows, &score_span, true);
    assert_eq!(ask_data_json(&actual), ask_data_json(&expected));
}

#[test]
fn real_hash_embedder_is_refused_before_vector_inference() {
    let rows = [candidate("answer", ANSWER)];
    let embedder = crate::search::HashEmbedder::default_256();
    let result = crate::core::run_cli_future(async {
        let cx = Cx::current().expect("runtime context");
        SemanticScores::build(&cx, QUESTION, &rows, &embedder).await
    })
    .unwrap();
    assert!(matches!(result, Err(SemanticFailure::Unavailable)));
}
