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
use ee::models::SITUATION_CLASSIFY_SCHEMA_V1;
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

fn assert_situation_cli_degraded(
    output: std::process::Output,
    command: &str,
) -> Result<JsonValue, String> {
    ensure(
        output.status.code() == Some(6),
        format!(
            "situation CLI must exit 6 while unavailable: {}",
            output.status
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "json situation command must not emit diagnostics on stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout must be UTF-8 JSON: {error}"))?;
    let actual: JsonValue =
        serde_json::from_str(&stdout).map_err(|error| format!("stdout JSON: {error}"))?;
    ensure_json_equal(
        actual.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "degraded response schema",
    )?;
    ensure_json_equal(
        actual.get("success"),
        JsonValue::Bool(false),
        "degraded success flag",
    )?;
    ensure_json_equal(
        actual.pointer("/data/command"),
        JsonValue::String(command.to_owned()),
        "degraded command",
    )?;
    ensure_json_equal(
        actual.pointer("/data/code"),
        JsonValue::String("situation_decisioning_unavailable".to_owned()),
        "degraded code",
    )?;
    ensure_json_equal(
        actual.pointer("/data/followUpBead"),
        JsonValue::String("eidetic_engine_cli-6cks".to_owned()),
        "follow-up bead",
    )?;
    ensure_json_equal(
        actual.pointer("/data/evidenceIds"),
        serde_json::json!([]),
        "degraded evidence ids",
    )?;
    Ok(actual)
}

#[test]
fn gate19_cli_classify_release_routing_degrades_until_boundary_rework() -> TestResult {
    let output = run_ee(&[
        "--json",
        "situation",
        "classify",
        "fix failing release workflow",
    ])?;
    let actual = assert_situation_cli_degraded(output, "situation classify")?;
    ensure_json_equal(
        actual.pointer("/data/sideEffectClass"),
        JsonValue::String(
            "conservative abstention; no situation routing, link, or recommendation mutation"
                .to_owned(),
        ),
        "side effect class",
    )
}

#[test]
fn gate19_cli_classify_async_migration_degrades_until_boundary_rework() -> TestResult {
    let output = run_ee(&[
        "--json",
        "situation",
        "classify",
        "migrate async runtime from tokio to asupersync",
    ])?;
    assert_situation_cli_degraded(output, "situation classify").map(|_| ())
}

#[test]
fn gate19_heuristic_tag_goldens_are_stable_and_non_decisioning() -> TestResult {
    let low_confidence = classification_envelope("docs fix");
    let high_risk = classification_envelope("fix failing release workflow");
    let async_migration = classification_envelope("migrate async runtime from tokio to asupersync");

    ensure(
        low_confidence == read_golden("low_confidence_broadening")?,
        "low-confidence broadening golden mismatch",
    )?;
    ensure(
        high_risk == read_golden("high_risk_alternative")?,
        "high-risk alternative golden mismatch",
    )?;
    ensure(
        async_migration == read_golden("classify_async_migration")?,
        "async migration classification golden mismatch",
    )?;

    for (name, envelope) in [
        ("low-confidence", &low_confidence),
        ("high-risk", &high_risk),
        ("async-migration", &async_migration),
    ] {
        ensure_json_equal(
            envelope.pointer("/data/classificationMode"),
            JsonValue::String("heuristic_tagging".to_owned()),
            format!("{name} mode").as_str(),
        )?;
        ensure_json_equal(
            envelope.pointer("/data/heuristic"),
            JsonValue::Bool(true),
            format!("{name} heuristic flag").as_str(),
        )?;
        ensure_json_equal(
            envelope.pointer("/data/decisioningAllowed"),
            JsonValue::Bool(false),
            format!("{name} decisioning").as_str(),
        )?;
        ensure_json_equal(
            envelope.pointer("/data/confidence"),
            JsonValue::String("low".to_owned()),
            format!("{name} confidence").as_str(),
        )?;
        ensure_json_equal(
            envelope.pointer("/data/routingDecisions"),
            serde_json::json!([]),
            format!("{name} routes").as_str(),
        )?;
    }

    let low_routes = low_confidence
        .pointer("/data/routingDecisions")
        .and_then(JsonValue::as_array)
        .ok_or("low-confidence routing decisions missing")?;
    ensure(
        low_routes.is_empty(),
        "low-confidence classifications must not route",
    )?;

    ensure(
        high_risk
            .pointer("/data/alternativeCategories")
            .and_then(JsonValue::as_array)
            .is_some_and(|alternatives| {
                alternatives.iter().any(|entry| {
                    entry.get("category").and_then(JsonValue::as_str) == Some("deployment")
                })
            }),
        "high-risk alternative must remain visible as a heuristic tag",
    )
}

#[test]
fn gate19_cli_explain_degrades_until_stored_situations_exist() -> TestResult {
    let output = run_ee(&["--json", "situation", "explain", "sit.release_bug"])?;
    assert_situation_cli_degraded(output, "situation explain").map(|_| ())
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
    ensure_json_equal(link.get("plannedLink"), JsonValue::Null, "planned link")?;
    ensure_json_equal(
        compare.get("recommended"),
        JsonValue::Bool(false),
        "compare recommendation",
    )?;
    ensure_json_equal(
        compare.pointer("/overlap/routingTargets"),
        serde_json::json!([]),
        "routing targets",
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
        serde_json::json!(0.0),
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
fn gate19_heuristic_tags_never_emit_downstream_routes() -> TestResult {
    let result = classify_task("fix failing release workflow");
    ensure(
        result.routing_decisions.is_empty(),
        "heuristic tags must not route",
    )?;
    let json = result.data_json();
    ensure_json_equal(
        json.get("decisioningAllowed"),
        JsonValue::Bool(false),
        "decisioning allowed",
    )?;
    ensure_json_equal(
        json.get("plannerEligible"),
        JsonValue::Bool(false),
        "planner eligible",
    )
}
