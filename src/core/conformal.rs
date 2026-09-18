//! Split-conformal helpers for explanation surfaces.
//!
//! The search scorer already emits calibrated score intervals. This module
//! holds the small, deterministic pieces needed by explanation surfaces that
//! need a prediction-set view over already-ranked memory candidates.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read},
    path::Path,
};

use serde_json::Value;

pub const WHY_CONFORMAL_CONFIDENCE_INTERVALS_SCHEMA_V1: &str = "ee.why.conformal_prediction_set.v1";
pub const DEFAULT_CONFORMAL_COVERAGE: f32 = 0.95;
pub const MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES: usize = 20;

// Match the search calibration reader's 64 MiB budget. Read one extra byte
// to distinguish real EOF from a budget-truncated prefix, even when the cap
// lands exactly on a JSONL record boundary. Incomplete or unreadable input
// must use the conservative fallback rather than certify a partial dataset.
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
    /// Nominal coverage when calibration is usable; zero means no calibrated
    /// guarantee. Keep the v1 numeric field rather than fabricate 95% coverage
    /// for a fallback or change the existing Rust/JSON field type.
    pub coverage_guarantee: f32,
    /// Requested error level, not evidence that calibration succeeded.
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
    let calibration_sample_count = residuals.len();
    let (mut quantile, mut status) =
        if calibration_sample_count >= MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES {
            (
                split_conformal_quantile(residuals, DEFAULT_CONFORMAL_COVERAGE),
                "calibrated",
            )
        } else {
            (1.0, "conservative_insufficient_calibration")
        };

    let target_memory_id = target_memory_id.trim();
    let mut invalid_scores = !is_unit_score(target_score);
    let mut by_memory_id = BTreeMap::<String, WhyConformalCandidate>::new();
    for candidate in candidates {
        let memory_id = candidate.memory_id.trim();
        // The explicit target score owns both the interval and its entry in
        // the prediction set. A related/pack-mate duplicate must not override
        // only the latter and produce contradictory evidence for one identity.
        if memory_id.is_empty() || memory_id == target_memory_id {
            continue;
        }
        invalid_scores |= !is_unit_score(candidate.score);
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
    by_memory_id.insert(
        target_memory_id.to_owned(),
        WhyConformalCandidate {
            memory_id: target_memory_id.to_owned(),
            score: clamp_unit_score(target_score),
            source: "target".to_owned(),
        },
    );

    // Out-of-domain scores are not calibrated probabilities. Clamping them
    // is suitable for a finite display value, but cannot justify excluding
    // candidates or certifying an interval. Preserve the entire set instead.
    if invalid_scores {
        quantile = 1.0;
        status = "conservative_invalid_scores";
    }

    let mut ranked = by_memory_id.into_values().collect::<Vec<_>>();
    // Use a total order, with identity/source tie-breaks, independently of
    // upstream iteration order. clamp_unit_score also canonicalizes -0.0.
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
        coverage_guarantee: if status == "calibrated" {
            DEFAULT_CONFORMAL_COVERAGE
        } else {
            0.0
        },
        alpha: 1.0 - DEFAULT_CONFORMAL_COVERAGE,
        target_memory_id: target_memory_id.to_owned(),
        score_interval: conformal_score_interval(target_score, quantile),
        nonconformity_quantile: quantile,
        calibration_sample_count,
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
    read_conformal_calibration(file, CONFORMAL_CALIBRATION_MAX_BYTES).unwrap_or_default()
}

fn read_conformal_calibration(reader: impl Read, max_bytes: u64) -> io::Result<Vec<f32>> {
    let mut reader = BufReader::new(reader.take(max_bytes.saturating_add(1)));
    let mut scores = Vec::new();
    let mut line = String::new();
    let mut bytes_read = 0_u64;
    loop {
        line.clear();
        let count = reader.read_line(&mut line)?;
        if count == 0 {
            return Ok(scores);
        }
        bytes_read = bytes_read.saturating_add(count as u64);
        if bytes_read > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "conformal calibration exceeds its byte budget",
            ));
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A corrupt or partially appended record invalidates this read.
        // Keeping only its valid prefix can bias the calibration threshold.
        let value = serde_json::from_str::<Value>(line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if let Some(score) = conformal_nonconformity_from_value(&value) {
            scores.push(score);
        }
    }
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
    let file = options.open(path).ok()?;
    let opened_metadata = file.metadata().ok()?;
    if !opened_metadata.is_file() || opened_metadata.len() > CONFORMAL_CALIBRATION_MAX_BYTES {
        return None;
    }
    Some(file)
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

fn is_unit_score(score: f32) -> bool {
    score.is_finite() && (0.0..=1.0).contains(&score)
}

fn clamp_unit_score(score: f32) -> f32 {
    if !score.is_finite() || score <= 0.0 {
        0.0
    } else {
        score.min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_CONFORMAL_COVERAGE, MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES, WhyConformalCandidate,
        conformal_nonconformity_from_value, conformal_score_interval, read_conformal_calibration,
        split_conformal_quantile, why_conformal_confidence_intervals,
    };
    use serde_json::{Value, json};
    use std::io::{self, Cursor, Read};

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

    #[test]
    fn calibration_reader_accepts_complete_data_without_final_newline() {
        let input = concat!(
            "\n{\"metadata\": true}\n",
            "{\"nonconformityScore\": 0.25}\r\n",
            "{\"score\": 0.25}"
        );
        assert_eq!(
            read_conformal_calibration(input.as_bytes(), input.len() as u64)
                .expect("complete calibration"),
            vec![0.25, 0.75]
        );
    }

    #[test]
    fn calibration_reader_rejects_over_budget_complete_record_prefix() {
        let prefix =
            "{\"nonconformityScore\":0.125}\n".repeat(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES);
        let input = format!("{prefix}{{\"nonconformityScore\":1.0}}\n");
        let error = read_conformal_calibration(input.as_bytes(), prefix.len() as u64)
            .expect_err("a complete-looking prefix is not the complete dataset");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn calibration_reader_requires_true_eof_at_exact_byte_budget() {
        let input = b"{\"nonconformityScore\":0.25}\n";
        assert_eq!(
            read_conformal_calibration(input.as_slice(), input.len() as u64)
                .expect("exact-budget complete input"),
            vec![0.25]
        );
        assert!(read_conformal_calibration(input.as_slice(), input.len() as u64 - 1).is_err());
    }

    #[test]
    fn calibration_reader_rejects_malformed_tail_after_enough_valid_samples() {
        let prefix =
            "{\"nonconformityScore\":0.125}\n".repeat(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES);
        let input = format!("{prefix}{{\"nonconformityScore\":");
        assert!(read_conformal_calibration(input.as_bytes(), input.len() as u64).is_err());
    }

    #[test]
    fn calibration_reader_rejects_invalid_utf8_after_valid_records() {
        let mut input = b"{\"nonconformityScore\":0.125}\n".to_vec();
        input.push(0xff);
        assert!(read_conformal_calibration(input.as_slice(), input.len() as u64).is_err());
    }

    #[test]
    fn calibration_reader_does_not_count_out_of_domain_rows() {
        let input = concat!(
            "{\"nonconformityScore\":0.25}\n",
            "{\"score\":1e300}\n",
            "{\"nonconformityScore\":-0.5}\n",
            "{\"score\":0.25}\n"
        );
        assert_eq!(
            read_conformal_calibration(input.as_bytes(), input.len() as u64)
                .expect("syntactically complete input"),
            vec![0.25, 0.75]
        );
    }

    #[test]
    fn calibration_reader_handles_empty_zero_budget_input() {
        assert!(
            read_conformal_calibration(b"".as_slice(), 0)
                .expect("empty input")
                .is_empty()
        );
        assert!(read_conformal_calibration(b"\n".as_slice(), 0).is_err());
    }

    #[test]
    fn calibration_reader_propagates_read_failure_after_valid_prefix() {
        struct FailsAfterPrefix(Cursor<Vec<u8>>);

        impl Read for FailsAfterPrefix {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = self.0.read(buffer)?;
                if count == 0 && !buffer.is_empty() {
                    return Err(io::Error::other("injected calibration read failure"));
                }
                Ok(count)
            }
        }

        let prefix =
            "{\"nonconformityScore\":0.125}\n".repeat(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES);
        let budget = prefix.len() as u64 + 1;
        let reader = FailsAfterPrefix(Cursor::new(prefix.into_bytes()));
        let error = read_conformal_calibration(reader, budget)
            .expect_err("read errors must not return a calibrated-looking prefix");
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    fn calibrated_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().expect("temporary workspace");
        let workspace = temp.path().canonicalize().expect("physical temporary root");
        std::fs::create_dir_all(workspace.join(".ee/search")).expect("calibration directory");
        std::fs::write(
            workspace.join(".ee/search/calibration.jsonl"),
            "{\"nonconformityScore\":0.25}\n".repeat(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES),
        )
        .expect("calibration fixture");
        (temp, workspace)
    }

    fn candidate(memory_id: &str, score: f32) -> WhyConformalCandidate {
        WhyConformalCandidate {
            memory_id: memory_id.to_owned(),
            score,
            source: "fixture".to_owned(),
        }
    }

    #[test]
    fn uncalibrated_prediction_set_does_not_claim_nominal_coverage() {
        let report =
            why_conformal_confidence_intervals(None, "target", 0.75, [candidate("other", 0.125)]);
        assert_eq!(report.coverage_guarantee, 0.0);
        assert_eq!(
            report.calibration_status,
            "conservative_insufficient_calibration"
        );
        assert_eq!(report.score_interval, [0.0, 1.0]);
        assert!(report.prediction_set.iter().all(|entry| entry.included));
    }

    #[test]
    fn explicit_target_owns_interval_and_prediction_set_despite_duplicate() {
        let (_temp, workspace) = calibrated_workspace();
        let candidates = [candidate(" target ", 1.0), candidate("other", 0.875)];
        let forward = why_conformal_confidence_intervals(
            Some(&workspace),
            " target ",
            0.25,
            candidates.clone(),
        );
        let reverse = why_conformal_confidence_intervals(
            Some(&workspace),
            " target ",
            0.25,
            candidates.into_iter().rev(),
        );
        assert_eq!(forward, reverse);
        assert_eq!(forward.coverage_guarantee, DEFAULT_CONFORMAL_COVERAGE);
        assert_eq!(forward.target_memory_id, "target");
        assert_eq!(forward.score_interval, [0.0, 0.5]);
        let target = forward
            .prediction_set
            .iter()
            .find(|entry| entry.memory_id == "target")
            .expect("target entry");
        assert_eq!(target.score, 0.25);
        assert_eq!(target.source, "target");
        assert!(!target.included);
        assert_eq!(forward.prediction_set.len(), 2);
    }

    #[test]
    fn invalid_live_scores_abstain_even_with_sufficient_calibration() {
        let (_temp, workspace) = calibrated_workspace();
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
            for (target_score, other_score) in [(invalid, 0.125), (0.75, invalid)] {
                let report = why_conformal_confidence_intervals(
                    Some(&workspace),
                    "target",
                    target_score,
                    [candidate("other", other_score)],
                );
                assert_eq!(report.calibration_status, "conservative_invalid_scores");
                assert_eq!(report.coverage_guarantee, 0.0);
                assert_eq!(report.nonconformity_quantile, 1.0);
                assert_eq!(report.score_interval, [0.0, 1.0]);
                assert_eq!(
                    report.calibration_sample_count,
                    MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES
                );
                assert!(report.prediction_set.iter().all(|entry| entry.included));
            }
        }
    }

    #[test]
    fn signed_zero_cannot_change_tied_candidate_order() {
        let first = why_conformal_confidence_intervals(
            None,
            "target",
            0.75,
            [candidate("a", -0.0), candidate("b", 0.0)],
        );
        let second = why_conformal_confidence_intervals(
            None,
            "target",
            0.75,
            [candidate("b", -0.0), candidate("a", 0.0)],
        );
        assert_eq!(first, second);
        assert_eq!(first.prediction_set[1].memory_id, "a");
        assert_eq!(first.prediction_set[1].score.to_bits(), 0.0_f32.to_bits());
    }
}
