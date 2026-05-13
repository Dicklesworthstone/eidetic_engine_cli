//! Verification evidence records for build/test/provenance gates.
//!
//! The verification ledger models what happened during a gate without treating
//! "a command ran" as proof. In particular, remote-required Cargo gates that
//! fall back to local execution are explicit non-passing evidence.

use serde::{Deserialize, Serialize};

use crate::models::{ProducerMetadata, ProducerSourceSystem};

pub const VERIFICATION_EVIDENCE_SCHEMA_V1: &str = "ee.verification.evidence.v1";
pub const VERIFICATION_CLOSURE_GUIDANCE_SCHEMA_V1: &str = "ee.verification.closure_guidance.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Blocked,
    Interrupted,
    FallbackDetected,
    Unknown,
}

impl VerificationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Interrupted => "interrupted",
            Self::FallbackDetected => "fallback_detected",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationOffload {
    pub required_remote: bool,
    pub remote_required_env: Option<String>,
    pub offload_tool: Option<String>,
    pub worker: Option<String>,
    pub fallback_detected: bool,
    pub fallback_reason: Option<String>,
}

impl VerificationOffload {
    #[must_use]
    pub fn local() -> Self {
        Self {
            required_remote: false,
            remote_required_env: None,
            offload_tool: None,
            worker: None,
            fallback_detected: false,
            fallback_reason: None,
        }
    }

    #[must_use]
    pub fn rch_required(worker: Option<&str>) -> Self {
        Self {
            required_remote: true,
            remote_required_env: Some("RCH_REQUIRE_REMOTE=1".to_owned()),
            offload_tool: Some("rch".to_owned()),
            worker: normalized_non_empty(worker),
            fallback_detected: false,
            fallback_reason: None,
        }
    }

    #[must_use]
    pub fn rch_fallback(worker: Option<&str>, reason: Option<&str>) -> Self {
        Self {
            required_remote: true,
            remote_required_env: Some("RCH_REQUIRE_REMOTE=1".to_owned()),
            offload_tool: Some("rch".to_owned()),
            worker: normalized_non_empty(worker),
            fallback_detected: true,
            fallback_reason: normalized_non_empty(reason),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationEnvironment {
    pub workspace_fingerprint: Option<String>,
    pub cwd: Option<String>,
    pub toolchain: Option<String>,
}

impl VerificationEnvironment {
    #[must_use]
    pub fn new(
        workspace_fingerprint: Option<&str>,
        cwd: Option<&str>,
        toolchain: Option<&str>,
    ) -> Self {
        Self {
            workspace_fingerprint: normalized_non_empty(workspace_fingerprint),
            cwd: normalized_non_empty(cwd),
            toolchain: normalized_non_empty(toolchain),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationOutputSummary {
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub redacted: bool,
}

impl VerificationOutputSummary {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            stdout_tail: None,
            stderr_tail: None,
            redacted: false,
        }
    }

    #[must_use]
    pub fn redacted(stderr_tail: Option<&str>) -> Self {
        Self {
            stdout_tail: None,
            stderr_tail: normalized_non_empty(stderr_tail),
            redacted: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationArtifactRef {
    pub path: String,
    pub kind: String,
    pub content_hash: Option<String>,
}

impl VerificationArtifactRef {
    #[must_use]
    pub fn new(path: &str, kind: &str, content_hash: Option<&str>) -> Self {
        Self {
            path: path.to_owned(),
            kind: kind.to_owned(),
            content_hash: normalized_non_empty(content_hash),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationEvidenceRecord {
    pub schema: String,
    pub verification_id: String,
    pub bead_id: Option<String>,
    pub gate_name: String,
    pub command: String,
    pub command_hash: String,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub environment: VerificationEnvironment,
    pub offload: VerificationOffload,
    pub output_summary: VerificationOutputSummary,
    pub artifacts: Vec<VerificationArtifactRef>,
    pub producer: ProducerMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationGateRequirement {
    pub gate_name: String,
    pub command_contains: Option<String>,
    pub requires_remote: bool,
}

impl VerificationGateRequirement {
    #[must_use]
    pub fn new(gate_name: &str, command_contains: Option<&str>, requires_remote: bool) -> Self {
        Self {
            gate_name: gate_name.to_owned(),
            command_contains: normalized_non_empty(command_contains),
            requires_remote,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationGateAssessment {
    pub gate_name: String,
    pub command_contains: Option<String>,
    pub requires_remote: bool,
    pub satisfied: bool,
    pub matched_verification_id: Option<String>,
    pub matched_status: Option<VerificationStatus>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationClosureGuidance {
    pub schema: String,
    pub bead_id: Option<String>,
    pub can_close: bool,
    pub assessments: Vec<VerificationGateAssessment>,
    pub rejected_reasons: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct VerificationEvidenceInput<'a> {
    pub verification_id: &'a str,
    pub bead_id: Option<&'a str>,
    pub gate_name: &'a str,
    pub command: &'a str,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    pub started_at: Option<&'a str>,
    pub finished_at: Option<&'a str>,
    pub duration_ms: Option<u64>,
    pub environment: VerificationEnvironment,
    pub offload: VerificationOffload,
    pub output_summary: VerificationOutputSummary,
    pub artifacts: Vec<VerificationArtifactRef>,
    pub producer: ProducerMetadata,
}

impl VerificationEvidenceRecord {
    #[must_use]
    pub fn from_input(input: VerificationEvidenceInput<'_>) -> Self {
        Self {
            schema: VERIFICATION_EVIDENCE_SCHEMA_V1.to_owned(),
            verification_id: input.verification_id.to_owned(),
            bead_id: normalized_non_empty(input.bead_id),
            gate_name: input.gate_name.to_owned(),
            command: input.command.to_owned(),
            command_hash: command_hash(input.command),
            status: input.status,
            exit_code: input.exit_code,
            started_at: normalized_non_empty(input.started_at),
            finished_at: normalized_non_empty(input.finished_at),
            duration_ms: input.duration_ms,
            environment: input.environment,
            offload: input.offload,
            output_summary: input.output_summary,
            artifacts: input.artifacts,
            producer: input.producer,
        }
    }

    #[must_use]
    pub fn is_authoritative_pass(&self) -> bool {
        self.status == VerificationStatus::Passed
            && self.exit_code == Some(0)
            && !(self.offload.required_remote && self.offload.fallback_detected)
    }
}

#[must_use]
pub fn command_hash(command: &str) -> String {
    format!("blake3:{}", blake3::hash(command.as_bytes()).to_hex())
}

#[must_use]
pub fn sample_verification_evidence_records() -> Vec<VerificationEvidenceRecord> {
    let env = VerificationEnvironment::new(
        Some("repo:25e38e130474e7f0292de2a3"),
        Some("/repo"),
        Some("rustc 1.96.0-nightly"),
    );
    let producer = ProducerMetadata::unknown_agent(
        ProducerSourceSystem::Verification,
        Some("verify-run-20260513"),
        None,
        Some("repo:25e38e130474e7f0292de2a3"),
        Some("2026-05-13T00:00:00Z"),
    );

    vec![
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_pass_00000000000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo fmt",
            command: "cargo fmt --check",
            status: VerificationStatus::Passed,
            exit_code: Some(0),
            started_at: Some("2026-05-13T00:00:00Z"),
            finished_at: Some("2026-05-13T00:00:01Z"),
            duration_ms: Some(1000),
            environment: env.clone(),
            offload: VerificationOffload::local(),
            output_summary: VerificationOutputSummary::empty(),
            artifacts: Vec::new(),
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_fail_00000000000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo clippy",
            command: "cargo clippy --all-targets -- -D warnings",
            status: VerificationStatus::Failed,
            exit_code: Some(101),
            started_at: Some("2026-05-13T00:01:00Z"),
            finished_at: Some("2026-05-13T00:01:20Z"),
            duration_ms: Some(20000),
            environment: env.clone(),
            offload: VerificationOffload::rch_required(Some("css")),
            output_summary: VerificationOutputSummary::redacted(Some(
                "error: could not compile `ee` due to warnings",
            )),
            artifacts: vec![VerificationArtifactRef::new(
                "target/verify/clippy.log",
                "log",
                Some("blake3:clippylog"),
            )],
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_blocked_0000000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo test",
            command: "RCH_REQUIRE_REMOTE=1 rch exec -- cargo test",
            status: VerificationStatus::Blocked,
            exit_code: None,
            started_at: Some("2026-05-13T00:02:00Z"),
            finished_at: Some("2026-05-13T00:02:02Z"),
            duration_ms: Some(2000),
            environment: env.clone(),
            offload: VerificationOffload::rch_required(None),
            output_summary: VerificationOutputSummary::redacted(Some(
                "RCH worker unavailable; command did not run",
            )),
            artifacts: Vec::new(),
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_interrupted_000000000001",
            bead_id: Some("bd-example"),
            gate_name: "verify script",
            command: "./scripts/verify.sh",
            status: VerificationStatus::Interrupted,
            exit_code: None,
            started_at: Some("2026-05-13T00:03:00Z"),
            finished_at: None,
            duration_ms: None,
            environment: env.clone(),
            offload: VerificationOffload::local(),
            output_summary: VerificationOutputSummary::redacted(Some(
                "verification interrupted before a final exit code",
            )),
            artifacts: Vec::new(),
            producer: producer.clone(),
        }),
        VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_fallback_0000000000001",
            bead_id: Some("bd-example"),
            gate_name: "cargo test producer",
            command: "rch exec -- cargo test --lib producer",
            status: VerificationStatus::FallbackDetected,
            exit_code: Some(0),
            started_at: Some("2026-05-13T00:04:00Z"),
            finished_at: Some("2026-05-13T00:04:45Z"),
            duration_ms: Some(45000),
            environment: env,
            offload: VerificationOffload::rch_fallback(
                Some("css"),
                Some("project path normalized outside canonical remote root"),
            ),
            output_summary: VerificationOutputSummary::redacted(Some(
                "RCH fell back to local execution; remote-required gate is not verified",
            )),
            artifacts: Vec::new(),
            producer,
        }),
    ]
}

#[must_use]
pub fn rch_cargo_closure_requirements() -> Vec<VerificationGateRequirement> {
    vec![
        VerificationGateRequirement::new("cargo fmt", Some("cargo fmt --check"), true),
        VerificationGateRequirement::new(
            "cargo clippy",
            Some("cargo clippy --all-targets -- -D warnings"),
            true,
        ),
        VerificationGateRequirement::new("cargo test", Some("cargo test"), true),
        VerificationGateRequirement::new("forbidden deps", Some("forbidden_deps"), true),
    ]
}

#[must_use]
pub fn verification_closure_guidance(
    bead_id: Option<&str>,
    requirements: &[VerificationGateRequirement],
    records: &[VerificationEvidenceRecord],
) -> VerificationClosureGuidance {
    let assessments = requirements
        .iter()
        .map(|requirement| assess_requirement(requirement, records))
        .collect::<Vec<_>>();
    let rejected_reasons = assessments
        .iter()
        .filter(|assessment| !assessment.satisfied)
        .map(|assessment| format!("{}: {}", assessment.gate_name, assessment.reason))
        .collect::<Vec<_>>();

    VerificationClosureGuidance {
        schema: VERIFICATION_CLOSURE_GUIDANCE_SCHEMA_V1.to_owned(),
        bead_id: normalized_non_empty(bead_id),
        can_close: rejected_reasons.is_empty(),
        assessments,
        rejected_reasons,
    }
}

fn normalized_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_owned)
}

fn assess_requirement(
    requirement: &VerificationGateRequirement,
    records: &[VerificationEvidenceRecord],
) -> VerificationGateAssessment {
    let matched = records
        .iter()
        .rev()
        .find(|record| requirement_matches(requirement, record));

    let Some(record) = matched else {
        return VerificationGateAssessment {
            gate_name: requirement.gate_name.clone(),
            command_contains: requirement.command_contains.clone(),
            requires_remote: requirement.requires_remote,
            satisfied: false,
            matched_verification_id: None,
            matched_status: None,
            reason: "no matching verification evidence recorded".to_owned(),
        };
    };

    let satisfied = record_satisfies_requirement(requirement, record);
    VerificationGateAssessment {
        gate_name: requirement.gate_name.clone(),
        command_contains: requirement.command_contains.clone(),
        requires_remote: requirement.requires_remote,
        satisfied,
        matched_verification_id: Some(record.verification_id.clone()),
        matched_status: Some(record.status),
        reason: if satisfied {
            "authoritative pass evidence recorded".to_owned()
        } else {
            rejection_reason(requirement, record)
        },
    }
}

fn requirement_matches(
    requirement: &VerificationGateRequirement,
    record: &VerificationEvidenceRecord,
) -> bool {
    if record.gate_name == requirement.gate_name {
        return true;
    }

    requirement
        .command_contains
        .as_ref()
        .is_some_and(|fragment| record.command.contains(fragment))
}

fn record_satisfies_requirement(
    requirement: &VerificationGateRequirement,
    record: &VerificationEvidenceRecord,
) -> bool {
    record.is_authoritative_pass()
        && (!requirement.requires_remote
            || (record.offload.required_remote && !record.offload.fallback_detected))
}

fn rejection_reason(
    requirement: &VerificationGateRequirement,
    record: &VerificationEvidenceRecord,
) -> String {
    if record.offload.fallback_detected || record.status == VerificationStatus::FallbackDetected {
        return "matching evidence detected local fallback; remote-required gate is unverified"
            .to_owned();
    }
    if record.status == VerificationStatus::Blocked {
        return "matching evidence is blocked".to_owned();
    }
    if record.status == VerificationStatus::Interrupted {
        return "matching evidence was interrupted before completion".to_owned();
    }
    if record.status == VerificationStatus::Failed {
        return format!(
            "matching evidence failed with exitCode={}",
            record
                .exit_code
                .map_or_else(|| "null".to_owned(), |code| code.to_string())
        );
    }
    if record.status == VerificationStatus::Passed && record.exit_code != Some(0) {
        return "matching pass evidence lacks exitCode=0".to_owned();
    }
    if requirement.requires_remote && !record.offload.required_remote {
        return "matching pass evidence was local but this gate requires remote evidence"
            .to_owned();
    }

    format!("matching evidence has status={}", record.status.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const VERIFICATION_EVIDENCE_GOLDEN: &str = include_str!(
        "../../tests/fixtures/golden/models/verification_evidence_records.json.golden"
    );

    #[test]
    fn verification_evidence_records_match_golden_fixture() -> TestResult {
        let json = serde_json::to_string(&sample_verification_evidence_records())?;
        assert_eq!(json, VERIFICATION_EVIDENCE_GOLDEN.trim_end_matches('\n'));
        Ok(())
    }

    #[test]
    fn fallback_detected_is_never_an_authoritative_pass() -> TestResult {
        let records = sample_verification_evidence_records();
        let fallback = records
            .iter()
            .find(|record| record.status == VerificationStatus::FallbackDetected)
            .ok_or_else(|| std::io::Error::other("sample records include fallback_detected"))?;

        assert!(!fallback.is_authoritative_pass());
        assert!(fallback.offload.required_remote);
        assert!(fallback.offload.fallback_detected);
        assert_eq!(fallback.exit_code, Some(0));
        Ok(())
    }

    #[test]
    fn remote_required_blocked_record_has_no_exit_code() -> TestResult {
        let records = sample_verification_evidence_records();
        let blocked = records
            .iter()
            .find(|record| record.status == VerificationStatus::Blocked)
            .ok_or_else(|| std::io::Error::other("sample records include blocked"))?;

        assert!(!blocked.is_authoritative_pass());
        assert!(blocked.offload.required_remote);
        assert_eq!(blocked.exit_code, None);
        Ok(())
    }

    #[test]
    fn closure_guidance_rejects_fallback_cargo_evidence() {
        let records = sample_verification_evidence_records();
        let requirements = vec![VerificationGateRequirement::new(
            "cargo test producer",
            Some("cargo test --lib producer"),
            true,
        )];

        let guidance = verification_closure_guidance(Some("bd-example"), &requirements, &records);

        assert!(!guidance.can_close);
        assert!(!guidance.assessments[0].satisfied);
        assert_eq!(
            guidance.assessments[0].matched_status,
            Some(VerificationStatus::FallbackDetected)
        );
        assert_eq!(
            guidance.rejected_reasons,
            vec![
                "cargo test producer: matching evidence detected local fallback; remote-required gate is unverified"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn closure_guidance_accepts_authoritative_remote_pass() {
        let producer = ProducerMetadata::unknown_agent(
            ProducerSourceSystem::Verification,
            Some("verify-run-pass"),
            None,
            Some("repo:abc"),
            Some("2026-05-13T01:00:00Z"),
        );
        let record = VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_remote_pass",
            bead_id: Some("bd-example"),
            gate_name: "cargo test",
            command: "RCH_REQUIRE_REMOTE=1 rch exec -- cargo test",
            status: VerificationStatus::Passed,
            exit_code: Some(0),
            started_at: Some("2026-05-13T01:00:00Z"),
            finished_at: Some("2026-05-13T01:01:00Z"),
            duration_ms: Some(60_000),
            environment: VerificationEnvironment::new(Some("repo:abc"), Some("/repo"), None),
            offload: VerificationOffload::rch_required(Some("css")),
            output_summary: VerificationOutputSummary::empty(),
            artifacts: Vec::new(),
            producer,
        });
        let requirements = vec![VerificationGateRequirement::new(
            "cargo test",
            Some("cargo test"),
            true,
        )];

        let guidance = verification_closure_guidance(Some("bd-example"), &requirements, &[record]);

        assert!(guidance.can_close);
        assert!(guidance.rejected_reasons.is_empty());
        assert!(guidance.assessments[0].satisfied);
    }
}
