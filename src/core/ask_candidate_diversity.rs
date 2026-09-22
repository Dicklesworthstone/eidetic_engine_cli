//! Keep independent evidence reachable through a bounded candidate budget.

use std::collections::{BTreeMap, BTreeSet};

use super::super::{CORROBORATION_CAP, native};
use super::{AskCandidate, AskRequest, RankedCandidate, SpanScorer, best_span_score};

fn support_key<'a>(id: &'a str, groups: &'a BTreeMap<String, String>) -> &'a str {
    groups.get(id).map_or(id, String::as_str)
}

/// Prefer each lineage's best plausible answer before spending spare slots on
/// repeated excerpts. Use the complete request-local scorer, not confidence or
/// input order. Evidence below even the maximum corroboration-adjusted floor
/// cannot evict a relevant passage merely because it has a different origin.
///
/// Lineage metadata is linear in the already-scoped corpus; both ranked sets
/// are bounded by the existing admission limit. Neither bodies nor spans are
/// cloned. Explicit/inferred conflict reservations run afterwards and retain
/// priority over diversity. The final order remains the ordinary score order.
pub(super) fn preserve_independent_support<'a>(
    request: &AskRequest,
    question_terms: &[String],
    unique: &BTreeMap<&str, &'a AskCandidate>,
    ranked: &mut [RankedCandidate<'a>],
    scorer: SpanScorer<'_>,
) {
    if ranked.len() < 2 || unique.len() <= ranked.len() {
        return;
    }
    let groups =
        native::candidate_support_groups(unique.values().copied(), &request.native_sources);
    if groups.is_empty() {
        return;
    }
    let minimum = request.min_confidence / CORROBORATION_CAP;
    let mut representatives: BTreeMap<&str, RankedCandidate<'a>> = BTreeMap::new();
    let mut best: BTreeSet<RankedCandidate<'a>> = BTreeSet::new();
    for candidate in unique.values().copied() {
        if candidate.content.trim().is_empty() {
            continue;
        }
        let entry = RankedCandidate {
            candidate,
            score: best_span_score(question_terms, candidate, scorer),
        };
        if entry.score < minimum {
            continue;
        }
        let key = support_key(&candidate.memory_id, &groups);
        if let Some(previous) = representatives.get(key).copied() {
            if entry >= previous {
                continue;
            }
            best.remove(&previous);
        }
        representatives.insert(key, entry);
        best.insert(entry);
        if best.len() > ranked.len() {
            // Invariant: `best.insert(entry)` runs unconditionally two lines
            // above, and the guard compares usizes, so `best.len()` is at least
            // `ranked.len() + 1` and therefore at least one.
            #[allow(clippy::expect_used)]
            let worst = best.pop_last().expect("over-budget selection is nonempty");
            representatives.remove(support_key(&worst.candidate.memory_id, &groups));
        }
    }

    let mut selected: Vec<_> = best.into_iter().collect();
    let mut selected_ids: BTreeSet<_> = selected
        .iter()
        .map(|entry| entry.candidate.memory_id.as_str())
        .collect();
    for entry in ranked.iter().copied() {
        if selected.len() == ranked.len() {
            break;
        }
        if selected_ids.insert(entry.candidate.memory_id.as_str()) {
            selected.push(entry);
        }
    }
    selected.sort();
    ranked.copy_from_slice(&selected);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ask::selection::select_candidates_with_scorer;
    use crate::core::ask::{
        ASK_CANDIDATE_SCAN_CAP, AskContradiction, AskNativeSource, AskSpan, clustering,
    };
    use crate::models::{MemoryId, RuleId};
    use crate::pack::PackEntityRef;

    fn candidate(id: &str, score: f32, uri: &str) -> AskCandidate {
        AskCandidate {
            memory_id: id.to_owned(),
            content: "Use the cache for repeated reads.".to_owned(),
            confidence: score,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some(uri.to_owned()),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            team_provenance: None,
        }
    }

    fn select<'a>(
        request: &AskRequest,
        rows: &'a [AskCandidate],
        limit: usize,
    ) -> Vec<&'a AskCandidate> {
        select_candidates_with_scorer(request, &[], rows, limit, &|_, _, score, _| score)
            .expect("valid candidates")
    }

    fn ids(rows: &[&AskCandidate]) -> Vec<String> {
        rows.iter().map(|row| row.memory_id.clone()).collect()
    }

    fn spans(rows: &[&AskCandidate]) -> Vec<AskSpan> {
        rows.iter()
            .map(|row| AskSpan {
                memory_id: row.memory_id.clone(),
                byte_start: 0,
                byte_end: row.content.len(),
                text: row.content.clone(),
                score: row.confidence,
                trust_class: row.trust_class.clone(),
                memory_confidence: row.confidence,
                provenance_uri: row.provenance_uri.clone(),
                team_provenance: None,
            })
            .collect()
    }

    #[test]
    fn independent_corroboration_survives_a_corpus_larger_than_the_scan_cap() {
        for prefix in [
            "cass-session://conversation#L",
            "file://decisions.md#L",
            "https://example.test/decisions#section-",
        ] {
            let request = AskRequest::default();
            // Line references are one-based. L0 is invalid provenance, not
            // another excerpt of the same document or session.
            let mut rows: Vec<_> = (1..=ASK_CANDIDATE_SCAN_CAP + 4)
                .map(|index| candidate(&format!("a-{index:05}"), 0.54, &format!("{prefix}{index}")))
                .collect();
            rows.push(candidate("z-independent", 0.54, "file://independent.md#L1"));
            for row in &rows {
                let uri = row.provenance_uri.as_deref().expect("fixture provenance");
                assert!(uri.parse::<crate::models::ProvenanceUri>().is_ok(), "{uri}");
            }
            let selected = select(&request, &rows, ASK_CANDIDATE_SCAN_CAP);
            assert_eq!(selected.len(), ASK_CANDIDATE_SCAN_CAP);
            assert!(selected.iter().any(|row| row.memory_id == "z-independent"));
            let admitted = spans(&selected);
            let groups = native::support_groups(&admitted, &request.native_sources);
            assert_eq!(groups.values().collect::<BTreeSet<_>>().len(), 2);
            let clusters = clustering::cluster_spans_with_groups(&admitted, &groups);
            assert_eq!(clusters.len(), 1);
            assert!(clusters[0].score > request.min_confidence);
            assert!((clusters[0].score - 0.54 * (1.0 + 0.1 * 2.0_f32.ln())).abs() < 1e-6);

            // Repeated excerpts alone still must not manufacture confidence.
            rows.pop();
            let admitted = spans(&select(&request, &rows, ASK_CANDIDATE_SCAN_CAP));
            let groups = native::support_groups(&admitted, &request.native_sources);
            let clusters = clustering::cluster_spans_with_groups(&admitted, &groups);
            assert_eq!(clusters[0].score, 0.54);
            assert!(clusters[0].score < request.min_confidence);
        }
    }

    #[test]
    fn raw_anchor_order_and_spare_duplicate_slots_are_preserved() {
        let rows = vec![
            candidate("a", 0.9, "file://one.md#L1"),
            candidate("b", 0.8, "file://one.md#L2"),
            candidate("c", 0.7, "file://one.md#L3"),
            candidate("z", 0.5, "file://two.md#L1"),
        ];
        let request = AskRequest::default();
        assert_eq!(ids(&select(&request, &rows, 3)), ["a", "b", "z"]);
        assert_eq!(ids(&select(&request, &rows, 2)), ["a", "z"]);
        assert_eq!(ids(&select(&request, &rows, 1)), ["a"]);
        assert!(select(&request, &rows, 0).is_empty());
        assert_eq!(ids(&select(&request, &rows, 20)), ["a", "b", "c", "z"]);
    }

    #[test]
    fn evidence_below_the_reachable_floor_does_not_evict_relevant_duplicates() {
        let rows = vec![
            candidate("a", 0.9, "file://one.md#L1"),
            candidate("b", 0.8, "file://one.md#L2"),
            candidate("z", 0.1, "file://two.md#L1"),
        ];
        assert_eq!(ids(&select(&AskRequest::default(), &rows, 2)), ["a", "b"]);
    }

    #[test]
    fn best_group_representatives_are_stable_under_input_permutations() {
        let request = AskRequest::default();
        let mut rows = vec![
            candidate("a-weak", 0.5, "file://one.md#L1"),
            candidate("b", 0.8, "file://two.md#L1"),
            candidate("c", 0.7, "file://three.md#L1"),
            candidate("y-copy", 0.9, "file://one.md#L2"),
            candidate("z-copy", 0.9, "file://one.md#L3"),
        ];
        for _ in 0..rows.len() {
            assert_eq!(ids(&select(&request, &rows, 2)), ["y-copy", "b"]);
            rows.reverse();
            assert_eq!(ids(&select(&request, &rows, 2)), ["y-copy", "b"]);
            rows.reverse();
            rows.rotate_left(1);
        }
    }

    #[test]
    fn opaque_capture_labels_are_not_treated_as_one_document() {
        let rows = vec![
            candidate("a", 0.9, "manual://cli"),
            candidate("b", 0.8, "manual://cli"),
            candidate("z", 0.7, "file://other.md#L1"),
        ];
        assert_eq!(ids(&select(&AskRequest::default(), &rows, 2)), ["a", "b"]);
    }

    #[test]
    fn native_derivation_and_candidate_grouping_match_final_span_lineage() {
        let parent = MemoryId::from_uuid(uuid::Uuid::from_u128(101)).to_string();
        let rule_id = RuleId::from_uuid(uuid::Uuid::from_u128(102));
        let mut request = AskRequest::default();
        request.native_sources.insert(
            rule_id.to_string(),
            AskNativeSource {
                entity: PackEntityRef::Rule(rule_id),
                entity_revision: format!("blake3:{}", "0".repeat(64)),
                source_memory_ids: vec![parent.clone()],
            },
        );
        let rows = vec![
            candidate(&parent, 0.9, "cass-session://one#L1"),
            candidate(&rule_id.to_string(), 0.8, "manual://rule"),
            candidate("excerpt", 0.7, "cass-session://one#L2"),
            candidate("independent", 0.6, "file://independent.md#L1"),
        ];
        let groups = native::candidate_support_groups(rows.iter(), &request.native_sources);
        let all: Vec<_> = rows.iter().collect();
        assert_eq!(
            groups,
            native::support_groups(&spans(&all), &request.native_sources)
        );
        assert_eq!(groups.get(&parent), groups.get(&rule_id.to_string()));
        assert_eq!(groups.get(&parent), groups.get("excerpt"));
        assert_ne!(groups.get(&parent), groups.get("independent"));
        assert_eq!(
            ids(&select(&request, &rows, 2)),
            [parent, "independent".to_owned()]
        );
    }

    fn crowded_native_derivations() -> (AskRequest, Vec<AskCandidate>, Vec<String>) {
        let mut request = AskRequest {
            question: "Use the cache for repeated reads".to_owned(),
            ..AskRequest::default()
        };
        let mut rows = Vec::new();
        let mut parents = Vec::new();
        for number in 1..=2 {
            let parent = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            let rule = RuleId::from_uuid(uuid::Uuid::from_u128(number + 10));
            let mut source = candidate(
                &parent,
                0.1,
                &format!("cass-session://shared-incident#L{number}"),
            );
            source.content = "Historical incident source material.".to_owned();
            rows.push(source);
            rows.push(candidate(&rule.to_string(), 0.54, "manual://derived-rule"));
            request.native_sources.insert(
                rule.to_string(),
                AskNativeSource {
                    entity: PackEntityRef::Rule(rule),
                    entity_revision: format!("blake3:{}", "0".repeat(64)),
                    source_memory_ids: vec![parent.clone()],
                },
            );
            parents.push(parent);
        }
        // These unrelated passages outrank the historical connectors, but
        // share one origin and cannot corroborate either answer statement.
        for index in 0..ASK_CANDIDATE_SCAN_CAP {
            let id = MemoryId::from_uuid(uuid::Uuid::from_u128(index as u128 + 100));
            let mut row = candidate(
                &id.to_string(),
                0.53,
                &format!("file://inventory.md#L{}", index + 1),
            );
            row.content = "Unrelated inventory notes.".to_owned();
            rows.push(row);
        }
        (request, rows, parents)
    }

    #[test]
    fn dropping_lineage_connectors_cannot_turn_correlated_rules_into_an_answer() {
        let (request, mut rows, parents) = crowded_native_derivations();
        let selected = select(&request, &rows, ASK_CANDIDATE_SCAN_CAP);
        assert_eq!(selected.len(), ASK_CANDIDATE_SCAN_CAP);
        assert!(
            parents
                .iter()
                .all(|parent| { selected.iter().all(|row| row.memory_id != *parent) })
        );
        assert!(
            request
                .native_sources
                .keys()
                .all(|id| { selected.iter().any(|row| row.memory_id == *id) })
        );

        let score = |_: &[String], _: &str, confidence: f32, _: &str| confidence;
        let report = crate::core::ask::evaluate_ask_scored(&request, &rows, &score, false);
        assert!(
            report.abstained,
            "one origin cannot cross the evidence floor"
        );
        assert!(!report.extractiveness_violated);
        assert_eq!(report.confidence, 0.54);
        assert_eq!(report.confidence_components.corroboration, 1.0);
        let data = crate::core::ask::ask_data_json(&report);
        for parent in &parents {
            assert!(!data.to_string().contains(parent));
        }
        rows.reverse();
        assert_eq!(
            data,
            crate::core::ask::ask_data_json(&crate::core::ask::evaluate_ask_scored(
                &request, &rows, &score, false,
            ))
        );

        // A genuinely separate source still provides the second independent
        // observation. Retaining old lineage must not suppress real support.
        rows.push(candidate(
            "independent-answer",
            0.54,
            "file://independent-observation.md#L1",
        ));
        let independent = crate::core::ask::evaluate_ask_scored(&request, &rows, &score, false);
        assert!(!independent.abstained);
        assert!(!independent.conflict_detected);
        let expected = 0.54 * (1.0 + 0.1 * 2.0_f32.ln());
        assert!((independent.confidence - expected).abs() < 1e-6);
    }

    #[test]
    fn dropped_connectors_preserve_opposition_without_inflating_either_side() {
        let (request, mut rows, parents) = crowded_native_derivations();
        for row in &mut rows {
            if request.native_sources.contains_key(&row.memory_id) {
                row.confidence = 0.8;
            } else if !parents.contains(&row.memory_id) {
                row.confidence = 0.79;
            }
        }
        let mut opposing = candidate(
            "independent-opposition",
            0.8,
            "file://opposing-observation.md#L1",
        );
        opposing.content = "Never use the cache for repeated reads.".to_owned();
        rows.push(opposing);
        let selected = select(&request, &rows, ASK_CANDIDATE_SCAN_CAP);
        assert!(
            parents
                .iter()
                .all(|parent| { selected.iter().all(|row| row.memory_id != *parent) })
        );
        let score = |_: &[String], _: &str, confidence: f32, _: &str| confidence;
        let report = crate::core::ask::evaluate_ask_scored(&request, &rows, &score, false);
        assert!(!report.abstained);
        assert!(report.conflict_detected);
        assert!(!report.extractiveness_violated);
        assert_eq!(report.confidence_components.top_span_score, 0.8);
        assert_eq!(report.confidence_components.corroboration, 1.0);
        assert_eq!(report.confidence_components.contradiction_penalty, 0.4);
        assert!((report.confidence - 0.8 * (1.0 - 0.4)).abs() < 1e-6);
        let sides = report.sides.as_ref().expect("both supported sides");
        assert_eq!(sides.len(), 2);
        assert!(sides.iter().any(|side| {
            side.citations.iter().any(|citation| {
                request.native_sources.contains_key(&citation.memory_id)
                    && citation.text == "Use the cache for repeated reads."
            })
        }));
        assert!(sides.iter().any(|side| {
            side.citations.iter().any(|citation| {
                citation.memory_id == "independent-opposition"
                    && citation.text == "Never use the cache for repeated reads."
            })
        }));
        let data = crate::core::ask::ask_data_json(&report);
        for parent in parents {
            assert!(!data.to_string().contains(&parent));
        }
        rows.reverse();
        assert_eq!(
            data,
            crate::core::ask::ask_data_json(&crate::core::ask::evaluate_ask_scored(
                &request, &rows, &score, false,
            ))
        );
    }

    #[test]
    fn complete_scorer_not_source_confidence_controls_diversity_and_its_floor() {
        let mut rows = vec![
            candidate("a", 0.1, "file://one.md#L1"),
            candidate("b", 1.0, "file://one.md#L2"),
            candidate("y", 0.1, "file://two.md#L1"),
            candidate("z", 1.0, "file://three.md#L1"),
        ];
        for (row, text) in rows.iter_mut().zip([
            "Primary evidence.",
            "Repeated evidence.",
            "Independent evidence.",
            "Unrelated evidence.",
        ]) {
            row.content = text.to_owned();
        }
        let scorer = |_: &[String], text: &str, _: f32, _: &str| {
            if text.starts_with("Primary") {
                0.9
            } else if text.starts_with("Repeated") {
                0.8
            } else if text.starts_with("Independent") {
                0.5
            } else {
                0.2
            }
        };
        let selected =
            select_candidates_with_scorer(&AskRequest::default(), &[], &rows, 2, &scorer)
                .expect("valid candidates");
        assert_eq!(ids(&selected), ["a", "y"]);
    }

    #[test]
    fn explicit_conflict_reservation_has_priority_over_diversity() {
        let mut request = AskRequest::default();
        request.contradictions.push(AskContradiction {
            id: "conflict".to_owned(),
            src_memory_id: "a".to_owned(),
            dst_memory_id: "opposed".to_owned(),
            confidence: 0.9,
            source: "human".to_owned(),
        });
        let rows = vec![
            candidate("a", 0.95, "file://one.md#L1"),
            candidate("b", 0.9, "file://one.md#L2"),
            candidate("independent", 0.85, "file://two.md#L1"),
            candidate("opposed", 0.7, "file://three.md#L1"),
        ];
        assert_eq!(ids(&select(&request, &rows, 2)), ["a", "opposed"]);
    }
}
