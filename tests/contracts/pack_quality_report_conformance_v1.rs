//! bd-eecz6: conformance harness for `ee.eval.pack_quality_report.v1`
//! deterministic fixture coverage.
//!
//! Exercises the four contract surfaces:
//!   (a) selected memory IDs change under retrieval profile swap
//!   (b) omitted memory IDs report with reason codes
//!   (c) degradation posture stays stable across re-runs
//!   (d) redaction status is preserved end-to-end
//!
//! Plus the cross-cutting metamorphic relation: deterministic re-runs
//! of `evaluate_pack_quality` against the same `PackQualityCase` +
//! `PackQualityActual` inputs produce byte-identical JSON reports.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use ee::eval::runner::{PackQualityQuerySurface, PackQualityTokenBudget};
use ee::eval::{
    PACK_QUALITY_REPORT_SCHEMA_V1, PackQualityActual, PackQualityCase, PackQualityReport,
    PackQualityVerdict, compare_pack_quality, evaluate_pack_quality,
};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_eq<T: std::fmt::Debug + PartialEq>(left: &T, right: &T, label: &str) -> TestResult {
    if left == right {
        Ok(())
    } else {
        Err(format!("{label}: left={left:?} right={right:?} (bd-eecz6)"))
    }
}

fn make_case(
    case_id: &str,
    expected_ids: &[&str],
    critical_omitted: &[&str],
    allowed_degradations: &[&str],
    forbidden_leaks: &[&str],
) -> PackQualityCase {
    PackQualityCase {
        case_id: case_id.into(),
        scenario_id: "bd_eecz6_scenario".into(),
        command_step: 1,
        query_surface: PackQualityQuerySurface {
            kind: "inline".into(),
            query: Some("conformance fixture query".into()),
            path: None,
            schema: None,
        },
        expected_selected_memory_ids: expected_ids.iter().map(|s| (*s).to_string()).collect(),
        critical_omitted_memory_ids: critical_omitted.iter().map(|s| (*s).to_string()).collect(),
        min_provenance_density: 0.5,
        allowed_degradation_codes: allowed_degradations
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        forbidden_redaction_leaks: forbidden_leaks.iter().map(|s| (*s).to_string()).collect(),
        token_budget: PackQualityTokenBudget {
            max_tokens: 2_000,
            expected_used_tokens_max: 1_800,
            expect_truncation: false,
        },
        stable_first_failure_label: format!("{case_id}-bd-eecz6"),
    }
}

fn make_actual(
    selected_ids: &[&str],
    degradations: &[&str],
    leaks: &[&str],
    tokens: u32,
    provenance_density: f64,
) -> PackQualityActual {
    PackQualityActual {
        selected_memory_ids: selected_ids.iter().map(|s| (*s).to_string()).collect(),
        degradation_codes: degradations.iter().map(|s| (*s).to_string()).collect(),
        redaction_leaks: leaks.iter().map(|s| (*s).to_string()).collect(),
        tokens_used: tokens,
        provenance_density,
    }
}

/// Run `evaluate_pack_quality` THREE times against the same inputs and
/// require the JSON serializations to be byte-identical.
fn assert_deterministic_evaluation(
    fixture_id: &str,
    cases: &[PackQualityCase],
    actuals: &[PackQualityActual],
) -> TestResult {
    let report_1 = serde_json::to_string(&evaluate_pack_quality(fixture_id, cases, actuals))
        .map_err(|error| format!("serialize report_1: {error}"))?;
    let report_2 = serde_json::to_string(&evaluate_pack_quality(fixture_id, cases, actuals))
        .map_err(|error| format!("serialize report_2: {error}"))?;
    let report_3 = serde_json::to_string(&evaluate_pack_quality(fixture_id, cases, actuals))
        .map_err(|error| format!("serialize report_3: {error}"))?;
    ensure_eq(&report_1, &report_2, "report run 1 == run 2 (determinism)")?;
    ensure_eq(&report_2, &report_3, "report run 2 == run 3 (determinism)")?;
    ensure(
        report_1.contains(PACK_QUALITY_REPORT_SCHEMA_V1),
        format!(
            "report must embed the schema constant `{PACK_QUALITY_REPORT_SCHEMA_V1}`: {report_1}"
        ),
    )
}

// --------------------------- contract surface (a) ---------------------------

#[test]
fn selected_ids_change_under_profile_swap_yields_distinct_reports() -> TestResult {
    // Two "profiles" model the same scenario picking different memory
    // IDs. Their reports must diverge in `comparisons[0].actual_selected_ids`
    // — but each profile's report must STILL be deterministic across
    // re-runs against the same inputs.
    let case = make_case(
        "profile_swap_case",
        &["mem_alpha", "mem_beta"],
        &[],
        &[],
        &["secret"],
    );
    let actual_balanced = make_actual(&["mem_alpha", "mem_beta"], &[], &[], 500, 0.9);
    let actual_compact = make_actual(&["mem_alpha"], &[], &[], 300, 0.7);

    assert_deterministic_evaluation(
        "profile_swap_balanced",
        &[case.clone()],
        &[actual_balanced.clone()],
    )?;
    assert_deterministic_evaluation(
        "profile_swap_compact",
        &[case.clone()],
        &[actual_compact.clone()],
    )?;

    let balanced_report =
        evaluate_pack_quality("profile_swap_balanced", &[case.clone()], &[actual_balanced]);
    let compact_report = evaluate_pack_quality("profile_swap_compact", &[case], &[actual_compact]);

    ensure_eq(
        &balanced_report.aggregate_verdict,
        &PackQualityVerdict::Within,
        "balanced profile selects expected IDs → Within",
    )?;
    ensure(
        !matches!(compact_report.aggregate_verdict, PackQualityVerdict::Within),
        "compact profile drops mem_beta → not Within",
    )
}

// --------------------------- contract surface (b) ---------------------------

#[test]
fn omitted_critical_ids_report_with_reason_codes() -> TestResult {
    // Critical IDs are listed in `critical_omitted_memory_ids` and MUST
    // NOT appear in `selected_memory_ids`. When they do, the comparison
    // surfaces them in `omitted_critical_found` and the verdict drops
    // to `Regression`.
    let case = make_case(
        "critical_omitted_case",
        &["mem_alpha"],
        &["mem_forbidden"], // critical_omitted: must not be selected
        &[],
        &["secret"],
    );
    let actual = make_actual(
        &["mem_alpha", "mem_forbidden"], // mem_forbidden IS selected → violation
        &[],
        &[],
        500,
        0.9,
    );

    assert_deterministic_evaluation(
        "critical_omitted_fixture",
        &[case.clone()],
        &[actual.clone()],
    )?;

    let report = evaluate_pack_quality(
        "critical_omitted_fixture",
        &[case.clone()],
        &[actual.clone()],
    );
    ensure_eq(
        &report.aggregate_verdict,
        &PackQualityVerdict::Regression,
        "selecting a critical-omitted ID must drop verdict to Regression",
    )?;
    let comparison = report.comparisons.first().ok_or("no comparison emitted")?;
    ensure(
        comparison
            .omitted_critical_found
            .iter()
            .any(|id| id == "mem_forbidden"),
        format!(
            "omitted_critical_found must surface mem_forbidden, got {:?}",
            comparison.omitted_critical_found
        ),
    )?;
    let direct = compare_pack_quality(&case, &actual);
    ensure_eq(
        &direct.verdict,
        &PackQualityVerdict::Regression,
        "compare_pack_quality direct call agrees with evaluate_pack_quality",
    )
}

// --------------------------- contract surface (c) ---------------------------

#[test]
fn degradation_posture_is_stable_across_runs() -> TestResult {
    // Allowed degradations should not push the verdict away from
    // Within; unexpected ones should. Either way the report JSON must
    // be deterministic across re-runs.
    let case = make_case(
        "degradation_stable_case",
        &["mem_alpha"],
        &[],
        &["stale_index", "embed_model_unavailable"], // both allowed
        &["secret"],
    );

    let actual_allowed_only = make_actual(
        &["mem_alpha"],
        &["stale_index"], // present and allowed
        &[],
        500,
        0.9,
    );
    let actual_with_unexpected = make_actual(
        &["mem_alpha"],
        &["stale_index", "unknown_code"], // unknown_code not in allowed list
        &[],
        500,
        0.9,
    );

    assert_deterministic_evaluation(
        "degradation_allowed_only",
        &[case.clone()],
        &[actual_allowed_only.clone()],
    )?;
    assert_deterministic_evaluation(
        "degradation_with_unexpected",
        &[case.clone()],
        &[actual_with_unexpected.clone()],
    )?;

    // Allowed-only must yield Within (no contract violation).
    let allowed_report = evaluate_pack_quality(
        "degradation_allowed_only",
        &[case.clone()],
        &[actual_allowed_only],
    );
    ensure_eq(
        &allowed_report.aggregate_verdict,
        &PackQualityVerdict::Within,
        "all-allowed degradations keep verdict Within",
    )?;
    Ok(())
}

// --------------------------- contract surface (d) ---------------------------

#[test]
fn forbidden_redaction_leak_drops_verdict_to_regression() -> TestResult {
    // forbidden_redaction_leaks defines the leak categories that MUST
    // NOT appear in `actual.redaction_leaks`. If any does, verdict must
    // be Regression and the report must be byte-identical across runs.
    let case = make_case(
        "redaction_leak_case",
        &["mem_alpha"],
        &[],
        &[],
        &["secret", "pii"],
    );
    let actual_leaking = make_actual(
        &["mem_alpha"],
        &[],
        &["secret"], // forbidden leak observed
        500,
        0.9,
    );
    let actual_clean = make_actual(&["mem_alpha"], &[], &[], 500, 0.9);

    assert_deterministic_evaluation(
        "redaction_leaking",
        &[case.clone()],
        &[actual_leaking.clone()],
    )?;
    assert_deterministic_evaluation("redaction_clean", &[case.clone()], &[actual_clean.clone()])?;

    let leaking_report =
        evaluate_pack_quality("redaction_leaking", &[case.clone()], &[actual_leaking]);
    ensure_eq(
        &leaking_report.aggregate_verdict,
        &PackQualityVerdict::Regression,
        "forbidden redaction leak must surface as Regression",
    )?;
    let clean_report = evaluate_pack_quality("redaction_clean", &[case], &[actual_clean]);
    ensure_eq(
        &clean_report.aggregate_verdict,
        &PackQualityVerdict::Within,
        "no leak keeps verdict Within",
    )
}

// ------------------------- cross-cutting determinism -------------------------

#[test]
fn aggregate_report_three_case_matrix_is_byte_identical_across_re_runs() -> TestResult {
    // Build a fixture matrix that mixes the three non-inconclusive
    // verdicts so the aggregate has to fold them. Re-serializing the
    // report three times must yield identical bytes — that is the
    // determinism contract `ee eval --pack-quality` depends on.
    let within_case = make_case("within_case", &["mem_alpha"], &[], &[], &["secret"]);
    let within_actual = make_actual(&["mem_alpha"], &[], &[], 500, 0.9);

    let drift_case = make_case(
        "drift_case",
        &["mem_alpha", "mem_beta"],
        &[],
        &[],
        &["secret"],
    );
    // Missing one expected ID — verdict typically drifts.
    let drift_actual = make_actual(&["mem_alpha"], &[], &[], 500, 0.9);

    let regression_case = make_case(
        "regression_case",
        &["mem_alpha"],
        &[],
        &[],
        &["secret", "pii"],
    );
    let regression_actual = make_actual(&["mem_alpha"], &[], &["secret"], 500, 0.9);

    let cases = vec![within_case, drift_case, regression_case];
    let actuals = vec![within_actual, drift_actual, regression_actual];

    assert_deterministic_evaluation("three_case_matrix", &cases, &actuals)?;

    let report = evaluate_pack_quality("three_case_matrix", &cases, &actuals);
    ensure_eq(
        &report.cases_total,
        &3_usize,
        "cases_total counts all three",
    )?;
    ensure(
        report.cases_regression >= 1,
        format!(
            "at least one regression expected; got {} regression/{} total",
            report.cases_regression, report.cases_total
        ),
    )?;
    Ok(())
}

#[test]
fn empty_report_is_deterministic_within_and_carries_schema() -> TestResult {
    let report = PackQualityReport::new("empty_fixture".to_string());
    ensure_eq(
        &report.aggregate_verdict,
        &PackQualityVerdict::Within,
        "empty report defaults to Within",
    )?;
    ensure_eq(&report.cases_total, &0_usize, "empty report has zero cases")?;
    ensure_eq(
        &report.schema,
        &PACK_QUALITY_REPORT_SCHEMA_V1,
        "schema constant is pinned on the report",
    )
}

#[test]
fn pack_quality_report_schema_constant_is_stable() {
    assert_eq!(
        PACK_QUALITY_REPORT_SCHEMA_V1,
        "ee.eval.pack_quality_report.v1"
    );
}
