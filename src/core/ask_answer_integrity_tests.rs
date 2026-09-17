use super::*;

fn candidate(id: &str, content: &str) -> AskCandidate {
    AskCandidate {
        memory_id: id.to_owned(),
        content: content.to_owned(),
        confidence: 1.0,
        trust_class: "human_explicit".to_owned(),
        provenance_uri: Some(format!("manual://ask-integrity/{id}")),
        level: "procedural".to_owned(),
        kind: "rule".to_owned(),
        team_provenance: None,
    }
}

fn request(question: &str) -> AskRequest {
    AskRequest {
        question: question.to_owned(),
        ..AskRequest::default()
    }
}

fn assert_withheld(report: &AskReport, candidate_count: usize) {
    assert!(report.abstained);
    assert!(report.extractiveness_violated);
    assert_eq!(report.confidence, 0.0);
    assert!(report.answer_text.is_none());
    assert!(report.citations.is_empty());
    assert!(report.sides.is_none());
    assert!(report.nearest_evidence.is_none());
    assert!(report.conflict_link.is_none());
    assert_eq!(report.candidates_scanned, candidate_count);
}

fn assert_citations_match_sources(report: &AskReport, candidates: &[AskCandidate]) {
    let sides = report.sides.as_ref().expect("expected conflict sides");
    assert_eq!(sides.len(), 2);
    for side in sides {
        assert!(!side.citations.is_empty());
        let mut parts = Vec::new();
        for (offset, citation) in side.citations.iter().enumerate() {
            let source = candidates
                .iter()
                .find(|candidate| candidate.memory_id == citation.memory_id)
                .expect("citation must name a source candidate");
            assert_eq!(
                source.content.get(citation.byte_start..citation.byte_end),
                Some(citation.text.as_str()),
            );
            assert_eq!(citation.index, offset + 1);
            assert_eq!(citation.provenance_uri, source.provenance_uri);
            assert_eq!(citation.trust_class, source.trust_class);
            parts.push(format!("[{}] {}", citation.index, citation.text));
        }
        assert_eq!(side.answer_text, parts.join(" "));
    }
}

#[test]
fn conflicting_bodies_under_one_identity_withhold_both_sides() {
    let candidates = [
        candidate("same", "Run cargo fmt before release."),
        candidate("same", "Do not run cargo fmt before release."),
    ];
    for candidates in [candidates.to_vec(), candidates.into_iter().rev().collect()] {
        let report = evaluate_ask(&request("Run cargo fmt before release"), &candidates);
        assert_withheld(&report, 2);
    }
}

#[test]
fn explicit_conflict_also_validates_the_linked_source() {
    let mut request = request("production port");
    request.contradictions.push(AskContradiction {
        id: "conflict-port".to_owned(),
        src_memory_id: "port".to_owned(),
        dst_memory_id: "opposition".to_owned(),
        confidence: 1.0,
        source: "human".to_owned(),
    });
    let candidates = [
        candidate("port", "Production port is 443."),
        candidate("opposition", "Use the legacy listener."),
        // The map resolves this identity to a different, shorter body. The
        // explicit edge previously emitted the stale span without checking it.
        candidate("opposition", "é"),
    ];
    let report = evaluate_ask(&request, &candidates);
    assert_withheld(&report, 3);
}

#[test]
fn valid_unicode_conflict_preserves_exact_offsets_and_provenance() {
    let candidates = [
        candidate("yes", "  Enable café mode before release.  "),
        candidate("no", "  Do not enable café mode before release.  "),
    ];
    let report = evaluate_ask(&request("enable café mode before release"), &candidates);
    assert!(!report.abstained);
    assert!(report.conflict_detected);
    assert!(!report.extractiveness_violated);
    assert_citations_match_sources(&report, &candidates);
}

#[test]
fn explicit_conflict_without_negation_still_returns_both_supported_values() {
    let mut request = request("production port");
    request.contradictions.push(AskContradiction {
        id: "conflict-port".to_owned(),
        src_memory_id: "port-a".to_owned(),
        dst_memory_id: "port-b".to_owned(),
        confidence: 1.0,
        source: "human".to_owned(),
    });
    let candidates = [
        candidate("port-a", "Production port is 443."),
        candidate("port-b", "Production port is 8443."),
    ];
    let report = evaluate_ask(&request, &candidates);
    assert!(!report.abstained);
    assert!(report.conflict_detected);
    assert!(report.conflict_link.is_some());
    assert_citations_match_sources(&report, &candidates);
    let sides = report.sides.as_ref().unwrap();
    assert_eq!(sides[0].label, "query_match");
    assert_eq!(sides[1].label, "linked_opposition");
}

#[test]
fn ordinary_answer_invariant_failure_uses_the_same_withholding_contract() {
    let candidates = [
        candidate("same", "Run cargo fmt before release."),
        candidate("same", "é"),
    ];
    let report = evaluate_ask(&request("Run cargo fmt before release"), &candidates);
    assert_withheld(&report, 2);
}

#[test]
fn ordinary_valid_answer_is_still_extractive() {
    let candidates = [candidate("fmt", "  Run cargo fmt before release.  ")];
    let report = evaluate_ask(&request("Run cargo fmt before release"), &candidates);
    assert!(!report.abstained);
    assert!(!report.conflict_detected);
    assert!(!report.extractiveness_violated);
    assert_eq!(report.citations.len(), 1);
    let citation = &report.citations[0];
    assert_eq!(
        candidates[0].content.get(citation.byte_start..citation.byte_end),
        Some(citation.text.as_str()),
    );
}

fn span(id: &str, byte_start: usize, text: &str, score: f32) -> AskSpan {
    AskSpan {
        memory_id: id.to_owned(),
        byte_start,
        byte_end: byte_start + text.len(),
        text: text.to_owned(),
        score,
        trust_class: "human_explicit".to_owned(),
        memory_confidence: 1.0,
        provenance_uri: Some(format!("manual://ask-integrity/{id}")),
        team_provenance: None,
    }
}

#[test]
fn repeated_sentences_from_one_memory_do_not_raise_confidence() {
    let spans = [
        span("one", 0, "Run cargo fmt before release.", 0.5),
        span("one", 30, "Run cargo fmt before release.", 0.5),
        span("one", 60, "Run cargo fmt before release.", 0.5),
    ];
    let clusters = cluster_spans(&spans);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].score, 0.5);
    assert_eq!(clusters[0].byte_start, 0);
}

#[test]
fn repeated_candidate_rows_do_not_count_as_new_memories() {
    let single = span("one", 0, "Run cargo fmt before release.", 0.5);
    let clusters = cluster_spans(&vec![single; 100]);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].score, 0.5);
}

#[test]
fn distinct_memories_still_corroborate_with_the_existing_formula() {
    let spans = [
        span("a", 0, "Run cargo fmt before release.", 0.5),
        span("b", 0, "Run cargo fmt before release.", 0.5),
    ];
    let clusters = cluster_spans(&spans);
    let expected = 0.5 * (1.0 + 0.1 * 2.0_f32.ln());
    assert_eq!(clusters.len(), 1);
    assert!((clusters[0].score - expected).abs() < 1e-6);
}

#[test]
fn repetitions_do_not_amplify_an_existing_two_memory_corroboration() {
    let mut spans = vec![
        span("a", 0, "Run cargo fmt before release.", 0.5),
        span("b", 0, "Run cargo fmt before release.", 0.5),
    ];
    let baseline = cluster_spans(&spans);
    for offset in 1..50 {
        spans.push(span("a", offset * 30, "Run cargo fmt before release.", 0.5));
    }
    let repeated = cluster_spans(&spans);
    assert_eq!(repeated[0].score, baseline[0].score);
}

#[test]
fn corroboration_is_still_capped_for_many_distinct_memories() {
    let spans: Vec<_> = (0..100)
        .map(|n| {
            span(
                &format!("memory-{n:03}"),
                0,
                "Run cargo fmt before release.",
                0.5,
            )
        })
        .collect();
    let clusters = cluster_spans(&spans);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].score, 0.5 * CORROBORATION_CAP);
}

#[test]
fn repetition_cannot_turn_insufficient_evidence_into_an_answer() {
    let request = request("alpha beta gamma delta");
    let mut single = candidate("one", "Alpha beta.");
    single.confidence = 0.5;
    let mut repeated = single.clone();
    repeated.content = std::iter::repeat_n("Alpha beta.", 20)
        .collect::<Vec<_>>()
        .join(" ");
    let original_report = evaluate_ask(&request, &[single]);
    let repeated_report = evaluate_ask(&request, &[repeated]);
    assert!(original_report.abstained);
    assert!(repeated_report.abstained);
    assert_eq!(original_report.confidence, repeated_report.confidence);
    assert_eq!(repeated_report.confidence_components.corroboration, 1.0);
}

#[test]
fn supporting_memory_count_is_independent_of_input_order() {
    let mut spans = vec![
        span("b", 0, "Run cargo fmt before release.", 0.5),
        span("a", 60, "Run cargo fmt before release.", 0.5),
        span("a", 0, "Run cargo fmt before release.", 0.5),
    ];
    let expected = cluster_spans(&spans);
    for _ in 0..spans.len() {
        spans.rotate_left(1);
        let actual = cluster_spans(&spans);
        assert_eq!(actual.len(), expected.len());
        assert_eq!(actual[0].score, expected[0].score);
        assert_eq!(actual[0].memory_id, expected[0].memory_id);
        assert_eq!(actual[0].byte_start, expected[0].byte_start);
    }
}

#[test]
fn opposition_below_the_second_cluster_is_not_hidden() {
    let clusters = [
        span("format", 0, "Run cargo fmt before release.", 0.9),
        span("checksums", 0, "Release checksums protect archives.", 0.8),
        span("opposition", 0, "Do not run cargo fmt before release.", 0.7),
    ];
    assert!(detect_contradiction(&clusters));
}

#[test]
fn unrelated_negative_advice_is_not_a_contradiction() {
    let clusters = [
        span("format", 0, "Run cargo fmt before release.", 0.9),
        span("secrets", 0, "Never print database credentials.", 0.8),
    ];
    assert!(!detect_contradiction(&clusters));
}

#[test]
fn one_shared_generic_term_is_not_enough_to_infer_a_conflict() {
    let clusters = [
        span("format", 0, "Run cargo fmt before release.", 0.9),
        span("secrets", 0, "Do not publish release credentials.", 0.8),
    ];
    assert!(!detect_contradiction(&clusters));
}

#[test]
fn scoped_tls_opposition_remains_detectable() {
    let clusters = [
        span("all", 0, "TLS is required for all connections.", 0.8),
        span("internal", 0, "TLS is not required for internal connections.", 0.7),
    ];
    assert!(detect_contradiction(&clusters));
}

#[test]
fn lower_ranked_opposition_emits_only_the_related_conflict_sides() {
    let mut checksums = candidate("checksums", "Release checksums.");
    checksums.confidence = 0.2;
    let mut opposition = candidate("opposition", "Do not run cargo fmt before release.");
    opposition.confidence = 0.5;
    let candidates = [
        candidate("format", "Run cargo fmt before release."),
        checksums,
        opposition,
    ];
    let request = request("release");
    let terms = tokenize_for_ask(&request.question);
    let scores: Vec<_> = candidates
        .iter()
        .map(|c| score_span(&terms, &c.content, c.confidence, &c.trust_class))
        .collect();
    assert!(scores[0] > scores[1] && scores[1] > scores[2]);
    assert!(scores[2] >= request.min_confidence);

    let report = evaluate_ask(&request, &candidates);
    assert!(!report.abstained);
    assert!(report.conflict_detected);
    assert_citations_match_sources(&report, &candidates);
    let ids: BTreeSet<_> = report
        .sides
        .as_ref()
        .unwrap()
        .iter()
        .flat_map(|side| &side.citations)
        .map(|citation| citation.memory_id.as_str())
        .collect();
    assert_eq!(ids, BTreeSet::from(["format", "opposition"]));
}

#[test]
fn subthreshold_opposition_does_not_suppress_a_supported_answer() {
    let mut opposition = candidate("opposition", "Do not run cargo fmt before release.");
    opposition.confidence = 0.0;
    let candidates = [candidate("format", "Run cargo fmt before release."), opposition];
    let report = evaluate_ask(&request("release"), &candidates);
    assert!(!report.abstained);
    assert!(!report.conflict_detected);
    assert_eq!(report.citations.len(), 1);
    assert_eq!(report.citations[0].memory_id, "format");
}
