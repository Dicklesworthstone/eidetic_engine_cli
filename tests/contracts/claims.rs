#![allow(clippy::expect_used)]

//! Gate 14: executable claim verification contracts.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::core::claims::{
    CLAIM_VERIFY_SCHEMA_V1, ClaimListOptions, ClaimShowOptions, ClaimVerifyOptions,
    ClaimVerifyReport, ClaimVerifyResult, DiagClaimsOptions, DiagClaimsReport,
    build_claim_list_report, build_claim_show_report, build_claim_verify_report,
};
use ee::models::{ClaimId, DemoId, EvidenceId, ManifestVerificationStatus, PolicyId};
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

fn unique_claim_workspace(label: &str) -> Result<PathBuf, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let path = env::temp_dir().join(format!("ee_claims_{label}_{nonce}"));
    fs::create_dir_all(&path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    Ok(path)
}

#[derive(Debug)]
struct RealClaimFixture {
    workspace: PathBuf,
    claim_id: String,
    evidence_id: String,
    demo_id: String,
    policy_id: String,
}

fn id_uuid(seed: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(seed)
}

fn write_real_claim_fixture(
    label: &str,
    manifest_hash_override: Option<String>,
    write_manifest: bool,
) -> Result<RealClaimFixture, String> {
    let workspace = unique_claim_workspace(label)?;
    let claim_id = ClaimId::from_uuid(id_uuid(0x1000)).to_string();
    let evidence_id = EvidenceId::from_uuid(id_uuid(0x2000)).to_string();
    let demo_id = DemoId::from_uuid(id_uuid(0x3000)).to_string();
    let policy_id = PolicyId::from_uuid(id_uuid(0x4000)).to_string();

    fs::write(
        workspace.join("claims.yaml"),
        format!(
            "schema: ee.claims_file.v1\nversion: 1\nclaims:\n  - id: {claim_id}\n    title: Real manifest claim\n    description: Verifies a real artifact payload.\n    status: active\n    frequency: weekly\n    policyId: {policy_id}\n    evidenceIds:\n      - {evidence_id}\n    demoIds:\n      - {demo_id}\n    tags:\n      - release\n      - release\n"
        ),
    )
    .map_err(|error| format!("failed to write claims.yaml: {error}"))?;

    let claim_artifacts_dir = workspace.join("artifacts").join(&claim_id);
    fs::create_dir_all(&claim_artifacts_dir)
        .map_err(|error| format!("failed to create artifact dir: {error}"))?;
    let artifact_payload = b"{\"ok\":true,\"source\":\"contract\"}\n";
    fs::write(claim_artifacts_dir.join("stdout.json"), artifact_payload)
        .map_err(|error| format!("failed to write artifact payload: {error}"))?;

    if write_manifest {
        let artifact_hash = blake3::hash(artifact_payload).to_hex().to_string();
        let manifest_hash = manifest_hash_override.unwrap_or(artifact_hash);
        let manifest = serde_json::json!({
            "schema": "ee.claim_manifest.v1",
            "claimId": claim_id,
            "verificationStatus": "passing",
            "lastVerifiedAt": "2026-01-02T03:04:05Z",
            "artifacts": [
                {
                    "path": "stdout.json",
                    "artifactType": "report",
                    "blake3Hash": manifest_hash,
                    "sizeBytes": artifact_payload.len(),
                    "createdAt": "2026-01-02T03:04:05Z"
                }
            ]
        });
        fs::write(
            claim_artifacts_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("failed to write manifest.json: {error}"))?;
    }

    Ok(RealClaimFixture {
        workspace,
        claim_id,
        evidence_id,
        demo_id,
        policy_id,
    })
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
fn gate14_core_claim_reports_parse_and_verify_real_manifest_hashes() -> TestResult {
    let fixture = write_real_claim_fixture("core_real_manifest", None, true)?;

    let list = build_claim_list_report(&ClaimListOptions {
        workspace_path: fixture.workspace.clone(),
        status: Some("active".to_string()),
        frequency: Some("weekly".to_string()),
        tag: Some("release".to_string()),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;

    ensure(list.claims_file_exists, "claims file should exist")?;
    ensure(list.total_count == 1, "one real claim should be counted")?;
    ensure(list.filtered_count == 1, "claim should match filters")?;
    let summary = list
        .claims
        .first()
        .ok_or_else(|| "filtered claim should be present".to_string())?;
    ensure(summary.id == fixture.claim_id, "summary id comes from YAML")?;
    ensure(summary.title == "Real manifest claim", "summary title")?;
    ensure(
        summary.tags == vec!["release"],
        "tags are sorted and deduped",
    )?;
    ensure(summary.evidence_count == 1, "summary evidence count")?;
    ensure(summary.demo_count == 1, "summary demo count")?;

    let show = build_claim_show_report(&ClaimShowOptions {
        workspace_path: fixture.workspace.clone(),
        claim_id: fixture.claim_id.clone(),
        include_manifest: true,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;

    ensure(show.found, "claim show should find real claim")?;
    let claim = show
        .claim
        .as_ref()
        .ok_or_else(|| "claim details should be present".to_string())?;
    ensure(
        claim.policy_id.as_deref() == Some(fixture.policy_id.as_str()),
        "policy id comes from YAML",
    )?;
    ensure(
        claim.evidence_ids == vec![fixture.evidence_id.clone()],
        "evidence ids come from YAML",
    )?;
    ensure(
        claim.demo_ids == vec![fixture.demo_id.clone()],
        "demo ids come from YAML",
    )?;
    let manifest = show
        .manifest
        .as_ref()
        .ok_or_else(|| "manifest details should be present".to_string())?;
    ensure(manifest.artifact_count == 1, "manifest artifact count")?;
    ensure(
        manifest.verification_status == ManifestVerificationStatus::Passing,
        "manifest status is parsed",
    )?;

    let verify = build_claim_verify_report(&ClaimVerifyOptions {
        workspace_path: fixture.workspace,
        claim_id: fixture.claim_id.clone(),
        fail_fast: true,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;

    ensure(verify.verified_count == 1, "verified claim count")?;
    ensure(verify.failed_count == 0, "no failed claims")?;
    let result = verify
        .results
        .first()
        .ok_or_else(|| "verification result should be present".to_string())?;
    ensure(
        result.status == ManifestVerificationStatus::Passing,
        "claim verification status",
    )?;
    ensure(result.artifacts_checked == 1, "artifact checked count")?;
    ensure(result.artifacts_passed == 1, "artifact passed count")?;
    ensure(result.artifacts_failed == 0, "artifact failed count")?;
    ensure(result.errors.is_empty(), "passing claim has no errors")
}

#[test]
fn gate14_core_claim_verify_reports_hash_mismatch_from_real_manifest() -> TestResult {
    let fixture = write_real_claim_fixture("core_hash_mismatch", Some("0".repeat(64)), true)?;

    let verify = build_claim_verify_report(&ClaimVerifyOptions {
        workspace_path: fixture.workspace,
        claim_id: fixture.claim_id,
        fail_fast: true,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;

    ensure(verify.verified_count == 0, "no verified claims")?;
    ensure(verify.failed_count == 1, "one failed claim")?;
    let result = verify
        .results
        .first()
        .ok_or_else(|| "verification result should be present".to_string())?;
    ensure(
        result.status == ManifestVerificationStatus::Failing,
        "hash mismatch is failing",
    )?;
    ensure(result.artifacts_checked == 1, "artifact was checked")?;
    ensure(
        result.artifacts_passed == 0,
        "mismatched artifact is not passed",
    )?;
    ensure(result.artifacts_failed == 1, "mismatched artifact failed")?;
    ensure(
        result
            .errors
            .iter()
            .any(|error| error.contains("hash_mismatch")),
        "hash mismatch error is reported",
    )
}

#[test]
fn gate14_core_claim_verify_reports_missing_manifest_unavailable() -> TestResult {
    let fixture = write_real_claim_fixture("core_missing_manifest", None, false)?;

    let verify = build_claim_verify_report(&ClaimVerifyOptions {
        workspace_path: fixture.workspace,
        claim_id: fixture.claim_id,
        fail_fast: true,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;

    ensure(verify.verified_count == 0, "no verified claims")?;
    ensure(verify.failed_count == 1, "missing manifest fails claim")?;
    let result = verify
        .results
        .first()
        .ok_or_else(|| "verification result should be present".to_string())?;
    ensure(
        result.status == ManifestVerificationStatus::Failing,
        "missing manifest is failing",
    )?;
    ensure(result.artifacts_checked == 0, "no artifacts checked")?;
    ensure(result.artifacts_failed == 1, "missing manifest is counted")?;
    ensure(
        result
            .errors
            .iter()
            .any(|error| error.contains("manifest_unavailable")),
        "missing manifest error is reported",
    )
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

#[test]
fn gate14_diag_claims_existing_file_uses_real_claim_records() -> TestResult {
    let workspace = unique_claim_workspace("real_records")?;
    let claim_id = ClaimId::from_uuid(uuid::Uuid::nil()).to_string();
    let evidence_id = EvidenceId::from_uuid(uuid::Uuid::nil()).to_string();
    fs::write(
        workspace.join("claims.yaml"),
        format!(
            "schema: ee.claims_file.v1\nversion: 1\nclaims:\n  - id: {claim_id}\n    title: Real stale claim\n    status: stale\n    frequency: weekly\n    evidenceIds:\n      - {evidence_id}\n"
        ),
    )
    .map_err(|error| format!("failed to write claims.yaml: {error}"))?;

    let report = DiagClaimsReport::gather(&DiagClaimsOptions {
        workspace_path: workspace,
        staleness_threshold_days: 30,
        include_verified: false,
        ..Default::default()
    });

    ensure(report.claims_file_exists, "claims file should exist")?;
    ensure(report.counts.total == 1, "one real claim should be counted")?;
    ensure(report.counts.stale == 1, "stale claim should be counted")?;
    ensure(report.entries.len() == 1, "stale claim should be emitted")?;
    let entry = report
        .entries
        .first()
        .ok_or_else(|| "stale claim should be present".to_string())?;
    ensure(
        entry.id == claim_id,
        "diagnostic id should come from claims.yaml",
    )?;
    ensure(
        entry.title == "Real stale claim",
        "diagnostic title should come from claims.yaml",
    )?;
    ensure(
        entry.posture.as_str() == "stale",
        "diagnostic posture should come from real status",
    )?;
    ensure(
        entry.frequency.as_str() == "weekly",
        "diagnostic frequency should come from claims.yaml",
    )?;
    ensure(
        entry.evidence_count == 1,
        "diagnostic evidence count should come from claims.yaml",
    )?;
    ensure(
        report
            .entries
            .iter()
            .all(|entry| !entry.id.starts_with("claim-00") && !entry.title.contains("Example")),
        "diagnostics must not emit sample claim entries",
    )
}

#[test]
fn gate14_diag_claims_invalid_existing_file_degrades_without_examples() -> TestResult {
    let workspace = unique_claim_workspace("invalid_file")?;
    fs::write(
        workspace.join("claims.yaml"),
        "schema: ee.claims_file.v1\nclaims:\n  - id: claim_fixture_001\n    title: Placeholder verification must not pass\n",
    )
    .map_err(|error| format!("failed to write claims.yaml: {error}"))?;

    let report = DiagClaimsReport::gather(&DiagClaimsOptions {
        workspace_path: workspace,
        staleness_threshold_days: 30,
        ..Default::default()
    });

    ensure(report.claims_file_exists, "claims file should exist")?;
    ensure(
        report.health_status == "degraded",
        "invalid claims file should degrade",
    )?;
    ensure(
        report.entries.is_empty(),
        "invalid claims file must not produce diagnostic entries",
    )?;
    ensure(
        report
            .repair_actions
            .iter()
            .any(|action| action.contains("invalid claim id")),
        "invalid claims report includes parse repair action",
    )?;
    ensure(
        report
            .repair_actions
            .iter()
            .all(|action| !action.contains("Example")),
        "invalid claims report must not mention sample claims",
    )
}
