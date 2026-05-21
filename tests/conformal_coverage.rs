use ee::core::conformal::{
    DEFAULT_CONFORMAL_COVERAGE, WhyConformalCandidate, conformal_score_interval,
    split_conformal_quantile, why_conformal_confidence_intervals,
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
    assert_eq!(conformal_score_interval(0.1, 0.4), [0.0, 0.5]);
    assert_eq!(conformal_score_interval(0.9, 0.4), [0.5, 1.0]);
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
