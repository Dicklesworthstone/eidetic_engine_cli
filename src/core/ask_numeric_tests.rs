//! Behavior of numeric alternatives through the real ask selection/composition engine.
use super::*;

const FIRST: &str = "The production database service port is 5432.";
const SECOND: &str = "The production database service port is 6432.";
const QUESTION: &str = "What is the production database service port?";

fn candidate(id: &str, content: &str) -> AskCandidate {
    AskCandidate {
        memory_id: id.to_owned(),
        content: content.to_owned(),
        confidence: 1.0,
        trust_class: "human_explicit".to_owned(),
        provenance_uri: Some(format!("manual://numeric-settings/{id}")),
        level: "semantic".to_owned(),
        kind: "note".to_owned(),
        team_provenance: None,
    }
}

fn request() -> AskRequest {
    AskRequest { question: QUESTION.to_owned(), ..AskRequest::default() }
}

fn assert_numeric_answer(report: &AskReport, candidates: &[AskCandidate]) {
    assert!(!report.abstained && !report.extractiveness_violated, "{report:?}");
    assert!(report.conflict_detected);
    assert!(report.conflict_link.is_none(), "inference must not fabricate a stored link");
    assert!(report.answer_text.is_none() && report.citations.is_empty());
    assert_eq!(report.confidence_components.contradiction_penalty, CONTRADICTION_PENALTY);
    let sides = report.sides.as_ref().expect("supported numeric alternatives");
    assert_eq!(sides.len(), 2);
    assert_eq!(sides[0].label, "query_match");
    assert_eq!(sides[1].label, "numeric_alternative");
    for side in sides {
        assert_eq!(side.citations.len(), 1);
        let citation = &side.citations[0];
        let source = candidates.iter().find(|row| row.memory_id == citation.memory_id).expect("cited source");
        assert_eq!(source.content.get(citation.byte_start..citation.byte_end), Some(citation.text.as_str()));
        assert_eq!(citation.provenance_uri, source.provenance_uri);
        assert_eq!(citation.trust_class, source.trust_class);
        assert_eq!(citation.confidence.to_bits(), source.confidence.to_bits());
        assert_eq!(side.answer_text, format!("[1] {}", citation.text));
    }
}

#[test]
fn numeric_assignments_are_disclosed_not_confidently_combined() {
    let rows = [candidate("a", FIRST), candidate("b", SECOND)];
    let report = evaluate_ask(&request(), &rows);
    assert_numeric_answer(&report, &rows);
    assert_eq!(report.confidence_components.corroboration, 1.0);
    let sides = report.sides.as_ref().unwrap();
    assert_eq!(sides[0].citations[0].text, FIRST);
    assert_eq!(sides[1].citations[0].text, SECOND);
    assert!(render_ask_markdown(&report).contains(FIRST));
    assert!(render_ask_markdown(&report).contains(SECOND));
}

#[test]
fn equivalent_values_do_not_manufacture_a_conflict() {
    for second in [FIRST, "The production database service port is +05432.000."] {
        let rows = [candidate("a", FIRST), candidate("b", second)];
        let report = evaluate_ask(&request(), &rows);
        assert!(!report.abstained && !report.conflict_detected);
        assert!(report.sides.is_none());
        assert_eq!(report.confidence_components.contradiction_penalty, 0.0);
    }
    let equal = evaluate_ask(&request(), &[candidate("a", FIRST), candidate("b", FIRST)]);
    assert_eq!(equal.citations.len(), 1);
    assert!(equal.confidence_components.corroboration > 1.0);
}

#[test]
fn negative_numeric_restrictions_are_compatible() {
    let rows = [
        candidate("a", "Do not use port 5432 for the production database service."),
        candidate("b", "Do not use port 6432 for the production database service."),
    ];
    let report = evaluate_ask(&request(), &rows);
    assert!(!report.abstained && !report.conflict_detected);
    assert_eq!(report.citations.len(), 2, "neither compatible restriction is corroboration");
    assert_eq!(report.confidence_components.corroboration, 1.0);
}

#[test]
fn different_subjects_and_ranges_are_not_numeric_disputes() {
    for (left, right) in [
        (FIRST, "The staging database service port is 6432."),
        ("The production database timeout is at least 30 seconds.", "The production database timeout is at least 40 seconds."),
        ("The production database node1 port is 5432.", "The production database node2 port is 6432."),
    ] {
        let rows = [candidate("a", left), candidate("b", right)];
        let report = evaluate_ask(&AskRequest { question: left.to_owned(), ..request() }, &rows);
        assert!(!report.abstained && !report.conflict_detected, "{report:?}");
        assert!(report.sides.is_none());
    }
}

#[test]
fn numeric_opposition_survives_the_candidate_cap_and_input_order() {
    let mut rows: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 8)
        .map(|index| candidate(&format!("a-{index:05}"), FIRST))
        .collect();
    let mut other = candidate("z-opposition", SECOND);
    other.confidence = 0.9;
    rows.push(other);
    let request = request();
    let selected = selection::select_candidates(&request, &tokenize_for_ask(QUESTION), &rows, ASK_CANDIDATE_SCAN_CAP).unwrap();
    assert_eq!(selected.len(), ASK_CANDIDATE_SCAN_CAP);
    assert!(selected.iter().any(|row| row.memory_id == "z-opposition"));
    let report = evaluate_ask(&request, &rows);
    assert_numeric_answer(&report, &rows);
    assert_eq!(report.candidates_scanned, rows.len());
    assert_eq!(report.sides.as_ref().unwrap()[1].citations[0].memory_id, "z-opposition");
    let expected = ask_data_json(&report);
    rows.reverse();
    assert_eq!(ask_data_json(&evaluate_ask(&request, &rows)), expected);
    rows.rotate_left(17);
    assert_eq!(ask_data_json(&evaluate_ask(&request, &rows)), expected);
}

#[test]
fn subthreshold_numeric_claim_cannot_force_a_dispute() {
    let mut rows = [candidate("a", FIRST), candidate("b", SECOND)];
    rows[1].confidence = 0.0;
    let terms = tokenize_for_ask(QUESTION);
    let strong = score_span(&terms, FIRST, 1.0, "human_explicit");
    let weak = score_span(&terms, SECOND, 0.0, "human_explicit");
    assert!(strong > weak);
    let request = AskRequest { min_confidence: (strong + weak) / 2.0, ..request() };
    let report = evaluate_ask(&request, &rows);
    assert!(!report.abstained && !report.conflict_detected);
    assert_eq!(report.citations.len(), 1);
    assert_eq!(report.citations[0].memory_id, "a");
}

#[test]
fn explicit_stored_opposition_keeps_precedence() {
    let rows = [
        candidate("a", FIRST), candidate("b", SECOND),
        candidate("z-linked", "Use the database socket rather than TCP."),
    ];
    let request = AskRequest {
        contradictions: vec![AskContradiction {
            id: "link-port-dispute".to_owned(), src_memory_id: "a".to_owned(),
            dst_memory_id: "z-linked".to_owned(), confidence: 1.0, source: "human".to_owned(),
        }],
        ..request()
    };
    let report = evaluate_ask(&request, &rows);
    assert!(!report.abstained && report.conflict_detected);
    assert_eq!(report.conflict_link.as_ref().unwrap().id, "link-port-dispute");
    let sides = report.sides.as_ref().unwrap();
    assert_eq!(sides[1].label, "linked_opposition");
    assert_eq!(sides[1].citations[0].memory_id, "z-linked");
}

#[test]
fn unrelated_questions_still_abstain_despite_an_internal_numeric_dispute() {
    let rows = [candidate("a", FIRST), candidate("b", SECOND)];
    let report = evaluate_ask(&AskRequest { question: "orbital mechanics lunar trajectory".to_owned(), ..request() }, &rows);
    assert!(report.abstained && !report.conflict_detected);
    assert!(report.answer_text.is_none() && report.citations.is_empty() && report.sides.is_none());
}

#[test]
fn numeric_citations_preserve_multibyte_offsets() {
    let rows = [
        candidate("a", &format!("Préface 🦀.\n\n{FIRST}")),
        candidate("b", &format!("Mémo 🦀.\n\n{SECOND}")),
    ];
    let report = evaluate_ask(&request(), &rows);
    assert_numeric_answer(&report, &rows);
    for (side, row) in report.sides.as_ref().unwrap().iter().zip(&rows) {
        assert!(side.citations[0].byte_start > 0);
        assert_eq!(side.citations[0].byte_end, row.content.len());
    }
}

#[test]
fn a_one_span_budget_does_not_hide_the_other_supported_value() {
    let rows = [candidate("a", FIRST), candidate("b", SECOND)];
    let report = evaluate_ask(&AskRequest { max_evidence: 1, ..request() }, &rows);
    assert_numeric_answer(&report, &rows);
}
