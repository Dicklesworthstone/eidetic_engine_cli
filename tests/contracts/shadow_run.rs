#![allow(clippy::expect_used)]

//! Gate 14: shadow-run comparison and promotion-guard contracts.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::models::{DecisionPlane, DecisionRecord};
use ee::output::{ShadowRunReport, render_shadow_run_json};
use ee::shadow::pack::{PackShadowOutput, compare_outputs};
use ee::shadow::{
    ShadowGateConfig, ShadowPromotionGuards, ShadowVerdict, candidate_promotion_allowed,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{context}: expected to contain '{needle}' but got:\n{haystack}"),
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("shadow")
        .join(format!("{name}.json.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure(actual == expected, format!("golden mismatch for {name}"))
}

#[test]
fn gate14_pack_policy_compare_records_incumbent_and_candidate_without_mutation() -> TestResult {
    let incumbent = PackShadowOutput {
        selected_ids: vec!["mem.release_rule".to_string(), "mem.old_note".to_string()],
        tokens_used: 900,
        quality_score: 0.72,
        time_us: 100,
    };
    let candidate = PackShadowOutput {
        selected_ids: vec![
            "mem.release_rule".to_string(),
            "mem.failure_evidence".to_string(),
            "mem.preflight_warning".to_string(),
        ],
        tokens_used: 980,
        quality_score: 0.86,
        time_us: 120,
    };

    let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &ShadowGateConfig::default());

    ensure(
        verdict == ShadowVerdict::CandidateBetter || verdict == ShadowVerdict::Divergent,
        "candidate should be materially different and higher quality",
    )?;
    ensure(
        metrics.candidate_quality > metrics.incumbent_quality,
        "quality improves",
    )?;
    ensure(
        incumbent.selected_ids.len() == 2,
        "incumbent output is unchanged",
    )?;
    ensure(
        candidate.selected_ids.len() == 3,
        "candidate output is unchanged",
    )
}

#[test]
fn gate14_promotion_is_blocked_by_safety_guards_even_when_candidate_wins() -> TestResult {
    let guards = ShadowPromotionGuards {
        dropped_critical_warnings: true,
        redaction_differences: true,
        p99_regression: false,
        tail_risk_regression: true,
        shadow_mismatch_above_tolerance: false,
    };

    ensure(guards.blocks_promotion(), "guards block promotion")?;
    ensure(
        guards.blocker_codes()
            == vec![
                "dropped_critical_warnings",
                "redaction_differences",
                "tail_risk_regression",
            ],
        "blocker codes are stable",
    )?;
    ensure(
        !candidate_promotion_allowed(ShadowVerdict::CandidateBetter, &guards),
        "candidate better verdict is still blocked by guards",
    )
}

#[test]
fn gate14_shadow_run_report_matches_golden() -> TestResult {
    let mut report = ShadowRunReport::new("candidate.pack.facility_location", "incumbent.pack.mmr")
        .with_command("shadow compare");

    let divergent = DecisionRecord::builder()
        .plane(DecisionPlane::Packing)
        .policy_id("candidate.pack.facility_location")
        .decision_id("decision_pack_001")
        .trace_id("trace_gate14_001")
        .decided_at("2026-05-01T00:00:00Z")
        .outcome("include:mem.failure_evidence")
        .incumbent_outcome("exclude:mem.failure_evidence")
        .reason("candidate surfaced high-severity failure evidence")
        .confidence(0.91)
        .shadow(true)
        .build();

    let matched = DecisionRecord::builder()
        .plane(DecisionPlane::CacheAdmission)
        .policy_id("candidate.cache.s3_fifo")
        .decision_id("decision_cache_001")
        .trace_id("trace_gate14_001")
        .decided_at("2026-05-01T00:00:01Z")
        .outcome("admit")
        .incumbent_outcome("admit")
        .reason("both policies admit repeated key")
        .confidence(0.83)
        .shadow(true)
        .build();

    report.add_from_record(&divergent);
    report.add_from_record(&matched);
    report.compute_avg_confidence();

    let json = render_shadow_run_json(&report);
    ensure_contains(&json, "\"divergenceRate\":0.5000", "divergence rate")?;
    ensure_contains(&json, "\"traceId\":\"trace_gate14_001\"", "trace linkage")?;
    assert_golden("pack_policy_compare", &(json + "\n"))
}
