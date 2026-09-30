//! Reclaim redundant exact-copy slots for distinct, already-supported answers.
//!
//! This is not semantic deduplication: bodies and every span-scoring input
//! must match exactly. Independent provenance remains independent, and enough
//! of it is retained to reach the existing corroboration cap. A new body must
//! clear the ordinary evidence floor, alone or with a complete independently
//! corroborating bundle. Selecting a bundle never changes a source's score.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AskCandidate, AskRequest, CORROBORATION_CAP, RankedCandidate, SpanScorer, best_span_score,
    support_key,
};

type SourceSignature<'a> = (&'a str, u32, &'a str);

fn source_signature(candidate: &AskCandidate) -> SourceSignature<'_> {
    (
        candidate.content.as_str(),
        candidate.confidence.to_bits(),
        candidate.trust_class.as_str(),
    )
}

/// Run after lineage diversity and before the existing opposition reservations.
/// Copies of an already represented lineage, or independent copies beyond full
/// corroboration, are exchangeable. Keep the raw anchor, original source IDs,
/// exact bodies, and all weaker evidence unless a supported, previously
/// unrepresented body can use redundant slots. Selection frontiers are bounded
/// by the budget. The optional bundle pass sorts borrowed corpus references,
/// never cloned bodies or an unbounded span pool.
pub(super) fn preserve_distinct_answers<'a>(
    request: &AskRequest,
    question_terms: &[String],
    unique: &BTreeMap<&str, &'a AskCandidate>,
    ranked: &mut [RankedCandidate<'a>],
    groups: &BTreeMap<String, String>,
    scorer: SpanScorer<'_>,
) {
    if ranked.len() < 2 {
        return;
    }
    // Match clustering's capped logarithmic multiplier. A smaller budget may
    // never reach the cap, but copies of one lineage are still redundant.
    // Retain every independent vote until the cap is actually reached.
    let support_limit = (1..ranked.len())
        .find(|&count| 1.0 + 0.1 * (count as f32).ln() >= CORROBORATION_CAP)
        .unwrap_or(ranked.len());
    let mut support: BTreeMap<SourceSignature<'_>, BTreeSet<&str>> = BTreeMap::new();
    let mut retained = Vec::with_capacity(ranked.len());
    let mut redundant = Vec::new();
    let mut admitted_bodies = BTreeSet::new();
    for entry in ranked.iter().copied() {
        let candidate = entry.candidate;
        admitted_bodies.insert(candidate.content.as_str());
        // Matching only the best sentence would lose other facts in the body.
        // Matching only the body could discard a distinct trust/confidence
        // input that changes scores for its other spans. Require all three.
        let origins = support.entry(source_signature(candidate)).or_default();
        if origins.len() >= support_limit
            || !origins.insert(support_key(&candidate.memory_id, groups))
        {
            redundant.push(entry);
        } else {
            retained.push(entry);
        }
    }
    if redundant.is_empty() {
        return;
    }

    // Pick the best distinct bodies, not thousands of replicas of the next
    // body. Both maps contain at most the number of reclaimable slots. Scope
    // and identity validation already happened before this selection pass.
    let limit = redundant.len();
    let mut by_body: BTreeMap<&str, RankedCandidate<'a>> = BTreeMap::new();
    let mut best: BTreeSet<RankedCandidate<'a>> = BTreeSet::new();
    for candidate in unique.values().copied() {
        if candidate.content.trim().is_empty()
            || admitted_bodies.contains(candidate.content.as_str())
        {
            continue;
        }
        let entry = RankedCandidate {
            candidate,
            score: best_span_score(question_terms, candidate, scorer),
        };
        if !entry.score.is_finite() || entry.score <= 0.0 || entry.score < request.min_confidence {
            continue;
        }
        if let Some(previous) = by_body.get(candidate.content.as_str()).copied() {
            if entry >= previous {
                continue;
            }
            best.remove(&previous);
        }
        by_body.insert(candidate.content.as_str(), entry);
        best.insert(entry);
        if best.len() > limit
            && let Some(worst) = best.pop_last()
        {
            by_body.remove(worst.candidate.content.as_str());
        }
    }
    for entry in &best {
        admitted_bodies.insert(entry.candidate.content.as_str());
    }
    retained.extend(best);
    // Individually supported answers retain priority. Otherwise unused slots
    // can carry ALL the sources needed for an independently supported answer;
    // taking one subthreshold source would not make that answer reachable.
    let available = ranked.len() - retained.len();
    let corroborated = corroborated_answers(
        request,
        question_terms,
        unique,
        &admitted_bodies,
        groups,
        scorer,
        available,
        support_limit,
    );
    retained.extend(corroborated);
    let spare = ranked.len() - retained.len();
    retained.extend(redundant.into_iter().take(spare));
    retained.sort();
    ranked.copy_from_slice(&retained);
}

/// Find complete exact-body support bundles, never speculative partial ones.
/// A sorted vector of borrowed candidates permits one pass over each signature
/// without a map of corpus-sized support sets. For a cost k, at most limit/k
/// bundles can ever fit. Retaining only that many best bundles of each cost
/// preserves score-ordered greedy selection, including smaller bundles that
/// still fit after a higher-ranked large bundle has consumed most of the space.
#[allow(clippy::too_many_arguments)]
fn corroborated_answers<'a>(
    request: &AskRequest,
    question_terms: &[String],
    unique: &BTreeMap<&str, &'a AskCandidate>,
    admitted_bodies: &BTreeSet<&str>,
    groups: &BTreeMap<String, String>,
    scorer: SpanScorer<'_>,
    limit: usize,
    support_limit: usize,
) -> Vec<RankedCandidate<'a>> {
    use crate::core::ask::{segment_spans, tokenize_for_ask};

    let max_support = support_limit.min(limit);
    if max_support < 2 {
        return Vec::new();
    }
    let mut candidates: Vec<_> = unique
        .values()
        .copied()
        .filter(|candidate| {
            !candidate.content.trim().is_empty()
                && !admitted_bodies.contains(candidate.content.as_str())
        })
        .collect();
    candidates.sort_by(|left, right| {
        source_signature(left)
            .cmp(&source_signature(right))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    let mut by_cost: Vec<Vec<Vec<RankedCandidate<'a>>>> =
        (0..=max_support).map(|_| Vec::new()).collect();
    for body in candidates.chunk_by(|left, right| left.content == right.content) {
        let mut best_bundle: Option<Vec<RankedCandidate<'a>>> = None;
        for signature in
            body.chunk_by(|left, right| source_signature(left) == source_signature(right))
        {
            // chunk_by only yields nonempty slices, ordered by source ID
            // within one body/confidence/trust signature.
            let first = signature[0];
            let score = best_span_score(question_terms, first, scorer);
            if !score.is_finite() || score <= 0.0 || score >= request.min_confidence {
                continue;
            }
            // Clustering requires a posting hit even for exact copies. A
            // symbol-only span (or all stopwords) cannot corroborate merely
            // because a semantic scorer returns a positive number for it.
            let support_score = segment_spans(&first.content)
                .into_iter()
                .filter_map(|(start, end)| {
                    let text = &first.content[start..end];
                    if tokenize_for_ask(text).is_empty() {
                        return None;
                    }
                    let score = scorer(question_terms, text, first.confidence, &first.trust_class);
                    (score.is_finite() && score > 0.0).then_some(score)
                })
                .fold(0.0, f32::max);
            let Some(needed) = (2..=max_support).find(|&count| {
                let multiplier = (1.0 + 0.1 * (count as f32).ln()).min(CORROBORATION_CAP);
                support_score * multiplier >= request.min_confidence
            }) else {
                continue;
            };
            let mut origins = BTreeSet::new();
            let mut bundle = Vec::with_capacity(needed);
            for &candidate in signature {
                if origins.insert(support_key(&candidate.memory_id, groups)) {
                    bundle.push(RankedCandidate { candidate, score });
                    if bundle.len() == needed {
                        break;
                    }
                }
            }
            if bundle.len() != needed {
                continue;
            }
            if best_bundle
                .as_ref()
                .is_none_or(|previous| bundle[0] < previous[0])
            {
                best_bundle = Some(bundle);
            }
        }
        if let Some(bundle) = best_bundle {
            let cost = bundle.len();
            let frontier = &mut by_cost[cost];
            frontier.push(bundle);
            frontier.sort_by(|left, right| left[0].cmp(&right[0]));
            frontier.truncate(limit / cost);
        }
    }
    let mut bundles: Vec<_> = by_cost.into_iter().flatten().collect();
    bundles.sort_by(|left, right| left[0].cmp(&right[0]));
    let mut selected = Vec::with_capacity(limit);
    for bundle in bundles {
        if bundle.len() <= limit - selected.len() {
            selected.extend(bundle);
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ask::selection::select_candidates_with_scorer;
    use crate::core::ask::{
        ASK_CANDIDATE_SCAN_CAP, AskSpan, ask_data_json, clustering, evaluate_ask,
        evaluate_ask_scored, native,
    };

    const COMMON: &str = "The cache read path reuses cached responses.";
    const DISTINCT: &str = "Migration checks validate schema compatibility before deployment.";

    fn candidate(id: &str, body: &str, confidence: f32) -> AskCandidate {
        AskCandidate {
            memory_id: id.to_owned(),
            content: body.to_owned(),
            confidence,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some("manual://cli".to_owned()),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            team_provenance: None,
        }
    }

    fn corpus() -> Vec<AskCandidate> {
        let mut rows: Vec<_> = (0..80)
            .map(|index| candidate(&format!("a-{index:05}"), COMMON, 0.7))
            .collect();
        rows.push(candidate("z-first", DISTINCT, 0.54));
        rows.push(candidate("z-second", DISTINCT, 0.54));
        rows
    }

    fn select(rows: &[AskCandidate], limit: usize) -> Vec<&AskCandidate> {
        select_candidates_with_scorer(
            &AskRequest::default(),
            &[],
            rows,
            limit,
            &|_, _, confidence, _| confidence,
        )
        .expect("valid source identities")
    }

    fn distinct_ids<'a>(rows: &[&'a AskCandidate]) -> Vec<&'a str> {
        rows.iter()
            .filter(|row| row.content == DISTINCT)
            .map(|row| row.memory_id.as_str())
            .collect()
    }

    #[test]
    fn a_complete_independent_bundle_survives_saturated_results() {
        let rows = corpus();
        let selected = select(&rows, 32);
        assert_eq!(selected.len(), 32);
        assert_eq!(selected[0].memory_id, "a-00000");
        assert_eq!(distinct_ids(&selected), ["z-first", "z-second"]);
        let spans: Vec<_> = selected
            .iter()
            .map(|row| AskSpan {
                memory_id: row.memory_id.clone(),
                byte_start: 0,
                byte_end: row.content.len(),
                text: row.content.clone(),
                score: row.confidence,
                memory_confidence: row.confidence,
                trust_class: row.trust_class.clone(),
                provenance_uri: row.provenance_uri.clone(),
                team_provenance: None,
            })
            .collect();
        let groups = native::candidate_support_groups(rows.iter(), &BTreeMap::new());
        let clusters = clustering::cluster_spans_with_groups(&spans, &groups);
        assert_eq!(clusters.len(), 2);
        let distinct = clusters.iter().find(|span| span.text == DISTINCT).unwrap();
        let expected = 0.54 * (1.0 + 0.1 * 2.0_f32.ln());
        assert_eq!(distinct.score.to_bits(), expected.to_bits());
        assert!(distinct.score >= AskRequest::default().min_confidence);
        assert_eq!(distinct.memory_confidence, 0.54);
        let common = clusters.iter().find(|span| span.text == COMMON).unwrap();
        assert_eq!(common.score.to_bits(), (0.7 * CORROBORATION_CAP).to_bits());
    }

    #[test]
    fn admission_is_all_or_nothing_and_never_grows_the_budget() {
        let rows = corpus();
        for limit in 0..=40 {
            let selected = select(&rows, limit);
            assert_eq!(selected.len(), limit);
            assert_eq!(distinct_ids(&selected).len(), if limit >= 23 { 2 } else { 0 });
        }
        let selected = select(&rows, rows.len() + 10);
        assert_eq!(selected.len(), rows.len());
        assert_eq!(distinct_ids(&selected).len(), 2);
    }

    #[test]
    fn copies_of_one_origin_do_not_become_independent_support() {
        let mut rows = corpus();
        for (index, row) in rows
            .iter_mut()
            .filter(|row| row.content == DISTINCT)
            .enumerate()
        {
            row.provenance_uri = Some(format!("file://one-observation.md#L{}", index + 1));
        }
        let selected = select(&rows, 32);
        assert!(distinct_ids(&selected).is_empty());
        assert_eq!(selected.len(), 32);
    }

    #[test]
    fn support_requires_identical_bodies_and_scoring_inputs() {
        for variant in 0..3 {
            let mut rows = corpus();
            let last = rows.last_mut().unwrap();
            match variant {
                0 => last.content.push_str(" Additional context."),
                1 => last.confidence = 0.53,
                _ => last.trust_class = "cass_evidence".to_owned(),
            }
            let selected = select(&rows, 32);
            assert!(selected.iter().all(|row| row.content == COMMON));
        }
        let mut rows = corpus();
        rows.pop();
        assert!(distinct_ids(&select(&rows, 32)).is_empty());
        for row in rows.iter_mut().filter(|row| row.content == DISTINCT) {
            row.confidence = 0.1;
        }
        assert!(distinct_ids(&select(&rows, 32)).is_empty());
    }

    #[test]
    fn larger_bundles_need_the_actual_number_of_independent_sources() {
        let mut rows = corpus();
        for row in rows.iter_mut().filter(|row| row.content == DISTINCT) {
            row.confidence = 0.5;
        }
        assert!(distinct_ids(&select(&rows, 32)).is_empty());
        rows.push(candidate("z-third", DISTINCT, 0.5));
        assert!(distinct_ids(&select(&rows, 23)).is_empty());
        assert_eq!(
            distinct_ids(&select(&rows, 24)),
            ["z-first", "z-second", "z-third"]
        );
        assert!(0.5 * (1.0 + 0.1 * 2.0_f32.ln()) < AskRequest::default().min_confidence);
        assert!(0.5 * (1.0 + 0.1 * 3.0_f32.ln()) >= AskRequest::default().min_confidence);
    }

    #[test]
    fn bundle_admission_uses_the_complete_scorer_without_rewriting_confidence() {
        let mut rows = corpus();
        for row in rows.iter_mut().filter(|row| row.content == DISTINCT) {
            row.confidence = 0.1;
        }
        let scorer = |_: &[String], text: &str, _: f32, _: &str| {
            if text == DISTINCT { 0.54 } else { 0.7 }
        };
        let selected =
            select_candidates_with_scorer(&AskRequest::default(), &[], &rows, 32, &scorer)
                .expect("valid source identities");
        assert_eq!(distinct_ids(&selected), ["z-first", "z-second"]);
        assert!(
            selected
                .iter()
                .filter(|row| row.content == DISTINCT)
                .all(|row| row.confidence == 0.1)
        );
    }

    #[test]
    fn individually_supported_answers_keep_priority_over_bundles() {
        let mut rows = corpus();
        for index in 0..11 {
            rows.push(candidate(
                &format!("y-{index:05}"),
                &format!("Independent supported observation {index}."),
                0.6,
            ));
        }
        let selected = select(&rows, 32);
        assert_eq!(selected.len(), 32);
        assert!(distinct_ids(&selected).is_empty());
        assert_eq!(
            selected
                .iter()
                .filter(|row| row.memory_id.starts_with("y-"))
                .count(),
            11
        );
    }

    #[test]
    fn bundles_are_deterministic_under_corpus_permutations() {
        let mut rows = corpus();
        let expected: Vec<_> = select(&rows, 32)
            .iter()
            .map(|row| row.memory_id.clone())
            .collect();
        for _ in 0..8 {
            rows.reverse();
            assert_eq!(
                select(&rows, 32)
                    .iter()
                    .map(|row| row.memory_id.clone())
                    .collect::<Vec<_>>(),
                expected,
            );
            rows.rotate_left(7);
            assert_eq!(
                select(&rows, 32)
                    .iter()
                    .map(|row| row.memory_id.clone())
                    .collect::<Vec<_>>(),
                expected,
            );
        }
    }

    #[test]
    fn repeated_excerpts_release_slots_before_the_independent_support_cap() {
        for prefix in ["file://incident.md#L", "cass-session://incident#L"] {
            let mut rows: Vec<_> = (0..80)
                .map(|index| {
                    let mut row = candidate(&format!("a-{index:05}"), COMMON, 0.7);
                    row.provenance_uri = Some(format!("{prefix}{}", index + 1));
                    row
                })
                .collect();
            let mut other = candidate("z-distinct", DISTINCT, 0.6);
            other.provenance_uri = Some(format!("{prefix}81"));
            rows.push(other);
            for limit in 1..=32 {
                let selected = select(&rows, limit);
                assert_eq!(selected.len(), limit);
                assert_eq!(selected[0].memory_id, "a-00000");
                assert_eq!(distinct_ids(&selected).len(), usize::from(limit > 1));
                let expected: Vec<_> = selected
                    .iter()
                    .map(|row| row.memory_id.clone())
                    .collect();
                rows.reverse();
                assert_eq!(
                    select(&rows, limit)
                        .iter()
                        .map(|row| row.memory_id.clone())
                        .collect::<Vec<_>>(),
                    expected,
                );
            }
        }
    }

    #[test]
    fn same_origin_does_not_make_distinct_scoring_inputs_disposable() {
        for variant in 0..3 {
            let mut rows = vec![
                candidate("a", COMMON, 0.7),
                candidate("b", COMMON, 0.7),
                candidate("z", DISTINCT, 0.6),
            ];
            for (index, row) in rows.iter_mut().enumerate() {
                row.provenance_uri = Some(format!("file://one.md#L{}", index + 1));
            }
            match variant {
                0 => rows[1].confidence = 0.69,
                1 => rows[1].trust_class = "cass_evidence".to_owned(),
                _ => rows[1].content.push_str(" Another relevant fact."),
            }
            let selected = select(&rows, 2);
            assert_eq!(selected[0].memory_id, "a");
            assert_eq!(selected[1].memory_id, "b");
        }
    }

    #[test]
    fn duplicate_slots_stay_unchanged_without_a_supported_replacement() {
        for confidence in [0.1, 0.54] {
            let mut rows: Vec<_> = (0..80)
                .map(|index| {
                    let mut row = candidate(&format!("a-{index:05}"), COMMON, 0.7);
                    row.provenance_uri = Some(format!("file://one.md#L{}", index + 1));
                    row
                })
                .collect();
            let mut other = candidate("z", DISTINCT, confidence);
            other.provenance_uri = Some("file://one.md#L81".to_owned());
            rows.push(other);
            let ids: Vec<_> = select(&rows, 32)
                .iter()
                .map(|row| row.memory_id.clone())
                .collect();
            assert_eq!(
                ids,
                (0..32)
                    .map(|index| format!("a-{index:05}"))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn unclusterable_spans_cannot_manufacture_a_support_bundle() {
        let mut rows = corpus();
        for row in rows.iter_mut().filter(|row| row.content == DISTINCT) {
            row.content = "+++".to_owned();
        }
        assert!(crate::core::ask::tokenize_for_ask("+++").is_empty());
        assert!(select(&rows, 32).iter().all(|row| row.content == COMMON));
    }

    #[test]
    fn public_answers_expose_corroborated_evidence_beyond_the_scan_cap() {
        let mut rows: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 8)
            .map(|index| candidate(&format!("a-{index:05}"), COMMON, 0.7))
            .collect();
        rows.push(candidate("z-first", DISTINCT, 0.54));
        rows.push(candidate("z-second", DISTINCT, 0.54));
        let request = AskRequest {
            question: "cache migration deployment".to_owned(),
            ..AskRequest::default()
        };
        let scorer = |_: &[String], _: &str, confidence: f32, _: &str| confidence;
        for degraded in [false, true] {
            let report = evaluate_ask_scored(&request, &rows, &scorer, degraded);
            assert!(!report.abstained && !report.conflict_detected);
            assert!(!report.extractiveness_violated);
            assert_eq!(report.candidates_scanned, rows.len());
            assert_eq!(report.citations.len(), 2);
            let distinct = &report.citations[1];
            assert_eq!(distinct.memory_id, "z-first");
            assert_eq!(distinct.text, DISTINCT);
            assert_eq!(distinct.confidence, 0.54);
            assert_eq!(distinct.provenance_uri.as_deref(), Some("manual://cli"));
            assert_eq!(
                DISTINCT.get(distinct.byte_start..distinct.byte_end),
                Some(distinct.text.as_str()),
            );
            assert!(report.answer_text.as_ref().unwrap().contains(DISTINCT));
            rows.reverse();
            assert_eq!(
                ask_data_json(&report),
                ask_data_json(&evaluate_ask_scored(&request, &rows, &scorer, degraded)),
            );
        }
    }

    #[test]
    fn public_answers_keep_distinct_same_origin_commands_without_a_copy_bonus() {
        const SOFT: &str =
            "Run git reset --soft HEAD in the workspace before starting the release.";
        const HARD: &str =
            "Run git reset --hard HEAD in the workspace before starting the release.";
        let mut rows: Vec<_> = (0..ASK_CANDIDATE_SCAN_CAP + 8)
            .map(|index| {
                let mut row = candidate(&format!("a-{index:05}"), SOFT, 1.0);
                row.provenance_uri = Some(format!("cass-session://incident#L{}", index + 1));
                row
            })
            .collect();
        let mut other = candidate("z-other", HARD, 1.0);
        other.provenance_uri = Some("cass-session://incident#L1000".to_owned());
        rows.push(other);
        let request = AskRequest {
            question: SOFT.to_owned(),
            ..AskRequest::default()
        };
        let scorer = |_: &[String], _: &str, _: f32, _: &str| 0.7;
        for report in [
            evaluate_ask(&request, &rows),
            evaluate_ask_scored(&request, &rows, &scorer, false),
            evaluate_ask_scored(&request, &rows, &scorer, true),
        ] {
            assert!(!report.abstained && !report.conflict_detected);
            assert!(!report.extractiveness_violated);
            assert_eq!(report.citations.len(), 2);
            assert_eq!(report.confidence_components.corroboration, 1.0);
            assert_eq!(report.citations[0].memory_id, "a-00000");
            assert_eq!(report.citations[1].memory_id, "z-other");
            for citation in &report.citations {
                let source = rows
                    .iter()
                    .find(|row| row.memory_id == citation.memory_id)
                    .unwrap();
                assert_eq!(
                    source.content.get(citation.byte_start..citation.byte_end),
                    Some(citation.text.as_str()),
                );
                assert_eq!(citation.provenance_uri, source.provenance_uri);
                assert_eq!(citation.confidence, source.confidence);
            }
        }
        let before = ask_data_json(&evaluate_ask(&request, &rows));
        rows.reverse();
        assert_eq!(before, ask_data_json(&evaluate_ask(&request, &rows)));
    }
}
