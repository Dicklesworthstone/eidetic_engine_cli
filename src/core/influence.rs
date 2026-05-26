//! Deterministic influence attribution helpers for `ee why`.
//!
//! This module keeps the counterfactual math independent from storage so it can
//! be property-tested without a workspace database.

use std::{cmp::Ordering, collections::BTreeMap};

pub const WHY_COUNTERFACTUAL_INFLUENCE_SCHEMA_V1: &str = "ee.why.influence.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhyInfluenceDirection {
    Positive,
    Negative,
}

impl WhyInfluenceDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
        }
    }

    const fn sign(self) -> f32 {
        match self {
            Self::Positive => 1.0,
            Self::Negative => -1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WhyInfluenceCandidate {
    pub memory_id: String,
    pub relation: String,
    pub score: f32,
    pub direction: WhyInfluenceDirection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WhyInfluenceEntry {
    pub memory_id: String,
    pub rank: u32,
    pub relation: String,
    pub baseline_score: f32,
    pub leave_one_out_score: f32,
    pub influence_delta: f32,
    pub absolute_influence: f32,
    pub direction: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WhyCounterfactualInfluence {
    pub schema: &'static str,
    pub method: &'static str,
    pub target_memory_id: String,
    pub baseline_top_score: f32,
    pub approximation_error_ratio: f32,
    pub total_absolute_influence: f32,
    pub top_positive: Vec<WhyInfluenceEntry>,
    pub top_negative: Vec<WhyInfluenceEntry>,
    pub entries: Vec<WhyInfluenceEntry>,
}

#[derive(Clone, Debug)]
struct InfluenceAccumulator {
    relation: String,
    signed_delta: f32,
    strongest_abs_delta: f32,
}

#[must_use]
pub fn why_counterfactual_influence(
    target_memory_id: &str,
    baseline_top_score: f32,
    candidates: impl IntoIterator<Item = WhyInfluenceCandidate>,
) -> WhyCounterfactualInfluence {
    let baseline_top_score = clamp_unit_score(baseline_top_score);
    let mut by_memory_id = BTreeMap::<String, InfluenceAccumulator>::new();

    for candidate in candidates {
        let memory_id = candidate.memory_id.trim();
        if memory_id.is_empty() || memory_id == target_memory_id {
            continue;
        }
        let magnitude = clamp_unit_score(candidate.score);
        if magnitude == 0.0 {
            continue;
        }
        let signed_delta = magnitude * candidate.direction.sign();
        let relation = relation_label(candidate.relation.trim(), candidate.direction);
        by_memory_id
            .entry(memory_id.to_owned())
            .and_modify(|accumulator| {
                accumulator.signed_delta += signed_delta;
                if magnitude > accumulator.strongest_abs_delta
                    || (magnitude == accumulator.strongest_abs_delta
                        && relation.as_str() < accumulator.relation.as_str())
                {
                    accumulator.relation = relation.clone();
                    accumulator.strongest_abs_delta = magnitude;
                }
            })
            .or_insert(InfluenceAccumulator {
                relation,
                signed_delta,
                strongest_abs_delta: magnitude,
            });
    }

    let mut entries = by_memory_id
        .into_iter()
        .map(|(memory_id, accumulator)| {
            let leave_one_out_score =
                exact_leave_one_out_score(baseline_top_score, accumulator.signed_delta);
            let influence_delta = signed_score_delta(baseline_top_score, leave_one_out_score);
            let absolute_influence = influence_delta.abs();
            let direction = if influence_delta >= 0.0 {
                "positive"
            } else {
                "negative"
            };
            WhyInfluenceEntry {
                memory_id,
                rank: 0,
                relation: accumulator.relation,
                baseline_score: baseline_top_score,
                leave_one_out_score,
                influence_delta,
                absolute_influence,
                direction,
            }
        })
        .filter(|entry| entry.absolute_influence > 0.0)
        .collect::<Vec<_>>();

    entries.sort_by(compare_entries_by_absolute_influence);
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.rank = u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX);
    }

    let total_absolute_influence = sum_absolute_influence(&entries);
    let top_positive = top_positive_influencers(&entries);
    let top_negative = top_negative_influencers(&entries);

    WhyCounterfactualInfluence {
        schema: WHY_COUNTERFACTUAL_INFLUENCE_SCHEMA_V1,
        method: "deterministic_link_leave_one_out",
        target_memory_id: target_memory_id.to_owned(),
        baseline_top_score,
        approximation_error_ratio: 0.0,
        total_absolute_influence,
        top_positive,
        top_negative,
        entries,
    }
}

#[must_use]
pub fn exact_leave_one_out_score(baseline_top_score: f32, signed_influence_delta: f32) -> f32 {
    clamp_unit_score(baseline_top_score - signed_influence_delta)
}

#[must_use]
pub fn exact_leave_one_out_delta(entry: &WhyInfluenceEntry) -> f32 {
    signed_score_delta(entry.baseline_score, entry.leave_one_out_score)
}

#[must_use]
pub fn sum_absolute_influence(entries: &[WhyInfluenceEntry]) -> f32 {
    entries.iter().map(|entry| entry.absolute_influence).sum()
}

#[must_use]
pub fn relative_total_influence_error(report: &WhyCounterfactualInfluence) -> f32 {
    let exact_total = report
        .entries
        .iter()
        .map(|entry| exact_leave_one_out_delta(entry).abs())
        .sum::<f32>();
    if exact_total == 0.0 {
        return report.total_absolute_influence.abs();
    }
    ((report.total_absolute_influence - exact_total) / exact_total).abs()
}

fn top_positive_influencers(entries: &[WhyInfluenceEntry]) -> Vec<WhyInfluenceEntry> {
    let mut positive = entries
        .iter()
        .filter(|entry| entry.influence_delta > 0.0)
        .cloned()
        .collect::<Vec<_>>();
    // `total_cmp` gives a total order on f32 even if a NaN ever escapes
    // upstream filtering. `partial_cmp(...).unwrap_or(Equal)` would
    // collapse all NaN entries onto whatever the comparator hit first,
    // making the emitted `top_positive[]` (a `ee.why.influence.v1`
    // schema field) sensitive to candidate iteration order. The
    // `filter(influence_delta > 0.0)` above already excludes NaN
    // (`NaN > 0.0 == false`), so the two are observationally equivalent
    // today — but the `ee.why.influence.v1` envelope is a determinism-
    // contract surface (same target_memory_id + same candidates → byte-
    // identical JSON), and a defense-in-depth ordering keeps that
    // contract from silently breaking when a future caller path lets a
    // non-finite `influence_delta` through. Mirrors the equivalent
    // hardening of `why_conformal_confidence_intervals` and
    // `split_conformal_quantile` in `src/core/conformal.rs`.
    positive.sort_by(|left, right| {
        right
            .influence_delta
            .total_cmp(&left.influence_delta)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    positive.truncate(3);
    positive
}

fn top_negative_influencers(entries: &[WhyInfluenceEntry]) -> Vec<WhyInfluenceEntry> {
    let mut negative = entries
        .iter()
        .filter(|entry| entry.influence_delta < 0.0)
        .cloned()
        .collect::<Vec<_>>();
    // See `top_positive_influencers` for the `total_cmp` rationale.
    negative.sort_by(|left, right| {
        left.influence_delta
            .total_cmp(&right.influence_delta)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    negative.truncate(3);
    negative
}

fn compare_entries_by_absolute_influence(
    left: &WhyInfluenceEntry,
    right: &WhyInfluenceEntry,
) -> Ordering {
    // `total_cmp` keeps the sort total even when `absolute_influence`
    // somehow holds a NaN (the `filter(absolute_influence > 0.0)` at
    // line 135 excludes them today, but the `ee.why.influence.v1`
    // envelope must produce byte-identical JSON for the same
    // `target_memory_id` + candidate set, and a non-total ordering here
    // would be a silent determinism break under any future refactor.
    // Mirrors `src/core/conformal.rs::why_conformal_confidence_intervals`.
    right
        .absolute_influence
        .total_cmp(&left.absolute_influence)
        .then_with(|| left.memory_id.cmp(&right.memory_id))
        .then_with(|| left.relation.cmp(&right.relation))
}

fn relation_label(relation: &str, direction: WhyInfluenceDirection) -> String {
    if relation.is_empty() {
        direction.as_str().to_owned()
    } else {
        relation.to_owned()
    }
}

fn signed_score_delta(baseline_top_score: f32, leave_one_out_score: f32) -> f32 {
    baseline_top_score - leave_one_out_score
}

fn clamp_unit_score(score: f32) -> f32 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
