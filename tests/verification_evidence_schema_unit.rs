#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use ee::models::{
    VERIFICATION_EVIDENCE_SCHEMA_V1, VerificationEvidenceRecord, VerificationStatus,
    sample_verification_evidence_records,
};

type TestResult = Result<(), String>;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("verification")
}

fn sample_for(status: VerificationStatus) -> Result<VerificationEvidenceRecord, String> {
    sample_verification_evidence_records()
        .into_iter()
        .find(|record| record.status == status)
        .ok_or_else(|| format!("sample record missing status={}", status.as_str()))
}

#[test]
fn sample_records_cover_named_statuses_and_round_trip() -> TestResult {
    for status in [
        VerificationStatus::Passed,
        VerificationStatus::Failed,
        VerificationStatus::Blocked,
        VerificationStatus::Interrupted,
        VerificationStatus::FallbackDetected,
    ] {
        let record = sample_for(status)?;
        if record.schema != VERIFICATION_EVIDENCE_SCHEMA_V1 {
            return Err(format!(
                "status {} uses schema {}",
                status.as_str(),
                record.schema
            ));
        }
        if record.command_hash.strip_prefix("blake3:").is_none() {
            return Err(format!(
                "status {} missing blake3 command hash: {}",
                status.as_str(),
                record.command_hash
            ));
        }
        let encoded = serde_json::to_string(&record)
            .map_err(|error| format!("serialize {}: {error}", status.as_str()))?;
        let decoded: VerificationEvidenceRecord = serde_json::from_str(&encoded)
            .map_err(|error| format!("deserialize {}: {error}", status.as_str()))?;
        if decoded != record {
            return Err(format!(
                "round trip mismatch for status {}",
                status.as_str()
            ));
        }
    }
    Ok(())
}

#[test]
fn per_status_golden_fixtures_match_samples() -> TestResult {
    for (status, file_name) in [
        (VerificationStatus::Passed, "passed.json.golden"),
        (VerificationStatus::Failed, "failed.json.golden"),
        (VerificationStatus::Blocked, "blocked.json.golden"),
        (VerificationStatus::Interrupted, "interrupted.json.golden"),
        (
            VerificationStatus::FallbackDetected,
            "fallback_detected.json.golden",
        ),
    ] {
        let path = golden_dir().join(file_name);
        let fixture = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let expected: VerificationEvidenceRecord = serde_json::from_str(&fixture)
            .map_err(|error| format!("parse {}: {error}", path.display()))?;
        let actual = sample_for(status)?;
        if expected != actual {
            return Err(format!(
                "{} does not match sample status {}",
                path.display(),
                status.as_str()
            ));
        }
    }
    Ok(())
}
