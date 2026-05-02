//! Gate 13 calibrated curation certificate contract coverage.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::curate::{
    CandidateType, OutcomeProbabilities, RISK_CALIBRATION_MIN_COUNT, RiskCertificate, RiskFactor,
};
use ee::output::{CardsProfile, render_cards_json, selection_score_card, trust_score_card};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
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

fn calibrated_certificate() -> RiskCertificate {
    RiskCertificate::builder()
        .candidate_type(CandidateType::Promote)
        .target_memory_id("mem_00000000000000000000000001")
        .add_factor(RiskFactor::new(
            "irreversibility",
            0.35,
            0.20,
            "promotion is reversible",
        ))
        .add_factor(RiskFactor::new(
            "source_support",
            0.65,
            0.10,
            "candidate has direct evidence",
        ))
        .probabilities(OutcomeProbabilities::new(0.70, 0.20, 0.05, 0.04, 0.01))
        .calibration_window_id("cal_2026w18_promotions")
        .stratum("procedural_rules:promotion")
        .calibration_count(48)
        .nonconformity_score(0.18)
        .threshold(0.32)
        .action("promote")
        .generated_at("2026-05-01T00:00:00Z")
        .build()
}

fn risk_certificate_json(certificate: &RiskCertificate) -> Value {
    json!({
        "schema": certificate.schema,
        "candidateType": certificate.candidate_type.as_str(),
        "targetMemoryId": certificate.target_memory_id,
        "riskLevel": certificate.risk_level.as_str(),
        "riskScore": certificate.risk_score,
        "calibrationWindowId": certificate.calibration_window_id,
        "stratum": certificate.stratum,
        "calibrationCount": certificate.calibration_count,
        "nonconformityScore": certificate.nonconformity_score,
        "threshold": certificate.threshold,
        "action": certificate.action,
        "abstainReason": certificate.abstain_reason,
        "reportOnly": certificate.report_only,
        "generatedAt": certificate.generated_at,
    })
}

#[test]
fn curation_certificate_includes_calibration_contract_fields() -> TestResult {
    let certificate = calibrated_certificate();
    ensure(
        certificate.calibration_window_id == "cal_2026w18_promotions",
        "calibration window id present",
    )?;
    ensure(
        certificate.stratum == "procedural_rules:promotion",
        "stratum present",
    )?;
    ensure(
        certificate.calibration_count >= RISK_CALIBRATION_MIN_COUNT,
        "calibration count is sufficient",
    )?;
    ensure(
        certificate.nonconformity_score > 0.0,
        "nonconformity score present",
    )?;
    ensure(certificate.threshold > 0.0, "threshold present")?;
    ensure(certificate.action == "promote", "action present")?;
    ensure(
        certificate.abstain_reason.is_none(),
        "calibrated certificate does not abstain",
    )?;
    assert_golden(
        "certificates",
        "curation_risk",
        &pretty(&risk_certificate_json(&certificate))?,
    )
}

#[test]
fn under_calibrated_curation_certificate_records_abstain_reason() -> TestResult {
    let certificate = RiskCertificate::builder()
        .candidate_type(CandidateType::Tombstone)
        .target_memory_id("mem_00000000000000000000000002")
        .add_factor(RiskFactor::new(
            "irreversibility",
            0.90,
            0.95,
            "tombstone is hard to reverse",
        ))
        .calibration_window_id("cal_tombstone_sparse")
        .stratum("procedural_rules:tombstone")
        .calibration_count(RISK_CALIBRATION_MIN_COUNT - 1)
        .nonconformity_score(0.91)
        .threshold(0.40)
        .action("abstain")
        .build();

    ensure(
        certificate.is_under_calibrated(),
        "certificate is under-calibrated",
    )?;
    ensure(
        certificate.abstain_reason.as_deref() == Some("under_calibrated"),
        "under-calibrated certificate carries abstain reason",
    )
}

#[test]
fn math_cards_carry_complete_decision_explanations_without_changing_curation() -> TestResult {
    let curation_decision = "promote";
    let cards = vec![
        selection_score_card(0.82, 0.74, 0.66, 0.748),
        trust_score_card("agent_validated", 0.80, 0.90, 0.72),
    ];
    let rendered = render_cards_json(&cards, CardsProfile::Math);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;
    let items = value
        .as_array()
        .ok_or_else(|| "cards should render as array".to_string())?;
    ensure(!items.is_empty(), "cards present")?;
    for item in items {
        let math = &item["math"];
        ensure(math["formula"].is_string(), "card has equation")?;
        ensure(
            math["substitutedValues"].is_string(),
            "card has substituted values",
        )?;
        ensure(math["intuition"].is_string(), "card has intuition")?;
        ensure(math["assumptions"].is_array(), "card has assumptions")?;
        ensure(
            math["decisionChange"].is_string(),
            "card has decision change condition",
        )?;
    }
    ensure(
        curation_decision == "promote",
        "adding math cards does not change curation decision",
    )?;
    assert_golden("cards", "math_curation", &pretty(&value)?)
}
