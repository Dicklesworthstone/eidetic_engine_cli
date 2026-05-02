#![allow(clippy::expect_used)]

//! Gate 14: executable claim verification contracts.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::core::claims::{
    CLAIM_VERIFY_SCHEMA_V1, ClaimVerifyReport, ClaimVerifyResult, DiagClaimsOptions,
    DiagClaimsReport,
};
use ee::models::ManifestVerificationStatus;
use ee::output::render_claim_verify_json;

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
        .join("claims")
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
    ensure(
        actual == expected,
        format!("golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{actual}"),
    )
}

fn verified_claim_report() -> ClaimVerifyReport {
    ClaimVerifyReport {
        schema: CLAIM_VERIFY_SCHEMA_V1,
        claim_id: "claim_release_context_demo".to_string(),
        verify_all: false,
        claims_file: "fixtures/claims.yaml".to_string(),
        artifacts_dir: "fixtures/artifacts/claim_release_context_demo".to_string(),
        total_claims: 1,
        verified_count: 1,
        failed_count: 0,
        skipped_count: 0,
        fail_fast: true,
        results: vec![ClaimVerifyResult {
            claim_id: "claim_release_context_demo".to_string(),
            status: ManifestVerificationStatus::Passing,
            artifacts_checked: 3,
            artifacts_passed: 3,
            artifacts_failed: 0,
            errors: Vec::new(),
        }],
    }
}

fn regressed_claim_report() -> ClaimVerifyReport {
    ClaimVerifyReport {
        schema: CLAIM_VERIFY_SCHEMA_V1,
        claim_id: "claim_release_context_demo".to_string(),
        verify_all: false,
        claims_file: "fixtures/claims.yaml".to_string(),
        artifacts_dir: "fixtures/artifacts/claim_release_context_demo".to_string(),
        total_claims: 1,
        verified_count: 0,
        failed_count: 1,
        skipped_count: 0,
        fail_fast: true,
        results: vec![ClaimVerifyResult {
            claim_id: "claim_release_context_demo".to_string(),
            status: ManifestVerificationStatus::Failing,
            artifacts_checked: 3,
            artifacts_passed: 1,
            artifacts_failed: 2,
            errors: vec![
                "artifact_not_found: artifacts/stdout.json".to_string(),
                "stale_payload_hash: artifacts/benchmark.json".to_string(),
                "hash_mismatch: artifacts/manifest.json".to_string(),
            ],
        }],
    }
}

#[test]
fn gate14_verified_claim_has_required_evidence_fields() -> TestResult {
    let json = render_claim_verify_json(&verified_claim_report());

    ensure_contains(
        &json,
        "\"claimId\":\"claim_release_context_demo\"",
        "claim id",
    )?;
    ensure_contains(&json, "\"artifactsDir\":", "artifact manifest root")?;
    ensure_contains(&json, "\"artifactsChecked\":3", "artifact count")?;
    ensure_contains(&json, "\"status\":\"passing\"", "passing status")?;
    ensure_contains(&json, "\"success\":true", "success flag")?;
    assert_golden("verified_claim", &(json + "\n"))
}

#[test]
fn gate14_regressed_claim_reports_missing_stale_and_hash_mismatch_errors() -> TestResult {
    let json = render_claim_verify_json(&regressed_claim_report());

    ensure_contains(&json, "\"success\":false", "failure flag")?;
    ensure_contains(&json, "\"status\":\"failing\"", "failing status")?;
    ensure_contains(&json, "\"errors\":[", "errors array")?;
    ensure_contains(&json, "artifact_not_found", "missing artifact error")?;
    ensure_contains(&json, "stale_payload_hash", "stale hash error")?;
    ensure_contains(&json, "hash_mismatch", "hash mismatch error")?;
    assert_golden("regressed_claim", &(json + "\n"))
}

#[test]
fn gate14_diag_claims_missing_file_is_honest_degraded_posture() -> TestResult {
    let report = DiagClaimsReport::gather(&DiagClaimsOptions {
        workspace_path: PathBuf::from("/tmp/ee_gate14_no_claims"),
        staleness_threshold_days: 30,
        ..Default::default()
    });

    ensure(!report.claims_file_exists, "claims file should be absent")?;
    ensure(
        report.health_status == "degraded",
        "missing claims is degraded",
    )?;
    ensure(
        report
            .repair_actions
            .iter()
            .any(|action| action.contains("Create claims file")),
        "missing claims report includes repair action",
    )
}
