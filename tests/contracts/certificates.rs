//! Gate 13 certificate contract coverage.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use ee::core::certificate::{
    CERTIFICATE_MANIFEST_SCHEMA_V1, CERTIFICATE_PAYLOAD_SCHEMA_V1, CertificateListOptions,
    CertificateLookupOptions, CertificateVerifyReport, VerificationResult, list_certificates,
    show_certificate, show_certificate_with_options, verify_certificate,
    verify_certificate_with_options,
};
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

fn parse_stdout_json(output: &std::process::Output) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "stdout should be parseable JSON: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn hash_payload(payload: &str) -> String {
    blake3::hash(payload.as_bytes()).to_hex().to_string()
}

fn certificate_record(
    id: &str,
    status: &str,
    payload_path: &str,
    payload_hash: &str,
    payload_schema: &str,
    expires_at: Option<&str>,
    assumption_valid: bool,
) -> Value {
    let mut record = json!({
        "id": id,
        "kind": "pack",
        "status": status,
        "workspaceId": "workspace_main",
        "issuedAt": "2026-05-01T00:00:00Z",
        "payloadPath": payload_path,
        "payloadHash": payload_hash,
        "payloadSchema": payload_schema,
        "assumptions": [
            {
                "valid": assumption_valid
            }
        ]
    });
    if let Some(expires_at) = expires_at {
        record["expiresAt"] = json!(expires_at);
    }
    record
}

fn write_certificate_manifest_fixture() -> Result<(tempfile::TempDir, PathBuf), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let valid_payload = r#"{"packHash":"pack_valid","selected":["mem_01"]}"#;
    let changed_payload = r#"{"packHash":"pack_changed","selected":["mem_02"]}"#;
    let valid_hash = hash_payload(valid_payload);
    let changed_hash = hash_payload(changed_payload);

    fs::write(dir.path().join("valid-payload.json"), valid_payload)
        .map_err(|error| error.to_string())?;
    fs::write(dir.path().join("changed-payload.json"), changed_payload)
        .map_err(|error| error.to_string())?;

    let manifest = json!({
        "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
        "certificates": [
            certificate_record(
                "cert_pack_valid",
                "valid",
                "valid-payload.json",
                &valid_hash,
                CERTIFICATE_PAYLOAD_SCHEMA_V1,
                Some("2999-01-01T00:00:00Z"),
                true,
            ),
            certificate_record(
                "cert_pack_hash_mismatch",
                "valid",
                "changed-payload.json",
                &valid_hash,
                CERTIFICATE_PAYLOAD_SCHEMA_V1,
                Some("2999-01-01T00:00:00Z"),
                true,
            ),
            certificate_record(
                "cert_pack_stale_schema",
                "valid",
                "valid-payload.json",
                &valid_hash,
                "ee.certificate.payload.v0",
                Some("2999-01-01T00:00:00Z"),
                true,
            ),
            certificate_record(
                "cert_pack_expired",
                "expired",
                "valid-payload.json",
                &valid_hash,
                CERTIFICATE_PAYLOAD_SCHEMA_V1,
                Some("2000-01-01T00:00:00Z"),
                true,
            ),
            certificate_record(
                "cert_pack_revoked",
                "revoked",
                "valid-payload.json",
                &valid_hash,
                CERTIFICATE_PAYLOAD_SCHEMA_V1,
                Some("2999-01-01T00:00:00Z"),
                true,
            ),
            certificate_record(
                "cert_pack_failed_assumptions",
                "valid",
                "valid-payload.json",
                &changed_hash,
                CERTIFICATE_PAYLOAD_SCHEMA_V1,
                Some("2999-01-01T00:00:00Z"),
                false,
            )
        ]
    });
    let manifest_path = dir.path().join("certificates.json");
    let manifest_json =
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
    fs::write(&manifest_path, manifest_json).map_err(|error| error.to_string())?;

    Ok((dir, manifest_path))
}

#[test]
fn certificate_core_reports_legacy_mock_ids_as_unavailable() -> TestResult {
    let list = list_certificates(&CertificateListOptions::new().include_expired());
    ensure(
        list.certificates.is_empty(),
        "certificate list must not surface mock certificates",
    )?;
    ensure_equal(&list.total_count, &0, "certificate total count")?;
    ensure_equal(&list.usable_count, &0, "certificate usable count")?;
    ensure_equal(&list.expired_count, &0, "certificate expired count")?;
    ensure(
        list.kinds_present.is_empty(),
        "certificate kinds must be empty without a manifest store",
    )?;

    let shown = show_certificate("cert_pack_001");
    ensure_equal(
        &shown.verification_status,
        &VerificationResult::NotFound,
        "legacy certificate show status",
    )?;

    let stale_payload = verify_certificate("cert_pack_stale_payload");
    ensure_equal(
        &stale_payload.result,
        &VerificationResult::NotFound,
        "legacy stale payload id is not found",
    )?;
    ensure(
        stale_payload
            .failure_codes
            .iter()
            .any(|code| code == "not_found"),
        "not-found failure code",
    )?;

    let stale_schema = verify_certificate("cert_pack_stale_schema");
    ensure_equal(
        &stale_schema.result,
        &VerificationResult::NotFound,
        "legacy stale schema id is not found",
    )?;

    let failed_assumptions = verify_certificate("cert_pack_failed_assumptions");
    ensure_equal(
        &failed_assumptions.result,
        &VerificationResult::NotFound,
        "legacy failed-assumptions id is not found",
    )
}

#[test]
fn certificate_core_reads_file_backed_manifest_records() -> TestResult {
    let (_dir, manifest_path) = write_certificate_manifest_fixture()?;
    let list = list_certificates(
        &CertificateListOptions::new()
            .with_manifest_path(&manifest_path)
            .include_expired(),
    );
    ensure_equal(&list.total_count, &6, "certificate total count")?;
    ensure_equal(&list.usable_count, &4, "certificate usable count")?;
    ensure_equal(&list.expired_count, &1, "certificate expired count")?;
    ensure_equal(
        &list.kinds_present,
        &vec![ee::models::CertificateKind::Pack],
        "certificate kinds",
    )?;

    let filtered = list_certificates(
        &CertificateListOptions::new()
            .with_manifest_path(&manifest_path)
            .with_status(ee::models::CertificateStatus::Valid)
            .with_limit(2),
    );
    ensure_equal(
        &filtered.certificates.len(),
        &2usize,
        "filtered certificate count",
    )?;
    ensure_equal(
        &filtered.certificates[0].id,
        &"cert_pack_failed_assumptions".to_string(),
        "stable ordering",
    )?;

    let shown = show_certificate_with_options(
        &CertificateLookupOptions::new("cert_pack_valid").with_manifest_path(&manifest_path),
    );
    ensure_equal(
        &shown.verification_status,
        &VerificationResult::Valid,
        "manifest-backed show verification",
    )?;
    ensure_equal(
        &shown.certificate.payload_hash,
        &hash_payload(r#"{"packHash":"pack_valid","selected":["mem_01"]}"#),
        "manifest-backed payload hash",
    )?;

    let verified = verify_certificate_with_options(
        &CertificateLookupOptions::new("cert_pack_valid").with_manifest_path(&manifest_path),
    );
    ensure_equal(
        &verified.result,
        &VerificationResult::Valid,
        "manifest-backed verify result",
    )?;
    ensure(verified.hash_verified, "hash verified")?;
    ensure(verified.payload_hash_fresh, "payload hash fresh")
}

#[test]
fn certificate_verify_manifest_reports_explicit_failure_modes() -> TestResult {
    let (_dir, manifest_path) = write_certificate_manifest_fixture()?;
    let cases = [
        (
            "cert_pack_hash_mismatch",
            VerificationResult::HashMismatch,
            "hash_mismatch",
        ),
        (
            "cert_pack_stale_schema",
            VerificationResult::StaleSchemaVersion,
            "stale_schema_version",
        ),
        ("cert_pack_expired", VerificationResult::Expired, "expired"),
        ("cert_pack_revoked", VerificationResult::Revoked, "revoked"),
        (
            "cert_pack_failed_assumptions",
            VerificationResult::FailedAssumptions,
            "failed_assumptions",
        ),
    ];

    for (certificate_id, expected_result, expected_code) in cases {
        let report = verify_certificate_with_options(
            &CertificateLookupOptions::new(certificate_id).with_manifest_path(&manifest_path),
        );
        ensure_equal(
            &report.result,
            &expected_result,
            &format!("{certificate_id} result"),
        )?;
        ensure(
            report
                .failure_codes
                .iter()
                .any(|code| code == expected_code),
            format!("{certificate_id} failure code"),
        )?;
    }

    Ok(())
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
fn certificate_cli_reads_explicit_manifest_records() -> TestResult {
    let (_dir, manifest_path) = write_certificate_manifest_fixture()?;
    let manifest = manifest_path.display().to_string();

    let list = run_ee(&[
        "--json",
        "certificate",
        "list",
        "--manifest",
        &manifest,
        "--kind",
        "pack",
    ])?;
    ensure_equal(&list.status.code(), &Some(0), "list exit")?;
    ensure(list.stderr.is_empty(), "json certificate list stderr")?;
    let list_json = parse_stdout_json(&list)?;
    ensure_equal(
        &list_json["schema"],
        &json!("ee.certificate.list.v1"),
        "list schema",
    )?;
    ensure_equal(&list_json["success"], &json!(true), "list success")?;
    ensure_equal(
        &list_json["data"]["totalCount"],
        &json!(6),
        "manifest certificate count",
    )?;
    ensure_equal(
        &list_json["data"]["certificates"][0]["id"],
        &json!("cert_pack_failed_assumptions"),
        "deterministic list ordering",
    )?;

    let show = run_ee(&[
        "--json",
        "certificate",
        "show",
        "cert_pack_valid",
        "--manifest",
        &manifest,
    ])?;
    ensure_equal(&show.status.code(), &Some(0), "show exit")?;
    ensure(show.stderr.is_empty(), "json certificate show stderr")?;
    let show_json = parse_stdout_json(&show)?;
    ensure_equal(
        &show_json["data"]["certificate"]["payloadHash"],
        &json!(hash_payload(
            r#"{"packHash":"pack_valid","selected":["mem_01"]}"#
        )),
        "show payload hash",
    )?;

    let verify = run_ee(&[
        "--json",
        "certificate",
        "verify",
        "cert_pack_valid",
        "--manifest",
        &manifest,
    ])?;
    ensure_equal(&verify.status.code(), &Some(0), "verify exit")?;
    ensure(verify.stderr.is_empty(), "json certificate verify stderr")?;
    let verify_json = parse_stdout_json(&verify)?;
    ensure_equal(
        &verify_json["schema"],
        &json!("ee.certificate.verify.v1"),
        "verify schema",
    )?;
    ensure_equal(&verify_json["success"], &json!(true), "verify success")?;
    ensure_equal(
        &verify_json["data"]["result"],
        &json!("valid"),
        "manifest-backed verify result",
    )?;
    ensure_equal(
        &verify_json["data"]["hashVerified"],
        &json!(true),
        "manifest-backed hash check",
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
    let report = CertificateVerifyReport::failed_assumptions("cert_fixture_failed_assumptions");
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
