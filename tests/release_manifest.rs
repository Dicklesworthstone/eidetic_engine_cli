use std::fs;
use std::path::Path;

use ee::models::{
    ReleaseArtifact, ReleaseManifest, ReleaseVerificationCode, ReleaseVerificationReport,
    ReleaseVerificationStatus, is_allowed_package_member_path, release_artifact_file_name,
    verify_release_manifest_json,
};

type TestResult = Result<(), String>;

const MULTI_PLATFORM: &str = include_str!("fixtures/release_manifest/multi_platform.json");
const SINGLE_PLATFORM_DEV: &str =
    include_str!("fixtures/release_manifest/single_platform_dev.json");
const MISSING_SIGNATURE: &str = include_str!("fixtures/release_manifest/missing_signature.json");
const CHECKSUM_MISMATCH: &str = include_str!("fixtures/release_manifest/checksum_mismatch.json");
const UNSUPPORTED_TARGET: &str = include_str!("fixtures/release_manifest/unsupported_target.json");
const FUTURE_MANIFEST_VERSION: &str =
    include_str!("fixtures/release_manifest/future_manifest_version.json");

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: T,
    expected: T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn finding_codes(report: &ReleaseVerificationReport) -> Vec<ReleaseVerificationCode> {
    report.findings.iter().map(|finding| finding.code).collect()
}

fn parse_fixture(fixture: &str) -> Result<ReleaseManifest, String> {
    serde_json::from_str(fixture).map_err(|error| error.to_string())
}

#[test]
fn multi_platform_release_fixture_verifies_without_artifact_root() -> TestResult {
    let report = verify_release_manifest_json(MULTI_PLATFORM, None);

    ensure_equal(
        report.status,
        ReleaseVerificationStatus::Passed,
        "multi-platform status",
    )?;
    ensure_equal(report.artifacts_checked, 2, "artifacts checked")?;
    ensure(report.findings.is_empty(), "no findings for signed fixture")
}

#[test]
fn single_platform_dev_release_fixture_reports_signature_warning() -> TestResult {
    let report = verify_release_manifest_json(SINGLE_PLATFORM_DEV, None);
    let codes = finding_codes(&report);

    ensure_equal(
        report.status,
        ReleaseVerificationStatus::Warning,
        "dev status",
    )?;
    ensure(
        codes.contains(&ReleaseVerificationCode::SignatureMissing),
        "missing signature warning",
    )?;
    ensure_equal(report.artifacts_failed, 0, "warning does not fail artifact")
}

#[test]
fn missing_signature_fixture_is_warning_not_checksum_failure() -> TestResult {
    let report = verify_release_manifest_json(MISSING_SIGNATURE, None);
    let codes = finding_codes(&report);

    ensure_equal(
        report.status,
        ReleaseVerificationStatus::Warning,
        "missing signature status",
    )?;
    ensure(
        codes.contains(&ReleaseVerificationCode::SignatureMissing),
        "signature warning present",
    )?;
    ensure(
        !codes.contains(&ReleaseVerificationCode::ChecksumMismatch),
        "no checksum check without artifact root",
    )
}

#[test]
fn checksum_mismatch_fixture_detects_bad_archive_bytes() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let manifest = parse_fixture(CHECKSUM_MISMATCH)?;
    let artifact = manifest
        .artifacts
        .first()
        .ok_or_else(|| "checksum fixture has no artifact".to_owned())?;
    fs::write(
        temp.path().join(&artifact.file_name),
        b"not the manifest checksum",
    )
    .map_err(|error| error.to_string())?;

    let report = verify_release_manifest_json(CHECKSUM_MISMATCH, Some(temp.path()));
    let codes = finding_codes(&report);

    ensure_equal(
        report.status,
        ReleaseVerificationStatus::Failed,
        "checksum mismatch status",
    )?;
    ensure(
        codes.contains(&ReleaseVerificationCode::ChecksumMismatch),
        "checksum mismatch detected",
    )
}

#[test]
fn malformed_checksum_is_reported_before_file_verification() -> TestResult {
    let mut manifest = parse_fixture(CHECKSUM_MISMATCH)?;
    let artifact = manifest
        .artifacts
        .first_mut()
        .ok_or_else(|| "checksum fixture has no artifact".to_owned())?;
    artifact.checksum.value =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned();

    let report = manifest.verify(Some(Path::new("/")));
    let codes = finding_codes(&report);

    ensure(
        codes.contains(&ReleaseVerificationCode::InvalidChecksum),
        "uppercase checksum is invalid",
    )?;
    ensure(
        !codes.contains(&ReleaseVerificationCode::MissingArtifact),
        "invalid checksum skips file verification",
    )
}

#[test]
fn unsafe_artifact_path_is_reported_without_file_io() -> TestResult {
    let mut manifest = parse_fixture(CHECKSUM_MISMATCH)?;
    let artifact = manifest
        .artifacts
        .first_mut()
        .ok_or_else(|| "checksum fixture has no artifact".to_owned())?;
    artifact.file_name = "../escape.tar.xz".to_owned();

    let report = manifest.verify(Some(Path::new("/")));
    let codes = finding_codes(&report);

    ensure(
        codes.contains(&ReleaseVerificationCode::UnsafeArtifactPath),
        "unsafe artifact path detected",
    )?;
    ensure(
        !codes.contains(&ReleaseVerificationCode::MissingArtifact),
        "unsafe artifact path skips file verification",
    )
}

#[test]
fn missing_artifact_is_detected_when_root_is_supplied() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let report = verify_release_manifest_json(MULTI_PLATFORM, Some(temp.path()));
    let codes = finding_codes(&report);

    ensure_equal(
        report.status,
        ReleaseVerificationStatus::Failed,
        "missing artifact status",
    )?;
    ensure(
        codes.contains(&ReleaseVerificationCode::MissingArtifact),
        "missing artifact detected",
    )?;
    ensure_equal(report.artifacts_failed, 2, "all missing artifacts fail")
}

#[test]
fn unsupported_target_fixture_reports_target_error() -> TestResult {
    let report = verify_release_manifest_json(UNSUPPORTED_TARGET, None);
    let codes = finding_codes(&report);

    ensure_equal(
        report.status,
        ReleaseVerificationStatus::Failed,
        "unsupported target status",
    )?;
    ensure(
        codes.contains(&ReleaseVerificationCode::UnsupportedTarget),
        "unsupported target detected",
    )
}

#[test]
fn future_manifest_version_is_detected_before_typed_parse() -> TestResult {
    let report = verify_release_manifest_json(FUTURE_MANIFEST_VERSION, None);
    let codes = finding_codes(&report);

    ensure_equal(
        report.status,
        ReleaseVerificationStatus::Failed,
        "future manifest status",
    )?;
    ensure(
        codes.contains(&ReleaseVerificationCode::UnsupportedFutureManifestVersion),
        "future manifest version detected",
    )?;
    ensure_equal(report.artifacts_checked, 0, "future schema short-circuits")
}

#[test]
fn generated_manifest_orders_artifacts_stably() -> TestResult {
    let linux =
        ReleaseArtifact::from_bytes("0.1.0", "commit-a", "x86_64-unknown-linux-gnu", b"linux");
    let mac = ReleaseArtifact::from_bytes("0.1.0", "commit-a", "aarch64-apple-darwin", b"mac");
    let manifest = ReleaseManifest::new("0.1.0", "commit-a", vec![linux, mac]);
    let json = serde_json::to_string(&manifest).map_err(|error| error.to_string())?;
    let parsed: ReleaseManifest = serde_json::from_str(&json).map_err(|error| error.to_string())?;

    let first = parsed
        .artifacts
        .first()
        .ok_or_else(|| "missing first generated artifact".to_owned())?;
    let second = parsed
        .artifacts
        .get(1)
        .ok_or_else(|| "missing second generated artifact".to_owned())?;
    ensure_equal(
        first.target_triple.as_str(),
        "aarch64-apple-darwin",
        "first sorted target",
    )?;
    ensure_equal(
        second.target_triple.as_str(),
        "x86_64-unknown-linux-gnu",
        "second sorted target",
    )
}

#[test]
fn artifact_naming_and_package_exclusions_are_public_contract() -> TestResult {
    ensure_equal(
        release_artifact_file_name(
            "ee",
            "x86_64-pc-windows-msvc",
            ee::models::ReleaseArchiveFormat::Zip,
        ),
        "ee-x86_64-pc-windows-msvc.zip".to_owned(),
        "windows artifact name",
    )?;
    ensure(
        is_allowed_package_member_path("bin/ee"),
        "binary member allowed",
    )?;
    ensure(
        !is_allowed_package_member_path("target/release/ee"),
        "target cache path denied",
    )?;
    ensure(
        !is_allowed_package_member_path(".ee/config.toml"),
        "local config denied",
    )?;
    ensure(
        !is_allowed_package_member_path("..\\escape"),
        "windows parent escape denied",
    )
}
