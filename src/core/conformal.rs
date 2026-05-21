//! Split-conformal helpers for explanation surfaces.
//!
//! The search scorer already emits calibrated score intervals. This module
//! holds the small, deterministic pieces needed by explanation surfaces that
//! need a prediction-set view over already-ranked memory candidates.

use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use serde_json::Value;

pub const WHY_CONFORMAL_CONFIDENCE_INTERVALS_SCHEMA_V1: &str = "ee.why.conformal_prediction_set.v1";
pub const DEFAULT_CONFORMAL_COVERAGE: f32 = 0.95;
pub const MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct WhyConformalCandidate {
    pub memory_id: String,
    pub score: f32,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WhyConformalPredictionSetEntry {
    pub memory_id: String,
    pub rank: u32,
    pub source: String,
    pub score: f32,
    pub nonconformity_score: f32,
    pub included: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WhyConformalConfidenceIntervals {
    pub schema: &'static str,
    pub method: &'static str,
    pub coverage_guarantee: f32,
    pub alpha: f32,
    pub target_memory_id: String,
    pub score_interval: [f32; 2],
    pub nonconformity_quantile: f32,
    pub calibration_sample_count: usize,
    pub calibration_status: &'static str,
    pub prediction_set: Vec<WhyConformalPredictionSetEntry>,
}

pub fn why_conformal_confidence_intervals(
    workspace_path: Option<&Path>,
    target_memory_id: &str,
    target_score: f32,
    candidates: impl IntoIterator<Item = WhyConformalCandidate>,
) -> WhyConformalConfidenceIntervals {
    let residuals = workspace_path
        .map(load_conformal_nonconformity_scores)
        .unwrap_or_default();
    let (quantile, status) = if residuals.len() >= MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES {
        (
            split_conformal_quantile(residuals.clone(), DEFAULT_CONFORMAL_COVERAGE),
            "calibrated",
        )
    } else {
        (1.0, "conservative_insufficient_calibration")
    };

    let mut by_memory_id = BTreeMap::<String, WhyConformalCandidate>::new();
    for candidate in candidates {
        let memory_id = candidate.memory_id.trim();
        if memory_id.is_empty() {
            continue;
        }
        let candidate = WhyConformalCandidate {
            memory_id: memory_id.to_owned(),
            score: clamp_unit_score(candidate.score),
            source: candidate.source,
        };
        by_memory_id
            .entry(candidate.memory_id.clone())
            .and_modify(|current| {
                if candidate.score > current.score
                    || (candidate.score == current.score
                        && candidate.source.as_str() < current.source.as_str())
                {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    by_memory_id
        .entry(target_memory_id.to_owned())
        .or_insert_with(|| WhyConformalCandidate {
            memory_id: target_memory_id.to_owned(),
            score: clamp_unit_score(target_score),
            source: "target".to_owned(),
        });

    let mut ranked = by_memory_id.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
            .then_with(|| left.source.cmp(&right.source))
    });

    let prediction_set = ranked
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let nonconformity_score = 1.0 - candidate.score;
            WhyConformalPredictionSetEntry {
                memory_id: candidate.memory_id,
                rank: u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX),
                source: candidate.source,
                score: candidate.score,
                nonconformity_score,
                included: nonconformity_score <= quantile,
            }
        })
        .collect::<Vec<_>>();

    WhyConformalConfidenceIntervals {
        schema: WHY_CONFORMAL_CONFIDENCE_INTERVALS_SCHEMA_V1,
        method: "split_conformal_nonconformity",
        coverage_guarantee: DEFAULT_CONFORMAL_COVERAGE,
        alpha: 1.0 - DEFAULT_CONFORMAL_COVERAGE,
        target_memory_id: target_memory_id.to_owned(),
        score_interval: conformal_score_interval(target_score, quantile),
        nonconformity_quantile: quantile,
        calibration_sample_count: residuals.len(),
        calibration_status: status,
        prediction_set,
    }
}

pub fn conformal_score_interval(score: f32, quantile: f32) -> [f32; 2] {
    let score = clamp_unit_score(score);
    let quantile = clamp_unit_score(quantile);
    [(score - quantile).max(0.0), (score + quantile).min(1.0)]
}

pub fn split_conformal_quantile(mut scores: Vec<f32>, coverage: f32) -> f32 {
    scores.retain(|score| score.is_finite());
    if scores.is_empty() {
        return 1.0;
    }
    scores.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    let coverage = clamp_unit_score(coverage);
    let rank = ((scores.len() as f32 + 1.0) * coverage).ceil() as usize;
    scores[rank.saturating_sub(1).min(scores.len() - 1)]
}

fn load_conformal_nonconformity_scores(workspace_path: &Path) -> Vec<f32> {
    let path = workspace_path
        .join(".ee")
        .join("search")
        .join("calibration.jsonl");
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let value = serde_json::from_str::<Value>(line).ok()?;
            conformal_nonconformity_from_value(&value)
        })
        .collect()
}

fn conformal_nonconformity_from_value(value: &Value) -> Option<f32> {
    number_at(value, &["nonconformityScore", "nonconformity_score"])
        .or_else(|| {
            let score = number_at(value, &["score", "fusionScore", "fusion_score"])?;
            Some(1.0 - score)
        })
        .map(clamp_unit_score)
}

fn number_at(value: &Value, keys: &[&str]) -> Option<f32> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite())
            .map(|number| number as f32)
    })
}

fn clamp_unit_score(score: f32) -> f32 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
