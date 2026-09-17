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
    // Missing or invalid uncertainty is not zero uncertainty. Keep the full
    // score domain instead of turning a bad threshold into a point estimate.
    if !quantile.is_finite() || !(0.0..=1.0).contains(&quantile) {
        return [0.0, 1.0];
    }
    let score = clamp_unit_score(score);
    [(score - quantile).max(0.0), (score + quantile).min(1.0)]
}

/// Return the split-conformal threshold for nonconformity scores in `[0, 1]`.
/// Invalid observations do not count toward the calibration sample size.
/// When the requested rank exceeds the observed sample, the threshold is the
/// upper support bound, not the largest observed residual.
pub fn split_conformal_quantile(mut scores: Vec<f32>, coverage: f32) -> f32 {
    if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
        return 1.0;
    }
    scores.retain(|score| score.is_finite() && (0.0..=1.0).contains(score));
    if scores.is_empty() {
        return 1.0;
    }
    scores.sort_by(|left, right| left.total_cmp(right));
    let rank = ((scores.len() as f64 + 1.0) * f64::from(coverage)).ceil() as usize;
    if rank > scores.len() {
        return 1.0;
    }
    scores[rank.saturating_sub(1)]
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
    let residual_keys = ["nonconformityScore", "nonconformity_score"];
    if residual_keys.iter().any(|key| value.get(*key).is_some()) {
        // An explicitly invalid residual must not be replaced by another
        // field and counted as a valid calibration observation.
        return number_at(value, &residual_keys);
    }
    number_at(value, &["score", "fusionScore", "fusion_score"]).map(|score| 1.0 - score)
}

fn number_at(value: &Value, keys: &[&str]) -> Option<f32> {
    let number = keys.iter().find_map(|key| value.get(*key))?.as_f64()?;
    // Validate before narrowing to f32: a finite JSON f64 can overflow f32.
    // Clamping raw BM25 scores or invalid residuals would fabricate perfect
    // calibration samples and unjustifiably narrow the prediction set.
    if !number.is_finite() || !(0.0..=1.0).contains(&number) {
        return None;
    }
    Some(number as f32)
}

fn clamp_unit_score(score: f32) -> f32 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_CONFORMAL_COVERAGE, conformal_nonconformity_from_value, conformal_score_interval,
        split_conformal_quantile,
    };
    use serde_json::{Value, json};

    #[test]
    fn calibration_accepts_unit_scores_and_residual_aliases() {
        for key in ["nonconformityScore", "nonconformity_score"] {
            for number in [0.0_f32, 0.25, 1.0] {
                let mut row = json!({});
                row[key] = json!(number);
                assert_eq!(conformal_nonconformity_from_value(&row), Some(number));
            }
        }
        for key in ["score", "fusionScore", "fusion_score"] {
            for number in [0.0_f32, 0.25, 1.0] {
                let mut row = json!({});
                row[key] = json!(number);
                assert_eq!(conformal_nonconformity_from_value(&row), Some(1.0 - number));
            }
        }
    }

    #[test]
    fn invalid_calibration_numbers_are_not_clamped_into_samples() {
        for key in [
            "nonconformityScore",
            "nonconformity_score",
            "score",
            "fusionScore",
            "fusion_score",
        ] {
            for number in [-1e300_f64, -0.01, 1.01, 1e300] {
                let mut row = json!({});
                row[key] = json!(number);
                assert_eq!(
                    conformal_nonconformity_from_value(&row),
                    None,
                    "invalid calibration observation: {row}"
                );
            }
        }
    }

    #[test]
    fn invalid_explicit_residual_does_not_fall_back_to_score() {
        for invalid in [Value::Null, json!("0.5"), json!(false), json!(-0.5)] {
            for key in ["nonconformityScore", "nonconformity_score"] {
                let mut row = json!({"score": 0.75});
                row[key] = invalid.clone();
                assert_eq!(conformal_nonconformity_from_value(&row), None);
            }
        }
    }

    #[test]
    fn invalid_primary_number_does_not_fall_back_to_alias() {
        assert_eq!(
            conformal_nonconformity_from_value(&json!({
                "score": 12.0,
                "fusionScore": 0.75
            })),
            None
        );
        assert_eq!(
            conformal_nonconformity_from_value(&json!({
                "nonconformityScore": -1.0,
                "nonconformity_score": 0.25
            })),
            None
        );
    }

    #[test]
    fn small_sample_uses_upper_support_bound_for_unobserved_rank() {
        assert_eq!(
            split_conformal_quantile(vec![0.125, 0.25, 0.375], DEFAULT_CONFORMAL_COVERAGE),
            1.0
        );
        assert_eq!(split_conformal_quantile(vec![0.125; 100], 1.0), 1.0);
    }

    #[test]
    fn quantile_uses_valid_sample_count_and_order_statistic() {
        let scores = vec![f32::NAN, f32::INFINITY, -2.0, -1.0, 0.25, 0.75, 2.0];
        assert_eq!(split_conformal_quantile(scores, 0.5), 0.75);
        assert_eq!(
            split_conformal_quantile(vec![0.75, 0.25, 0.5, 0.0], 0.5),
            0.5
        );
    }

    #[test]
    fn empty_or_invalid_calibration_stays_conservative() {
        assert_eq!(split_conformal_quantile(Vec::new(), 0.95), 1.0);
        assert_eq!(
            split_conformal_quantile(vec![-1.0, 2.0, f32::NAN, f32::INFINITY], 0.95),
            1.0
        );
    }

    #[test]
    fn invalid_coverage_stays_conservative() {
        for coverage in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
            assert_eq!(split_conformal_quantile(vec![0.125; 100], coverage), 1.0);
        }
    }

    #[test]
    fn invalid_uncertainty_never_becomes_a_point_interval() {
        for quantile in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
            assert_eq!(conformal_score_interval(0.75, quantile), [0.0, 1.0]);
        }
        assert_eq!(conformal_score_interval(0.75, 0.125), [0.625, 0.875]);
    }
}
