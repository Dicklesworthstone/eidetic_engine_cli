use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::db::StoredMemory;

const CONSOLIDATION_SIEVE_DEFAULT_MAX_CANDIDATES: usize = 64;
pub(super) const CONSOLIDATION_SIEVE_ALGORITHM: &str = "sieve_streaming_greedy_v1";
const CONSOLIDATION_SIEVE_GROUP_BONUS: f64 = 1.0;
const CONSOLIDATION_SIEVE_LEVEL_KIND_BONUS: f64 = 0.25;

#[derive(Clone, Debug)]
pub(super) struct ConsolidationCandidatePlan {
    pub(super) candidate_id: String,
    pub(super) source_memory_id: String,
    pub(super) target_memory_id: String,
    pub(super) level: String,
    pub(super) kind: String,
    pub(super) normalized_content: String,
    pub(super) objective_score: f64,
    pub(super) proposed_content: String,
    pub(super) proposed_confidence: f32,
    pub(super) reason: String,
}

#[derive(Clone, Debug)]
pub(super) struct ConsolidationCandidateSelection {
    pub(super) candidates: Vec<ConsolidationCandidatePlan>,
    pub(super) considered_candidates: usize,
    pub(super) max_candidates: usize,
    pub(super) objective_value: f64,
}

fn normalize_memory_content_for_consolidation(content: &str) -> String {
    crate::curate::normalize_memory_content_for_consolidation(content)
}

pub(super) fn plan_consolidation_candidates(
    workspace_id: &str,
    memories: &[StoredMemory],
    item_limit: Option<u64>,
) -> ConsolidationCandidateSelection {
    let mut grouped = BTreeMap::<(String, String, String), Vec<&StoredMemory>>::new();
    for memory in memories {
        let normalized = normalize_memory_content_for_consolidation(&memory.content);
        if normalized.is_empty() {
            continue;
        }
        grouped
            .entry((memory.level.clone(), memory.kind.clone(), normalized))
            .or_default()
            .push(memory);
    }

    let mut candidates = Vec::new();
    for ((level, kind, normalized), mut group) in grouped {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|left, right| compare_consolidation_memory_preference(left, right));
        let Some(source) = group.first().copied() else {
            continue;
        };
        for target in group.iter().skip(1) {
            let candidate_id =
                stable_consolidation_candidate_id(workspace_id, &source.id, &target.id);
            let objective_score = consolidation_candidate_objective(source, target, group.len());
            candidates.push(ConsolidationCandidatePlan {
                candidate_id,
                source_memory_id: source.id.clone(),
                target_memory_id: target.id.clone(),
                level: level.clone(),
                kind: kind.clone(),
                normalized_content: normalized.clone(),
                objective_score,
                proposed_content: source.content.clone(),
                proposed_confidence: source.confidence.max(target.confidence),
                reason: format!(
                    "Duplicate {level}/{kind} memory content normalized to {:?}; consolidate {} into {} via {CONSOLIDATION_SIEVE_ALGORITHM}.",
                    normalized, target.id, source.id
                ),
            });
        }
    }
    candidates.sort_by(compare_consolidation_candidate_plan);
    sieve_stream_consolidation_candidates(
        candidates,
        consolidation_sieve_candidate_limit(item_limit),
    )
}

fn consolidation_sieve_candidate_limit(item_limit: Option<u64>) -> usize {
    item_limit
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(CONSOLIDATION_SIEVE_DEFAULT_MAX_CANDIDATES)
}

fn compare_consolidation_memory_preference(left: &StoredMemory, right: &StoredMemory) -> Ordering {
    right
        .confidence
        .total_cmp(&left.confidence)
        .then_with(|| right.utility.total_cmp(&left.utility))
        .then_with(|| right.importance.total_cmp(&left.importance))
        .then_with(|| left.id.cmp(&right.id))
}

fn consolidation_candidate_objective(
    source: &StoredMemory,
    target: &StoredMemory,
    duplicate_group_size: usize,
) -> f64 {
    let confidence_gain = f64::from((source.confidence - target.confidence).max(0.0));
    let utility_gain = f64::from((source.utility - target.utility).max(0.0));
    let importance_gain = f64::from((source.importance - target.importance).max(0.0));
    let group_pressure = (duplicate_group_size.saturating_sub(1) as f64).ln_1p();

    1.0 + group_pressure + confidence_gain + (utility_gain * 0.25) + (importance_gain * 0.25)
}

fn compare_consolidation_candidate_plan(
    left: &ConsolidationCandidatePlan,
    right: &ConsolidationCandidatePlan,
) -> Ordering {
    right
        .objective_score
        .total_cmp(&left.objective_score)
        .then_with(|| left.level.cmp(&right.level))
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.normalized_content.cmp(&right.normalized_content))
        .then_with(|| left.source_memory_id.cmp(&right.source_memory_id))
        .then_with(|| left.target_memory_id.cmp(&right.target_memory_id))
}

fn sieve_stream_consolidation_candidates(
    candidates: Vec<ConsolidationCandidatePlan>,
    max_candidates: usize,
) -> ConsolidationCandidateSelection {
    let considered_candidates = candidates.len();
    if max_candidates == 0 || candidates.is_empty() {
        return ConsolidationCandidateSelection {
            candidates: Vec::new(),
            considered_candidates,
            max_candidates,
            objective_value: 0.0,
        };
    }

    let mut selected = Vec::<ConsolidationCandidatePlan>::new();
    for candidate in candidates {
        if selected.len() < max_candidates {
            selected.push(candidate);
            continue;
        }

        let current_objective = consolidation_selection_objective(&selected);
        let mut best_replacement = None;
        let mut best_objective = current_objective;
        for index in 0..selected.len() {
            let mut replacement = selected.clone();
            replacement[index] = candidate.clone();
            let replacement_objective = consolidation_selection_objective(&replacement);
            if replacement_objective > best_objective {
                best_objective = replacement_objective;
                best_replacement = Some(index);
            }
        }

        if let Some(index) = best_replacement {
            selected[index] = candidate;
        }
    }

    selected.sort_by(compare_consolidation_candidate_plan);
    let objective_value = consolidation_selection_objective(&selected);
    ConsolidationCandidateSelection {
        candidates: selected,
        considered_candidates,
        max_candidates,
        objective_value,
    }
}

fn consolidation_selection_objective(candidates: &[ConsolidationCandidatePlan]) -> f64 {
    let base_score = candidates
        .iter()
        .map(|candidate| candidate.objective_score)
        .sum::<f64>();
    let distinct_groups = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.level.as_str(),
                candidate.kind.as_str(),
                candidate.normalized_content.as_str(),
            )
        })
        .collect::<BTreeSet<_>>()
        .len() as f64;
    let distinct_level_kinds = candidates
        .iter()
        .map(|candidate| (candidate.level.as_str(), candidate.kind.as_str()))
        .collect::<BTreeSet<_>>()
        .len() as f64;

    base_score
        + (distinct_groups * CONSOLIDATION_SIEVE_GROUP_BONUS)
        + (distinct_level_kinds * CONSOLIDATION_SIEVE_LEVEL_KIND_BONUS)
}

fn stable_consolidation_candidate_id(
    workspace_id: &str,
    source_memory_id: &str,
    target_memory_id: &str,
) -> String {
    crate::curate::steward_consolidation_candidate_id(
        workspace_id,
        source_memory_id,
        target_memory_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn candidate(
        id: &str,
        group: &str,
        level: &str,
        kind: &str,
        score: f64,
    ) -> ConsolidationCandidatePlan {
        ConsolidationCandidatePlan {
            candidate_id: format!("curate_{id}"),
            source_memory_id: format!("mem_source_{id}"),
            target_memory_id: format!("mem_target_{id}"),
            level: level.to_owned(),
            kind: kind.to_owned(),
            normalized_content: group.to_owned(),
            objective_score: score,
            proposed_content: format!("content {group}"),
            proposed_confidence: 0.9,
            reason: "test fixture".to_owned(),
        }
    }

    fn exhaustive_best_objective(
        candidates: &[ConsolidationCandidatePlan],
        max_candidates: usize,
    ) -> f64 {
        fn visit(
            candidates: &[ConsolidationCandidatePlan],
            max_candidates: usize,
            index: usize,
            selected: &mut Vec<ConsolidationCandidatePlan>,
            best: &mut f64,
        ) {
            if selected.len() == max_candidates || index == candidates.len() {
                *best = best.max(consolidation_selection_objective(selected));
                return;
            }

            selected.push(candidates[index].clone());
            visit(candidates, max_candidates, index + 1, selected, best);
            selected.pop();

            if candidates.len().saturating_sub(index + 1) + selected.len() >= max_candidates {
                visit(candidates, max_candidates, index + 1, selected, best);
            }
        }

        let mut best = 0.0;
        let mut selected = Vec::with_capacity(max_candidates);
        visit(candidates, max_candidates, 0, &mut selected, &mut best);
        best
    }

    #[test]
    fn sieve_quality_stays_within_five_percent_of_exhaustive_fixture() -> TestResult {
        let candidates = vec![
            candidate("0001", "alpha", "procedural", "rule", 8.0),
            candidate("0002", "alpha", "procedural", "rule", 7.8),
            candidate("0003", "beta", "procedural", "rule", 7.2),
            candidate("0004", "gamma", "evidence", "fact", 6.1),
            candidate("0005", "delta", "decision", "note", 5.8),
            candidate("0006", "epsilon", "episodic", "failure", 5.7),
            candidate("0007", "zeta", "evidence", "fact", 4.9),
        ];

        let selection = sieve_stream_consolidation_candidates(candidates.clone(), 3);
        let optimal = exhaustive_best_objective(&candidates, 3);
        if selection.objective_value + f64::EPSILON < optimal * 0.95 {
            return Err(format!(
                "selector objective {:.3} below 95% of exhaustive optimum {:.3}",
                selection.objective_value, optimal
            ));
        }

        Ok(())
    }

    #[test]
    fn sieve_zero_limit_considers_candidates_but_selects_none() -> TestResult {
        let candidates = vec![
            candidate("0001", "alpha", "procedural", "rule", 8.0),
            candidate("0002", "beta", "semantic", "fact", 7.0),
        ];

        let selection = sieve_stream_consolidation_candidates(candidates, 0);
        if !selection.candidates.is_empty() {
            return Err(format!(
                "zero limit must select no candidates, got {:?}",
                selection.candidates
            ));
        }
        if selection.considered_candidates != 2 {
            return Err(format!(
                "zero-limit run must still report two considered candidates, got {}",
                selection.considered_candidates
            ));
        }
        if selection.objective_value != 0.0 {
            return Err(format!(
                "zero-limit objective must be 0.0, got {}",
                selection.objective_value
            ));
        }
        Ok(())
    }

    #[test]
    fn normalization_and_limit_defaults_are_stable() -> TestResult {
        let normalized = normalize_memory_content_for_consolidation("  Cargo   fmt\nCHECK  ");
        if normalized != "cargo fmt check" {
            return Err(format!("unexpected normalized content: {normalized:?}"));
        }

        let default_limit = consolidation_sieve_candidate_limit(None);
        if default_limit != CONSOLIDATION_SIEVE_DEFAULT_MAX_CANDIDATES {
            return Err(format!(
                "default limit changed: expected {}, got {default_limit}",
                CONSOLIDATION_SIEVE_DEFAULT_MAX_CANDIDATES
            ));
        }

        let explicit_limit = consolidation_sieve_candidate_limit(Some(3));
        if explicit_limit != 3 {
            return Err(format!(
                "explicit limit should stay 3, got {explicit_limit}"
            ));
        }
        Ok(())
    }
}
