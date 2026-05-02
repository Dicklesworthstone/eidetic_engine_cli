//! Gate 19 situation model contract coverage.
//!
//! Freezes the public JSON shape for situation classification, routing
//! decisions, high-risk alternatives, low-confidence broadening, and fixture
//! metrics. These tests exercise the real CLI JSON path where practical and
//! compare deterministic model outputs to checked-in golden files.

use ee::core::situation::{
    SITUATION_COMPARE_SCHEMA_V1, SITUATION_FIXTURE_METRICS_SCHEMA_V1,
    SITUATION_LINK_DRY_RUN_SCHEMA_V1, SituationCompareOptions, classify_task, compare_situations,
    evaluate_built_in_situation_fixtures, plan_situation_link_dry_run,
};
use ee::models::{
    ROUTING_DECISION_SCHEMA_V1, SITUATION_CLASSIFY_SCHEMA_V1, SITUATION_LINK_SCHEMA_V1,
    SituationRoutingSurface,
};
use serde_json::Value as JsonValue;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_json_equal(actual: Option<&JsonValue>, expected: JsonValue, context: &str) -> TestResult {
    let actual = actual.ok_or_else(|| format!("{context}: missing JSON field"))?;
    ensure(
        actual == &expected,
        format!("{context}: expected {expected:?}, got {actual:?}"),
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("situation")
        .join(format!("{name}.json.golden"))
}

fn read_golden(name: &str) -> Result<JsonValue, String> {
    let path = golden_path(name);
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("golden {} must be JSON: {error}", path.display()))
}

fn classification_envelope(text: &str) -> JsonValue {
    let result = classify_task(text);
    serde_json::json!({
        "schema": SITUATION_CLASSIFY_SCHEMA_V1,
        "success": true,
        "data": result.data_json(),
    })
}

fn release_bug_compare_options() -> SituationCompareOptions {
    SituationCompareOptions::new("fix failing release workflow", "fix broken login crash")
        .source_situation_id("sit.release_bug")
        .target_situation_id("sit.login_bug")
        .with_evidence("feat.shared.fix")
        .created_at("2026-05-01T00:00:00Z")
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[test]
fn gate19_cli_classify_release_routing_matches_golden() -> TestResult {
    let output = run_ee(&[
        "--json",
        "situation",
        "classify",
        "fix failing release workflow",
    ])?;
    ensure(
        output.status.success(),
        format!("classify exited with {}", output.status),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "json classify must not emit diagnostics on stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout must be UTF-8 JSON: {error}"))?;
    let actual: JsonValue =
        serde_json::from_str(&stdout).map_err(|error| format!("stdout JSON: {error}"))?;
    let expected = read_golden("classify_release_routing")?;

    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String(SITUATION_CLASSIFY_SCHEMA_V1.to_string()),
        "classify schema",
    )?;
    ensure_json_equal(
        actual.get("success"),
        JsonValue::Bool(true),
        "classify success",
    )?;
    ensure(
        actual == expected,
        format!("classification golden mismatch\nactual: {actual}\nexpected: {expected}"),
    )
}

#[test]
fn gate19_low_confidence_and_high_risk_goldens_are_stable() -> TestResult {
    let low_confidence = classification_envelope("docs fix");
    let high_risk = classification_envelope("fix failing release workflow");

    ensure(
        low_confidence == read_golden("low_confidence_broadening")?,
        "low-confidence broadening golden mismatch",
    )?;
    ensure(
        high_risk == read_golden("high_risk_alternative")?,
        "high-risk alternative golden mismatch",
    )?;

    let low_routes = low_confidence
        .pointer("/data/routingDecisions")
        .and_then(JsonValue::as_array)
        .ok_or("low-confidence routing decisions missing")?;
    ensure(
        low_routes
            .iter()
            .any(|route| route.get("surface").and_then(JsonValue::as_str) == Some("manual_review")),
        "low-confidence classifications must include manual review",
    )?;

    let high_routes = high_risk
        .pointer("/data/routingDecisions")
        .and_then(JsonValue::as_array)
        .ok_or("high-risk routing decisions missing")?;
    ensure(
        high_routes.iter().any(|route| {
            route.get("surface").and_then(JsonValue::as_str) == Some("tripwire_candidate")
                && route
                    .get("tripwireCandidateIds")
                    .and_then(JsonValue::as_array)
                    .is_some_and(|ids| {
                        ids.iter().any(|id| {
                            id.as_str() == Some("tripwire.situation.deployment_alternative")
                        })
                    })
        }),
        "high-risk alternative must add a deterministic tripwire candidate",
    )
}

#[test]
fn gate19_compare_and_link_dry_run_goldens_are_stable() -> TestResult {
    let compare = compare_situations(&release_bug_compare_options()).data_json();
    let link = plan_situation_link_dry_run(&release_bug_compare_options()).data_json();

    ensure_json_equal(
        compare.get("schema"),
        JsonValue::String(SITUATION_COMPARE_SCHEMA_V1.to_string()),
        "compare schema",
    )?;
    ensure_json_equal(
        compare.get("dryRun"),
        JsonValue::Bool(true),
        "compare dry-run",
    )?;
    ensure_json_equal(
        link.get("schema"),
        JsonValue::String(SITUATION_LINK_DRY_RUN_SCHEMA_V1.to_string()),
        "link dry-run schema",
    )?;
    ensure_json_equal(
        link.get("wouldWrite"),
        JsonValue::Bool(false),
        "link mutation",
    )?;
    ensure_json_equal(
        link.pointer("/plannedLink/schema"),
        JsonValue::String(SITUATION_LINK_SCHEMA_V1.to_string()),
        "planned link schema",
    )?;
    ensure(
        compare == read_golden("compare_release_bug_to_login_bug")?,
        format!(
            "situation compare golden mismatch\nactual: {compare}\nexpected: {}",
            read_golden("compare_release_bug_to_login_bug")?
        ),
    )?;
    ensure(
        link == read_golden("link_dry_run_release_bug_to_login_bug")?,
        format!(
            "situation link dry-run golden mismatch\nactual: {link}\nexpected: {}",
            read_golden("link_dry_run_release_bug_to_login_bug")?
        ),
    )
}

#[test]
fn gate19_fixture_metrics_match_golden_and_cover_gate_surfaces() -> TestResult {
    let actual = evaluate_built_in_situation_fixtures().data_json();
    let expected = read_golden("fixture_metrics")?;

    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String(SITUATION_FIXTURE_METRICS_SCHEMA_V1.to_string()),
        "fixture metrics schema",
    )?;
    ensure_json_equal(actual.get("caseCount"), serde_json::json!(9), "case count")?;
    ensure_json_equal(
        actual.get("classificationPrecision"),
        serde_json::json!(1.0),
        "classification precision",
    )?;
    ensure_json_equal(
        actual.get("routingUsefulness"),
        serde_json::json!(1.0),
        "routing usefulness",
    )?;
    ensure_json_equal(
        actual.get("alternativeRecall"),
        serde_json::json!(1.0),
        "alternative recall",
    )?;
    ensure(
        actual == expected,
        format!("fixture metrics golden mismatch\nactual: {actual}\nexpected: {expected}"),
    )
}

#[test]
fn gate19_routing_decisions_preserve_schema_and_downstream_targets() -> TestResult {
    let result = classify_task("fix failing release workflow");
    ensure(
        result
            .routing_decisions
            .iter()
            .all(|decision| decision.schema == ROUTING_DECISION_SCHEMA_V1),
        "all routing decisions must carry the stable routing schema",
    )?;

    let surfaces: Vec<&str> = result
        .routing_decisions
        .iter()
        .map(|decision| decision.surface.as_str())
        .collect();
    let missing_surface = [
        SituationRoutingSurface::ContextProfile.as_str(),
        SituationRoutingSurface::PreflightProfile.as_str(),
        SituationRoutingSurface::ProcedureCandidate.as_str(),
        SituationRoutingSurface::FixtureFamily.as_str(),
        SituationRoutingSurface::TripwireCandidate.as_str(),
        SituationRoutingSurface::CounterfactualReplay.as_str(),
    ]
    .into_iter()
    .find(|expected| !surfaces.contains(expected));
    if let Some(expected) = missing_surface {
        return Err(format!("missing routing surface {expected}"));
    }

    let fixtures = result
        .routing_decisions
        .iter()
        .find(|decision| decision.surface == SituationRoutingSurface::FixtureFamily)
        .ok_or("fixture-family route missing")?;
    ensure(
        fixtures.fixture_ids
            == vec![
                "fixture.situation.bug_fix".to_string(),
                "fixture.preflight.standard".to_string(),
            ],
        "fixture-family routing changed",
    )
}
