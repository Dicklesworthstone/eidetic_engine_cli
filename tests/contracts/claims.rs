#![allow(clippy::expect_used)]

//! Gate 14: executable claim verification contracts.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use ee::core::claims::{
    CLAIM_VERIFY_SCHEMA_V1, ClaimListOptions, ClaimShowOptions, ClaimVerifyOptions,
    ClaimVerifyReport, ClaimVerifyResult, DiagClaimsOptions, DiagClaimsReport,
    build_claim_list_report, build_claim_show_report, build_claim_verify_report,
};
use ee::models::{ClaimId, DemoId, EvidenceId, ManifestVerificationStatus, PolicyId};
use ee::output::render_claim_verify_json;
use serde_json::{Value, json};

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
    tempfile::Builder::new()
        .prefix(&format!("ee_claims_{label}_"))
        .tempdir()
        .map(tempfile::TempDir::keep)
        .map_err(|error| format!("failed to create temporary claim workspace: {error}"))
}

fn run_ee(args: &[String]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_stdout_json(output: &std::process::Output) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "stdout should be parseable JSON: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
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

#[derive(Debug)]
struct EvidenceClaimFixture {
    workspace: PathBuf,
    file_claim_id: String,
}

fn write_evidence_claim_fixture(label: &str, ttl: &str) -> Result<EvidenceClaimFixture, String> {
    let workspace = unique_claim_workspace(label)?;
    let ee_dir = workspace.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| format!("failed to create .ee dir: {error}"))?;
    let evidence_dir = workspace.join("evidence");
    fs::create_dir_all(&evidence_dir)
        .map_err(|error| format!("failed to create evidence dir: {error}"))?;

    let file_claim_id = ClaimId::from_uuid(id_uuid(0x5000)).to_string();
    let command_claim_id = ClaimId::from_uuid(id_uuid(0x5001)).to_string();
    let memory_claim_id = ClaimId::from_uuid(id_uuid(0x5002)).to_string();
    let rule_claim_id = ClaimId::from_uuid(id_uuid(0x5003)).to_string();
    let payload = b"claim evidence payload\n";
    fs::write(evidence_dir.join("payload.txt"), payload)
        .map_err(|error| format!("failed to write payload evidence: {error}"))?;
    let payload_hash = blake3::hash(payload).to_hex().to_string();

    fs::write(
        ee_dir.join("memories.jsonl"),
        "{\"id\":\"memory-release-check\",\"body\":\"Run release checks\"}\n",
    )
    .map_err(|error| format!("failed to write memory store: {error}"))?;
    fs::write(
        ee_dir.join("rules.jsonl"),
        "{\"id\":\"rule-release-check\",\"status\":\"active\"}\n",
    )
    .map_err(|error| format!("failed to write rule store: {error}"))?;

    fs::write(
        ee_dir.join("claims.yaml"),
        format!(
            "schema: ee.claims_file.v1\nversion: 1\nclaims:\n  - claim_id: {file_claim_id}\n    statement: File hash claim\n    status: unverified\n    owner: qa\n    ttl: \"{ttl}\"\n    evidence:\n      kind: file-hash\n      target: evidence/payload.txt\n      expected_hash: {payload_hash}\n  - claim_id: {command_claim_id}\n    statement: Command exits successfully\n    status: unverified\n    owner: qa\n    ttl: \"{ttl}\"\n    evidence:\n      kind: command-exit\n      target: rustc --version\n      expected_exit: 0\n  - claim_id: {memory_claim_id}\n    statement: Memory exists\n    status: unverified\n    owner: qa\n    ttl: \"{ttl}\"\n    evidence:\n      kind: memory-presence\n      target: memory-release-check\n  - claim_id: {rule_claim_id}\n    statement: Rule is active\n    status: unverified\n    owner: qa\n    ttl: \"{ttl}\"\n    evidence:\n      kind: rule-status\n      target: rule-release-check\n      expected_status: active\n"
        ),
    )
    .map_err(|error| format!("failed to write .ee/claims.yaml: {error}"))?;

    Ok(EvidenceClaimFixture {
        workspace,
        file_claim_id,
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
            evidence_checked: 3,
            evidence_passed: 3,
            evidence_failed: 0,
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
            evidence_checked: 3,
            evidence_passed: 1,
            evidence_failed: 2,
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
fn gate14_core_claim_verify_supports_claims_yaml_evidence_kinds() -> TestResult {
    let fixture = write_evidence_claim_fixture("core_evidence_kinds", "2999-01-01T00:00:00Z")?;

    let list = build_claim_list_report(&ClaimListOptions {
        workspace_path: fixture.workspace.clone(),
        status: Some("unverified".to_string()),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    ensure(list.claims_file_exists, ".ee/claims.yaml should exist")?;
    ensure(list.total_count == 4, "four executable claims are parsed")?;
    ensure(list.filtered_count == 4, "status filter handles unverified")?;
    ensure(
        list.claims
            .iter()
            .all(|claim| claim.owner.as_deref() == Some("qa")),
        "claim owner comes from claims.yaml",
    )?;

    let show = build_claim_show_report(&ClaimShowOptions {
        workspace_path: fixture.workspace.clone(),
        claim_id: fixture.file_claim_id.clone(),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    let claim = show
        .claim
        .as_ref()
        .ok_or_else(|| "claim details should exist".to_string())?;
    ensure(claim.evidence.len() == 1, "claim show includes evidence")?;
    ensure(
        claim.evidence[0].kind == "file-hash",
        "evidence kind is parsed",
    )?;

    let verify = build_claim_verify_report(&ClaimVerifyOptions {
        workspace_path: fixture.workspace,
        claim_id: "all".to_string(),
        fail_fast: false,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    ensure(verify.verified_count == 4, "all evidence kinds verify")?;
    ensure(verify.failed_count == 0, "no evidence claims fail")?;
    ensure(verify.results.len() == 4, "one result per claim")?;
    ensure(
        verify
            .results
            .iter()
            .all(|result| result.evidence_checked == 1 && result.evidence_passed == 1),
        "each claim checked one evidence entry",
    )
}

#[test]
fn gate14_core_claim_verify_reports_file_hash_tampering() -> TestResult {
    let fixture = write_evidence_claim_fixture("core_evidence_tamper", "2999-01-01T00:00:00Z")?;
    fs::write(
        fixture.workspace.join("evidence").join("payload.txt"),
        b"tampered\n",
    )
    .map_err(|error| format!("failed to tamper evidence payload: {error}"))?;

    let verify = build_claim_verify_report(&ClaimVerifyOptions {
        workspace_path: fixture.workspace,
        claim_id: "all".to_string(),
        fail_fast: false,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    ensure(verify.verified_count == 3, "untampered claims still pass")?;
    ensure(verify.failed_count == 1, "tampered file hash fails")?;
    ensure(
        verify
            .results
            .iter()
            .flat_map(|result| result.errors.iter())
            .any(|error| error.contains("hash_mismatch")),
        "hash mismatch is reported",
    )
}

#[test]
fn gate14_core_claim_verify_reports_expired_claim_ttl() -> TestResult {
    let fixture = write_evidence_claim_fixture("core_evidence_expired", "2000-01-01T00:00:00Z")?;

    let verify = build_claim_verify_report(&ClaimVerifyOptions {
        workspace_path: fixture.workspace,
        claim_id: "all".to_string(),
        fail_fast: true,
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    ensure(verify.verified_count == 0, "expired claim does not verify")?;
    ensure(verify.failed_count == 1, "expired claim fails")?;
    let result = verify
        .results
        .first()
        .ok_or_else(|| "expired claim result should exist".to_string())?;
    ensure(
        result.status == ManifestVerificationStatus::Expired,
        "expired status is reported",
    )?;
    ensure(
        result
            .errors
            .iter()
            .any(|error| error.contains("claim_expired")),
        "expired claim reason is reported",
    )
}

#[test]
fn gate14_cli_claim_commands_parse_and_verify_real_files() -> TestResult {
    let fixture = write_real_claim_fixture("cli_real_manifest", None, true)?;
    let workspace = fixture.workspace.display().to_string();

    let list = run_ee(&[
        "--workspace".to_string(),
        workspace.clone(),
        "--json".to_string(),
        "claim".to_string(),
        "list".to_string(),
        "--status".to_string(),
        "active".to_string(),
    ])?;
    ensure(list.status.code() == Some(0), "claim list exit")?;
    ensure(list.stderr.is_empty(), "claim list JSON stderr")?;
    let list_json = parse_stdout_json(&list)?;
    ensure(
        list_json["schema"] == json!("ee.claim_list.v1"),
        "claim list schema",
    )?;
    ensure(list_json["success"] == json!(true), "claim list success")?;
    ensure(
        list_json["data"]["claims"][0]["id"] == json!(fixture.claim_id.as_str()),
        "claim list id",
    )?;

    let show = run_ee(&[
        "--workspace".to_string(),
        workspace.clone(),
        "--json".to_string(),
        "claim".to_string(),
        "show".to_string(),
        fixture.claim_id.clone(),
        "--include-manifest".to_string(),
    ])?;
    ensure(show.status.code() == Some(0), "claim show exit")?;
    ensure(show.stderr.is_empty(), "claim show JSON stderr")?;
    let show_json = parse_stdout_json(&show)?;
    ensure(show_json["success"] == json!(true), "claim show success")?;
    ensure(
        show_json["data"]["manifest"]["artifactCount"] == json!(1),
        "claim show manifest artifact count",
    )?;

    let verify = run_ee(&[
        "--workspace".to_string(),
        workspace,
        "--json".to_string(),
        "claim".to_string(),
        "verify".to_string(),
        fixture.claim_id,
    ])?;
    ensure(verify.status.code() == Some(0), "claim verify exit")?;
    ensure(verify.stderr.is_empty(), "claim verify JSON stderr")?;
    let verify_json = parse_stdout_json(&verify)?;
    ensure(
        verify_json["schema"] == json!("ee.claim_verify.v1"),
        "claim verify schema",
    )?;
    ensure(
        verify_json["success"] == json!(true),
        "claim verify success",
    )?;
    ensure(
        verify_json["data"]["results"][0]["status"] == json!("passing"),
        "claim verify manifest status",
    )
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
#[cfg(unix)]
fn gate14_core_claim_verify_rejects_symlink_artifact_evidence() -> TestResult {
    let fixture = write_real_claim_fixture("core_symlink_artifact", None, false)?;
    let claim_artifacts_dir = fixture.workspace.join("artifacts").join(&fixture.claim_id);
    let outside = unique_claim_workspace("core_symlink_artifact_outside")?;
    let outside_payload = b"{\"ok\":true,\"source\":\"outside\"}\n";
    let outside_payload_path = outside.join("outside.json");
    fs::write(&outside_payload_path, outside_payload)
        .map_err(|error| format!("failed to write outside artifact payload: {error}"))?;
    std::os::unix::fs::symlink(
        &outside_payload_path,
        claim_artifacts_dir.join("linked.json"),
    )
    .map_err(|error| format!("failed to create artifact symlink: {error}"))?;

    let manifest = serde_json::json!({
        "schema": "ee.claim_manifest.v1",
        "claimId": fixture.claim_id,
        "verificationStatus": "passing",
        "lastVerifiedAt": "2026-01-02T03:04:05Z",
        "artifacts": [
            {
                "path": "linked.json",
                "artifactType": "report",
                "blake3Hash": blake3::hash(outside_payload).to_hex().to_string(),
                "sizeBytes": outside_payload.len(),
                "createdAt": "2026-01-02T03:04:05Z"
            }
        ]
    });
    fs::write(
        claim_artifacts_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("failed to write manifest.json: {error}"))?;

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
        "symlink artifact is failing",
    )?;
    ensure(result.artifacts_checked == 1, "artifact was checked")?;
    ensure(
        result.artifacts_passed == 0,
        "symlink artifact is not evidence",
    )?;
    ensure(
        result.artifacts_failed == 1,
        "symlink artifact failed verification",
    )?;
    ensure(
        result
            .errors
            .iter()
            .any(|error| error.contains("artifact_symlink_refused")),
        "symlink artifact refusal is reported",
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
