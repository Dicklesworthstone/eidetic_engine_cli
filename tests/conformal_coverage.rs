use std::path::{Path, PathBuf};

use ee::core::conformal::{
    DEFAULT_CONFORMAL_COVERAGE, MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES, WhyConformalCandidate,
    WhyConformalConfidenceIntervals, conformal_score_interval, split_conformal_quantile,
    why_conformal_confidence_intervals,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn split_conformal_quantile_covers_held_out_fixture() -> TestResult {
    let calibration = (0..100)
        .map(|index| (index % 20) as f32 / 100.0)
        .collect::<Vec<_>>();
    let quantile = split_conformal_quantile(calibration, DEFAULT_CONFORMAL_COVERAGE);
    let held_out = (0..100)
        .map(|index| ((index * 7) % 20) as f32 / 100.0)
        .collect::<Vec<_>>();
    let covered = held_out
        .iter()
        .filter(|nonconformity| **nonconformity <= quantile)
        .count();
    let empirical = covered as f32 / held_out.len() as f32;
    ensure(
        empirical >= DEFAULT_CONFORMAL_COVERAGE,
        format!("empirical coverage {empirical} below nominal {DEFAULT_CONFORMAL_COVERAGE}"),
    )
}

#[test]
fn conformal_interval_is_ordered_and_clamped() -> TestResult {
    // Binary-exact inputs isolate ordering and endpoint clamping from decimal
    // representation error (0.9_f32 - 0.4_f32 is below 0.5 by one ULP).
    assert_eq!(conformal_score_interval(0.125, 0.375), [0.0, 0.5]);
    assert_eq!(conformal_score_interval(0.875, 0.375), [0.5, 1.0]);
    ensure(
        conformal_score_interval(f32::NAN, 0.2) == [0.0, 0.2],
        "non-finite scores should clamp to the unit interval before expansion",
    )
}

#[test]
fn why_prediction_set_ordering_is_deterministic() -> TestResult {
    let candidates = vec![
        WhyConformalCandidate {
            memory_id: "mem_b".to_owned(),
            score: 0.70,
            source: "link:supports".to_owned(),
        },
        WhyConformalCandidate {
            memory_id: "mem_a".to_owned(),
            score: 0.70,
            source: "link:supports".to_owned(),
        },
        WhyConformalCandidate {
            memory_id: "mem_c".to_owned(),
            score: 0.40,
            source: "link:derived_from".to_owned(),
        },
    ];

    let first = why_conformal_confidence_intervals(None, "mem_target", 0.80, candidates.clone());
    let second = why_conformal_confidence_intervals(None, "mem_target", 0.80, candidates);
    let first_ids = first
        .prediction_set
        .iter()
        .map(|entry| entry.memory_id.as_str())
        .collect::<Vec<_>>();
    let second_ids = second
        .prediction_set
        .iter()
        .map(|entry| entry.memory_id.as_str())
        .collect::<Vec<_>>();

    ensure(
        first_ids == second_ids,
        "prediction-set ordering must be stable",
    )?;
    ensure(
        first_ids == vec!["mem_target", "mem_a", "mem_b", "mem_c"],
        format!("unexpected prediction-set order: {first_ids:?}"),
    )?;
    ensure(
        first.prediction_set.iter().all(|entry| entry.included),
        "missing calibration should use conservative all-included set",
    )
}

#[test]
fn why_prediction_set_deduplicates_by_best_score() -> TestResult {
    let report = why_conformal_confidence_intervals(
        None,
        "mem_target",
        0.20,
        [
            WhyConformalCandidate {
                memory_id: "mem_dup".to_owned(),
                score: 0.30,
                source: "link:weak".to_owned(),
            },
            WhyConformalCandidate {
                memory_id: "mem_dup".to_owned(),
                score: 0.90,
                source: "link:strong".to_owned(),
            },
        ],
    );
    let duplicate = report
        .prediction_set
        .iter()
        .find(|entry| entry.memory_id == "mem_dup")
        .ok_or_else(|| "deduplicated candidate missing".to_owned())?;
    ensure(duplicate.score == 0.90, "dedupe must retain best score")
}

#[cfg(unix)]
#[test]
fn why_conformal_ignores_symlinked_workspace_calibration_file() -> TestResult {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = temp.path().join("workspace");
    let calibration_dir = workspace.join(".ee").join("search");
    std::fs::create_dir_all(&calibration_dir).map_err(|error| error.to_string())?;

    let outside = temp.path().join("outside-calibration.jsonl");
    let mut rows = (0..MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES)
        .map(|_| serde_json::json!({"nonconformityScore": 0.05}).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    rows.push('\n');
    std::fs::write(&outside, rows).map_err(|error| error.to_string())?;
    symlink(&outside, calibration_dir.join("calibration.jsonl"))
        .map_err(|error| error.to_string())?;

    let report = why_conformal_confidence_intervals(
        Some(&workspace),
        "mem_target",
        0.80,
        [WhyConformalCandidate {
            memory_id: "mem_other".to_owned(),
            score: 0.70,
            source: "link:supports".to_owned(),
        }],
    );

    ensure(
        report.calibration_sample_count == 0,
        "symlinked calibration file must not contribute samples",
    )?;
    ensure(
        report.calibration_status == "conservative_insufficient_calibration",
        format!(
            "symlinked calibration should fall back conservatively, got {}",
            report.calibration_status
        ),
    )?;
    ensure(
        report.nonconformity_quantile == 1.0,
        "symlinked calibration should not narrow conformal quantile",
    )
}

fn calibration_workspace() -> Result<(tempfile::TempDir, PathBuf), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    // Resolve only the trusted temporary root; production descendant-symlink
    // checks remain active on the actual calibration file.
    let workspace = temp.path().canonicalize().map_err(|error| error.to_string())?;
    std::fs::create_dir_all(workspace.join(".ee").join("search"))
        .map_err(|error| error.to_string())?;
    Ok((temp, workspace))
}

fn calibration_report(workspace: &Path) -> WhyConformalConfidenceIntervals {
    why_conformal_confidence_intervals(
        Some(workspace),
        "mem_target",
        0.75,
        [
            WhyConformalCandidate {
                memory_id: "mem_strong".to_owned(),
                score: 0.875,
                source: "fixture".to_owned(),
            },
            WhyConformalCandidate {
                memory_id: "mem_weak".to_owned(),
                score: 0.125,
                source: "fixture".to_owned(),
            },
        ],
    )
}

fn report_from_calibration(contents: &[u8]) -> Result<WhyConformalConfidenceIntervals, String> {
    let (_temp, workspace) = calibration_workspace()?;
    std::fs::write(workspace.join(".ee/search/calibration.jsonl"), contents)
        .map_err(|error| error.to_string())?;
    Ok(calibration_report(&workspace))
}

fn valid_calibration_rows(count: usize) -> String {
    "{\"nonconformityScore\":0.25}\n".repeat(count)
}

fn ensure_conservative_calibration(
    report: &WhyConformalConfidenceIntervals,
    expected_samples: usize,
) -> TestResult {
    ensure(
        report.calibration_sample_count == expected_samples,
        format!("unexpected accepted sample count: {report:?}"),
    )?;
    ensure(
        report.calibration_status == "conservative_insufficient_calibration",
        format!("unusable calibration must be conservative: {report:?}"),
    )?;
    ensure(
        report.nonconformity_quantile == 1.0 && report.score_interval == [0.0, 1.0],
        "unusable calibration must not narrow uncertainty",
    )?;
    let entries = report
        .prediction_set
        .iter()
        .map(|entry| (entry.memory_id.as_str(), entry.rank, entry.included))
        .collect::<Vec<_>>();
    ensure(
        entries == vec![("mem_strong", 1, true), ("mem_target", 2, true), ("mem_weak", 3, true)],
        format!("conservative fallback must retain every exact candidate: {entries:?}"),
    )
}

fn ensure_calibrated_selection(report: &WhyConformalConfidenceIntervals) -> TestResult {
    ensure(
        report.calibration_status == "calibrated"
            && report.calibration_sample_count == MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES,
        format!("valid calibration must remain usable: {report:?}"),
    )?;
    ensure(
        report.nonconformity_quantile == 0.25 && report.score_interval == [0.5, 1.0],
        format!("unexpected calibrated interval: {report:?}"),
    )?;
    let entries = report
        .prediction_set
        .iter()
        .map(|entry| (entry.memory_id.as_str(), entry.rank, entry.included))
        .collect::<Vec<_>>();
    ensure(
        entries == vec![("mem_strong", 1, true), ("mem_target", 2, true), ("mem_weak", 3, false)],
        format!("valid calibration must produce the exact selective set: {entries:?}"),
    )
}

#[test]
fn why_conformal_uses_valid_file_for_exact_selective_prediction_set() -> TestResult {
    let rows = valid_calibration_rows(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES);
    let report = report_from_calibration(rows.as_bytes())?;
    ensure_calibrated_selection(&report)
}

#[test]
fn why_conformal_rejected_numeric_rows_do_not_satisfy_sample_minimum() -> TestResult {
    for row in [
        r#"{"score":12.0}"#,
        r#"{"score":1e300}"#,
        r#"{"nonconformityScore":-0.25}"#,
        r#"{"nonconformityScore":1e300}"#,
        r#"{"nonconformityScore":null,"score":0.75}"#,
        r#"{"score":"invalid","fusionScore":0.75}"#,
    ] {
        let rows = format!("{row}\n").repeat(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES);
        let report = report_from_calibration(rows.as_bytes())?;
        ensure_conservative_calibration(&report, 0)?;
    }
    Ok(())
}

#[test]
fn why_conformal_counts_only_valid_rows_and_recovers_after_new_observation() -> TestResult {
    let (_temp, workspace) = calibration_workspace()?;
    let path = workspace.join(".ee/search/calibration.jsonl");
    let mut rows = valid_calibration_rows(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES - 1);
    rows.push_str("{\"score\":12.0}\n");
    std::fs::write(&path, &rows).map_err(|error| error.to_string())?;
    ensure_conservative_calibration(
        &calibration_report(&workspace),
        MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES - 1,
    )?;

    // The invalid row stays in the file. Only the new valid observation may
    // cross the minimum; there is no cache reset or threshold relaxation.
    rows.push_str("{\"score\":0.75}\n");
    std::fs::write(&path, &rows).map_err(|error| error.to_string())?;
    ensure_calibrated_selection(&calibration_report(&workspace))
}

#[test]
fn why_conformal_damaged_tail_invalidates_a_calibrated_looking_prefix() -> TestResult {
    for suffix in [b"{\"nonconformityScore\":".as_slice(), b"\xff\n".as_slice()] {
        let mut rows = valid_calibration_rows(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES).into_bytes();
        rows.extend_from_slice(suffix);
        let report = report_from_calibration(&rows)?;
        ensure_conservative_calibration(&report, 0)?;
    }
    Ok(())
}

#[test]
fn why_conformal_oversized_file_cannot_certify_its_valid_prefix() -> TestResult {
    let (_temp, workspace) = calibration_workspace()?;
    let path = workspace.join(".ee/search/calibration.jsonl");
    std::fs::write(
        &path,
        valid_calibration_rows(MIN_WHY_CONFORMAL_CALIBRATION_SAMPLES),
    )
    .map_err(|error| error.to_string())?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|error| error.to_string())?;
    // A sparse extension tests the actual 64 MiB production limit without
    // constructing a large calibration buffer in the test process.
    file.set_len(64 * 1024 * 1024 + 1)
        .map_err(|error| error.to_string())?;
    drop(file);
    ensure_conservative_calibration(&calibration_report(&workspace), 0)
}
