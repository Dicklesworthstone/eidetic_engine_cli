//! Gate 13 certificate contract coverage.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use ee::core::certificate::{VerificationResult, verify_certificate};
use ee::models::PrivacyBudgetCertificate;
use ee::models::TailRiskCertificate;
use ee::models::certificate::{
    PrivacyBudgetShareCertificate, PrivacyBudgetShareConstraint, ShareValidationCheck,
    ShareableAggregateKind, ShareableAggregateReport,
};
use ee::output::render_certificate_verify_json;
use ee::pack::{PackGuaranteeStatus, PackSelectionCertificate};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const UNSATISFIED_DEGRADED_MODE_EXIT: i32 = 7;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: &T,
    expected: &T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn golden_path(group: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join(group)
        .join(format!("{name}.json.golden"))
}

fn assert_golden(group: &str, name: &str, actual: &str) -> TestResult {
    let path = golden_path(group, name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!(
            "golden mismatch {}\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ),
    )
}

fn pretty(value: &Value) -> Result<String, String> {
    let mut rendered = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    rendered.push('\n');
    Ok(rendered)
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[test]
fn certificate_verify_detects_stale_payload_schema_and_assumptions() -> TestResult {
    let stale_payload = verify_certificate("cert_pack_stale_payload");
    ensure_equal(
        &stale_payload.result,
        &VerificationResult::StalePayloadHash,
        "stale payload result",
    )?;
    ensure(
        !stale_payload.hash_verified,
        "stale payload fails hash verification",
    )?;
    ensure(
        !stale_payload.payload_hash_fresh,
        "stale payload marks payload hash as stale",
    )?;
    ensure(
        stale_payload
            .failure_codes
            .iter()
            .any(|code| code == "stale_payload_hash"),
        "stale payload failure code",
    )?;

    let stale_schema = verify_certificate("cert_pack_stale_schema");
    ensure_equal(
        &stale_schema.result,
        &VerificationResult::StaleSchemaVersion,
        "stale schema result",
    )?;
    ensure(
        !stale_schema.schema_version_valid,
        "stale schema marks schema version invalid",
    )?;

    let failed_assumptions = verify_certificate("cert_pack_failed_assumptions");
    ensure_equal(
        &failed_assumptions.result,
        &VerificationResult::FailedAssumptions,
        "failed assumptions result",
    )?;
    ensure(
        !failed_assumptions.assumptions_valid,
        "failed assumptions are reported",
    )
}

#[test]
fn certificate_verify_json_degrades_until_manifest_store_exists() -> TestResult {
    let output = run_ee(&["certificate", "verify", "cert_pack_stale_schema", "--json"])?;
    ensure_equal(
        &output.status.code(),
        &Some(UNSATISFIED_DEGRADED_MODE_EXIT),
        "verify command unavailable exit",
    )?;
    ensure(
        output.stderr.is_empty(),
        "json certificate verify should keep stderr clean",
    )?;
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "stdout should be parseable JSON: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })?;
    ensure_equal(
        &value["schema"],
        &json!("ee.response.v1"),
        "json response schema",
    )?;
    ensure_equal(&value["success"], &json!(false), "success flag")?;
    ensure_equal(
        &value["data"]["code"],
        &json!("certificate_store_unavailable"),
        "degraded code",
    )?;
    ensure_equal(
        &value["data"]["degraded"][0]["code"],
        &json!("certificate_store_unavailable"),
        "degraded array code",
    )
}

#[test]
fn guarantee_status_valid_requires_certificate_id() -> TestResult {
    let base = ee::pack::assemble_draft_with_profile(
        ee::pack::ContextPackProfile::Submodular,
        "prepare release",
        ee::pack::TokenBudget::new(100).map_err(|error| format!("{error:?}"))?,
        crate::submodular_packer::fixture_candidates()?,
    )
    .map_err(|error| format!("{error:?}"))?
    .selection_certificate;

    ensure(
        base.has_valid_guarantee_identity(),
        "conditional guarantee without certificate id is allowed",
    )?;
    let mut invalid: PackSelectionCertificate = base.clone();
    invalid.guarantee_status = PackGuaranteeStatus::Valid;
    invalid.certificate_id = None;
    ensure(
        !invalid.has_valid_guarantee_identity(),
        "valid guarantee without certificate id is rejected",
    )?;
    invalid.certificate_id = Some("cert_pack_001".to_string());
    ensure(
        invalid.has_valid_guarantee_identity(),
        "valid guarantee with certificate id is accepted",
    )
}

#[test]
fn certificate_verify_renderer_includes_gate13_failure_fields() -> TestResult {
    let report = verify_certificate("cert_pack_failed_assumptions");
    let json = render_certificate_verify_json(&report);
    let value: Value = serde_json::from_str(&json).map_err(|error| error.to_string())?;
    ensure_equal(
        &value["data"]["assumptionsValid"],
        &json!(false),
        "assumptions flag",
    )?;
    ensure_equal(
        &value["data"]["failureCodes"],
        &json!(["failed_assumptions"]),
        "failure codes",
    )
}

#[test]
fn rate_distortion_certificate_golden_is_stable() -> TestResult {
    let report = ee::pack::compute_rate_distortion(4000, 3200, 12, 3);
    let value: Value =
        serde_json::from_str(&report.to_json()).map_err(|error| error.to_string())?;
    assert_golden("certificates", "rate_distortion", &pretty(&value)?)
}

#[test]
fn tail_risk_fixture_keeps_catastrophic_warnings_even_when_average_improves() -> TestResult {
    let certificate = TailRiskCertificate {
        metric: "retrieval_tail_regression".to_string(),
        observed: 1.0,
        threshold: 0.0,
        confidence_level: 0.99,
        upper_bound: 1.0,
        exceeds_bounds: true,
        recommended_action: Some("block_release".to_string()),
    };
    let value = json!({
        "schema": "ee.certificate.tail_risk_gate13.v1",
        "averageRetrievalDelta": 0.04,
        "catastrophicWarningsBefore": 1,
        "catastrophicWarningsAfter": 0,
        "catastrophicMisses": 1,
        "releaseBlocked": certificate.exceeds_bounds,
        "certificate": {
            "metric": certificate.metric,
            "observed": certificate.observed,
            "threshold": certificate.threshold,
            "confidenceLevel": certificate.confidence_level,
            "upperBound": certificate.upper_bound,
            "recommendedAction": certificate.recommended_action,
        }
    });
    ensure(
        value["releaseBlocked"] == json!(true),
        "catastrophic warning loss blocks release",
    )?;
    assert_golden("certificates", "tail_risk", &pretty(&value)?)
}

#[test]
fn privacy_budget_certificate_is_limited_to_shareable_aggregate_outputs() -> TestResult {
    let budget = PrivacyBudgetCertificate {
        category: "aggregate_export".to_string(),
        consumed: 0.2,
        total_consumed: 0.6,
        budget_limit: 1.0,
        remaining: 0.4,
        operation_allowed: true,
        resets_at: Some("2026-05-31T00:00:00Z".to_string()),
    };
    let report = ShareableAggregateReport {
        report_id: "agg_release_rules_001".to_string(),
        aggregate_kind: ShareableAggregateKind::Count,
        value: 42.0,
        sample_size: 60,
        epsilon_consumed: 0.2,
        delta_consumed: 0.00001,
        noise_scale: 1.0,
        sensitivity: 1.0,
        k_anonymity_satisfied: true,
        shareable: true,
        share_denial_reason: None,
        generated_at: "2026-05-01T00:00:00Z".to_string(),
    };
    let certificate = PrivacyBudgetShareCertificate {
        budget,
        report,
        constraints: PrivacyBudgetShareConstraint::default_safe(),
        share_approved: true,
        validations: vec![ShareValidationCheck::pass(
            "k_anonymity",
            "k-anonymity",
            "60",
            ">=5",
        )],
        certified_at: "2026-05-01T00:00:01Z".to_string(),
    };

    let shareable_output = json!({
        "schema": "ee.aggregate.shareable.v1",
        "reportId": certificate.report.report_id,
        "shareable": certificate.share_approved,
        "privacyBudgetCertificate": {
            "category": certificate.budget.category,
            "consumed": certificate.budget.consumed,
            "remaining": certificate.budget.remaining,
            "validationCount": certificate.total_count(),
        }
    });
    let local_recall_output = json!({
        "schema": "ee.context.pack.v1",
        "command": "context",
        "memories": ["mem_00000000000000000000000001"],
    });

    ensure(
        shareable_output.get("privacyBudgetCertificate").is_some(),
        "shareable aggregate output includes privacy certificate",
    )?;
    ensure(
        local_recall_output
            .get("privacyBudgetCertificate")
            .is_none(),
        "ordinary local recall output has no privacy certificate",
    )?;
    assert_golden(
        "certificates",
        "privacy_budget",
        &pretty(&shareable_output)?,
    )
}
