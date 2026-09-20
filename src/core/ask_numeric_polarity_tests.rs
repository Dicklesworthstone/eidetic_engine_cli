//! Numerical subject identity must survive inferred negation and admission.
use super::*;

const FORBIDS_OTHER: &str = "The production database service port is not 6432.";
const FORBIDS_SAME: &str = "The production database service port is not 5432.";

#[test]
fn different_port_and_negative_restriction_are_compatible() {
    let mut rows = [candidate("a", FIRST), candidate("b", FORBIDS_OTHER)];
    let report = evaluate_ask(&request(), &rows);
    assert!(!report.abstained && !report.conflict_detected, "{report:?}");
    assert!(report.sides.is_none() && report.conflict_link.is_none());
    assert_eq!(report.confidence_components.contradiction_penalty, 0.0);
    assert_eq!(report.confidence_components.corroboration, 1.0);
    assert_eq!(report.citations.len(), 2);
    for citation in &report.citations {
        let source = rows.iter().find(|row| row.memory_id == citation.memory_id).unwrap();
        assert_eq!(source.content.get(citation.byte_start..citation.byte_end), Some(citation.text.as_str()));
        assert_eq!(citation.provenance_uri, source.provenance_uri);
    }
    rows.reverse();
    assert_eq!(ask_data_json(&evaluate_ask(&request(), &rows)), ask_data_json(&report));
}

#[test]
fn same_numeric_value_still_has_real_affirming_and_negating_sides() {
    let rows = [candidate("a", FIRST), candidate("b", FORBIDS_SAME)];
    let report = evaluate_ask(&request(), &rows);
    assert!(!report.abstained && report.conflict_detected, "{report:?}");
    assert!(report.conflict_link.is_none());
    assert_eq!(report.confidence_components.contradiction_penalty, CONTRADICTION_PENALTY);
    let sides = report.sides.as_ref().unwrap();
    assert_eq!(sides.len(), 2);
    assert_eq!(sides[0].label, "affirming");
    assert_eq!(sides[1].label, "negating");
    assert_eq!(sides[0].citations[0].text, FIRST);
    assert_eq!(sides[1].citations[0].text, FORBIDS_SAME);
}

#[test]
fn a_conflict_side_does_not_absorb_a_compatible_different_value() {
    let rows = [
        candidate("a", FIRST),
        candidate("b", FORBIDS_SAME),
        candidate("z-compatible", FORBIDS_OTHER),
    ];
    let report = evaluate_ask(&request(), &rows);
    assert!(!report.abstained && report.conflict_detected, "{report:?}");
    let sides = report.sides.as_ref().unwrap();
    assert_eq!(sides[0].citations.len(), 1);
    assert_eq!(sides[1].citations.len(), 1);
    assert_eq!(sides[0].citations[0].memory_id, "a");
    assert_eq!(sides[1].citations[0].memory_id, "b");
    assert!(!sides.iter().flat_map(|side| &side.citations)
        .any(|citation| citation.memory_id == "z-compatible"));
}

#[test]
fn compatible_negation_cannot_steal_the_numeric_opposition_slot() {
    let mut rows: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 8)
        .map(|index| candidate(&format!("a-support-{index:05}"), FIRST))
        .collect();
    rows.push(candidate("b-compatible", FORBIDS_OTHER));
    let mut alternative = candidate("z-opposition", SECOND);
    alternative.confidence = 0.9;
    rows.push(alternative);
    let request = request();
    let selected = selection::select_candidates(
        &request, &tokenize_for_ask(QUESTION), &rows, ASK_CANDIDATE_SCAN_CAP,
    ).unwrap();
    assert_eq!(selected.len(), ASK_CANDIDATE_SCAN_CAP);
    assert!(selected.iter().any(|row| row.memory_id == "z-opposition"));
    assert!(!selected.iter().any(|row| row.memory_id == "b-compatible"));
    let report = evaluate_ask(&request, &rows);
    assert_numeric_answer(&report, &rows);
    assert_eq!(report.sides.as_ref().unwrap()[1].citations[0].memory_id, "z-opposition");
    rows.reverse();
    assert_eq!(ask_data_json(&evaluate_ask(&request, &rows)), ask_data_json(&report));
}
