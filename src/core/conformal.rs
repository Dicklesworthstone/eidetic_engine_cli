//! Split-conformal helpers for explanation surfaces.
//!
//! The search scorer already emits calibrated score intervals. This module
//! holds the small, deterministic pieces needed by explanation surfaces that
//! need a prediction-set view over already-ranked memory candidates.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::Path,
};

use serde_json::Value;

pub const WHY_CONFORMAL_CONFIDENCE_INTERVALS_SCHEMA_V1: &str = "ee.why.conformal_prediction_set.v1";
pub const DEFAULT_CONFORMAL_COVERAGE: f32 = 0.95;
pub const MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES: usize = 20;

// Mirror the cap on the parallel reader in
// `src/core/search.rs::MAX_SEARCH_SCORE_CALIBRATION_BYTES` (commit 27f6ad4d).
// `.ee/search/calibration.jsonl` is workspace-local and grown by feedback
// events, so a peer agent or a runaway emitter can plant a large file
// between `ee why` invocations. The previous unbounded
// `BufReader::new(file).lines()` shape would pre-size each `String` to fit
// the line, so a multi-GB record (or multi-GB single-line file) would OOM
// `ee why <id>`'s conformal prediction-set surface. 64 MiB matches the
// parallel reader on the same file; a truncated tail line just fails the
// JSON parse via `serde_json::from_str(...).ok()?` and is silently
// dropped — the same observable shape an actually-corrupt row produces.
const CONFORMAL_CALIBRATION_MAX_BYTES: u64 = 64 * 1024 * 1024;

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
    // `total_cmp` gives a total order on f32 even if a NaN sneaks past
    // `clamp_unit_score`. `partial_cmp(...).unwrap_or(Equal)` would
    // collapse all NaN scores onto whatever the comparator hit first,
    // making the resulting `rank` field at line 118 sensitive to
    // upstream HashMap iteration order. This sort feeds the
    // deterministic conformal `prediction_set[]` field shape, so a non-
    // total ordering here is a determinism hazard, not just a ranking
    // ambiguity.
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
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
    // Use `total_cmp` instead of `partial_cmp(...).unwrap_or(Equal)` for a
    // total ordering. The `retain(is_finite)` above already filters NaN
    // so the two are observationally equivalent today, but defense-in-
    // depth matters because the quantile lookup at the next line trusts
    // a total order: a NaN sneaking past the filter (e.g. through a
    // future caller that bypasses `split_conformal_quantile` and reads
    // `scores` directly, or a refactor that moves the retain elsewhere)
    // would silently scramble the rank lookup and yield a non-
    // deterministic conformal threshold without breaking any test.
    scores.sort_by(|left, right| left.total_cmp(right));
    let coverage = clamp_unit_score(coverage);
    let rank = ((scores.len() as f32 + 1.0) * coverage).ceil() as usize;
    scores[rank.saturating_sub(1).min(scores.len() - 1)]
}

fn load_conformal_nonconformity_scores(workspace_path: &Path) -> Vec<f32> {
    let path = workspace_path
        .join(".ee")
        .join("search")
        .join("calibration.jsonl");
    let Some(file) = open_conformal_calibration_file_no_follow(&path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file.take(CONFORMAL_CALIBRATION_MAX_BYTES));
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

fn open_conformal_calibration_file_no_follow(path: &Path) -> Option<File> {
    if super::path_safety::path_has_symlink_component(path).ok()? {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_conformal_calibration_open_no_follow(&mut options);
    options.open(path).ok()
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_conformal_calibration_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_conformal_calibration_open_no_follow(_options: &mut fs::OpenOptions) {}

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
