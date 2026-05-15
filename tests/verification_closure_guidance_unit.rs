#![forbid(unsafe_code)]

use ee::models::{
    VerificationEvidenceRecord, VerificationGateRequirement, VerificationOffload,
    VerificationStatus, command_hash, sample_verification_evidence_records,
    verification_closure_guidance,
};

type TestResult = Result<(), String>;

#[derive(Clone, Copy)]
struct ClosureCase {
    name: &'static str,
    status: VerificationStatus,
    remote_evidence: bool,
    fallback_detected: bool,
    expected_can_close: bool,
    expected_reason_fragment: Option<&'static str>,
}

fn cargo_test_record(case: ClosureCase) -> Result<VerificationEvidenceRecord, String> {
    let mut record = sample_verification_evidence_records()
        .into_iter()
        .find(|record| record.status == VerificationStatus::Passed)
        .ok_or_else(|| "sample pass record exists".to_owned())?;
    record.verification_id = format!("ver_case_{}", case.name);
    record.gate_name = "cargo test".to_owned();
    record.command = "RCH_REQUIRE_REMOTE=1 rch exec -- cargo test".to_owned();
    record.command_hash = command_hash(&record.command);
    record.status = case.status;
    record.exit_code = match case.status {
        VerificationStatus::Passed | VerificationStatus::FallbackDetected => Some(0),
        VerificationStatus::Failed => Some(101),
        VerificationStatus::Blocked
        | VerificationStatus::Interrupted
        | VerificationStatus::Unknown => None,
    };
    record.offload = if case.fallback_detected {
        VerificationOffload::rch_fallback(
            Some("css"),
            Some("project path normalized outside canonical remote root"),
        )
    } else if case.remote_evidence {
        VerificationOffload::rch_required(Some("css"))
    } else {
        VerificationOffload::local()
    };
    Ok(record)
}

#[test]
fn closure_guidance_table_drives_remote_required_cargo_evidence() -> TestResult {
    let requirement = VerificationGateRequirement::new("cargo test", Some("cargo test"), true);
    let cases = [
        ClosureCase {
            name: "remote_pass",
            status: VerificationStatus::Passed,
            remote_evidence: true,
            fallback_detected: false,
            expected_can_close: true,
            expected_reason_fragment: None,
        },
        ClosureCase {
            name: "local_pass",
            status: VerificationStatus::Passed,
            remote_evidence: false,
            fallback_detected: false,
            expected_can_close: false,
            expected_reason_fragment: Some("requires remote evidence"),
        },
        ClosureCase {
            name: "fallback",
            status: VerificationStatus::FallbackDetected,
            remote_evidence: true,
            fallback_detected: true,
            expected_can_close: false,
            expected_reason_fragment: Some("local fallback"),
        },
        ClosureCase {
            name: "failed",
            status: VerificationStatus::Failed,
            remote_evidence: true,
            fallback_detected: false,
            expected_can_close: false,
            expected_reason_fragment: Some("failed with exitCode=101"),
        },
        ClosureCase {
            name: "blocked",
            status: VerificationStatus::Blocked,
            remote_evidence: true,
            fallback_detected: false,
            expected_can_close: false,
            expected_reason_fragment: Some("blocked"),
        },
        ClosureCase {
            name: "interrupted",
            status: VerificationStatus::Interrupted,
            remote_evidence: true,
            fallback_detected: false,
            expected_can_close: false,
            expected_reason_fragment: Some("interrupted"),
        },
    ];

    for case in cases {
        let record = cargo_test_record(case)?;
        if !record.command_hash.starts_with("blake3:") {
            return Err(format!(
                "{} command hash lacks blake3 prefix: {}",
                case.name, record.command_hash
            ));
        }
        let guidance = verification_closure_guidance(
            Some("bd-example"),
            std::slice::from_ref(&requirement),
            &[record],
        );
        if guidance.can_close != case.expected_can_close {
            return Err(format!(
                "{} expected can_close={}, got {}",
                case.name, case.expected_can_close, guidance.can_close
            ));
        }
        if let Some(fragment) = case.expected_reason_fragment {
            let reason = guidance
                .rejected_reasons
                .first()
                .ok_or_else(|| format!("{} expected a rejection reason", case.name))?;
            if !reason.contains(fragment) {
                return Err(format!(
                    "{} expected reason containing {:?}, got {:?}",
                    case.name, fragment, reason
                ));
            }
        } else if !guidance.rejected_reasons.is_empty() {
            return Err(format!(
                "{} expected no rejection reasons, got {:?}",
                case.name, guidance.rejected_reasons
            ));
        }
    }
    Ok(())
}

#[test]
fn closure_guidance_rejects_topology_blocked_evidence_until_remote_pass_supersedes_it() -> TestResult
{
    let requirement = VerificationGateRequirement::new("cargo test", Some("cargo test"), true);
    let blocked = cargo_test_record(ClosureCase {
        name: "topology_blocked",
        status: VerificationStatus::Blocked,
        remote_evidence: true,
        fallback_detected: false,
        expected_can_close: false,
        expected_reason_fragment: Some("blocked"),
    })?;
    let pass = cargo_test_record(ClosureCase {
        name: "remote_pass_after_topology_fix",
        status: VerificationStatus::Passed,
        remote_evidence: true,
        fallback_detected: false,
        expected_can_close: true,
        expected_reason_fragment: None,
    })?;

    let blocked_guidance = verification_closure_guidance(
        Some("bd-rch-topology"),
        std::slice::from_ref(&requirement),
        std::slice::from_ref(&blocked),
    );
    if blocked_guidance.can_close {
        return Err("topology-blocked remote-required evidence must not allow closure".to_owned());
    }
    let blocked_reason = blocked_guidance
        .rejected_reasons
        .first()
        .ok_or_else(|| "blocked evidence should produce a rejection reason".to_owned())?;
    if !blocked_reason.contains("blocked") {
        return Err(format!(
            "blocked topology evidence should explain blocked status, got {blocked_reason:?}"
        ));
    }

    let superseded_guidance =
        verification_closure_guidance(Some("bd-rch-topology"), &[requirement], &[blocked, pass]);
    if !superseded_guidance.can_close {
        return Err(format!(
            "later authoritative remote pass should supersede topology-blocked attempt, got {:?}",
            superseded_guidance.rejected_reasons
        ));
    }

    Ok(())
}
