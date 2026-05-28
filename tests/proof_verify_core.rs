mod models {
    pub use ee::models::*;
}

/// Sibling helper required by `src/core/proof_verify.rs`, which is included
/// here via `#[path]` and references `super::duration_millis_saturating`.
/// Mirrors the implementation in `src/core/mod.rs` so the test crate root
/// satisfies the `super::` lookup without exposing additional crate-private
/// symbols. Keep the body in sync if the production helper evolves.
#[must_use]
pub(crate) fn duration_millis_saturating(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[path = "../src/core/proof_verify.rs"]
mod proof_verify;

use std::path::Path;

use proof_verify::{
    PROOF_CHECK_SCHEMA_V1, PROOF_TOOL_MISSING_CODE, PROOF_VIOLATION_DETECTED_CODE,
    ProofArtifactKind, ProofCheckStatus, ProofCommandOutcome, ProofCommandRunner,
    discover_proof_artifacts, run_proof_checks,
};

#[derive(Clone, Debug)]
struct PassingRunner;

impl ProofCommandRunner for PassingRunner {
    fn run(&self, artifact: &proof_verify::ProofArtifact) -> ProofCommandOutcome {
        ProofCommandOutcome {
            tool_available: true,
            duration_ms: 1,
            exit_code: Some(0),
            stdout: format!("checked {}", artifact.path.display()),
            stderr: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct MissingToolRunner;

impl ProofCommandRunner for MissingToolRunner {
    fn run(&self, artifact: &proof_verify::ProofArtifact) -> ProofCommandOutcome {
        ProofCommandOutcome {
            tool_available: false,
            duration_ms: 0,
            exit_code: None,
            stdout: String::new(),
            stderr: format!("{} not found on PATH", artifact.kind.default_tool()),
        }
    }
}

#[derive(Clone, Debug)]
struct FailingRunner;

impl ProofCommandRunner for FailingRunner {
    fn run(&self, artifact: &proof_verify::ProofArtifact) -> ProofCommandOutcome {
        ProofCommandOutcome {
            tool_available: true,
            duration_ms: 2,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: format!("{} proof check failed", artifact.kind.as_str()),
        }
    }
}

#[test]
fn discovers_committed_lean_and_tla_artifacts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs");
    let artifacts = discover_proof_artifacts(&root).expect("proof discovery should succeed");

    assert_eq!(artifacts.len(), 2);
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind == ProofArtifactKind::Lean4
                && artifact.path.ends_with("pack_determinism.lean")
                && artifact.invariants.contains(&"pack_determinism".to_owned()))
    );
    assert!(artifacts.iter().any(|artifact| {
        artifact.kind == ProofArtifactKind::TlaPlus
            && artifact.path.ends_with("agent_mail_coordination.tla")
            && artifact
                .invariants
                .contains(&"exclusive_reservations_do_not_overlap".to_owned())
    }));
}

#[test]
fn artifact_kind_wire_values_match_schema() {
    assert_eq!(ProofArtifactKind::Lean4.as_str(), "lean4");
    assert_eq!(ProofArtifactKind::Lean4.default_tool(), "lake");
    assert_eq!(ProofArtifactKind::TlaPlus.as_str(), "tla+");
    assert_eq!(ProofArtifactKind::TlaPlus.default_tool(), "tlc");
}

#[test]
fn passing_runner_maps_kind_specific_success_statuses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs");
    let report = run_proof_checks(&root, &PassingRunner).expect("proof checks should run");

    assert!(report.success);
    assert_eq!(report.schema, PROOF_CHECK_SCHEMA_V1);
    assert!(report.degraded.is_empty());
    assert!(report.checks.iter().any(|check| {
        check.artifact.kind == ProofArtifactKind::Lean4 && check.status == ProofCheckStatus::Proved
    }));
    assert!(report.checks.iter().any(|check| {
        check.artifact.kind == ProofArtifactKind::TlaPlus
            && check.status == ProofCheckStatus::ModelChecked
    }));
}

#[test]
fn missing_tool_runner_keeps_report_successful_with_degradation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs");
    let report = run_proof_checks(&root, &MissingToolRunner).expect("proof checks should run");

    assert!(report.success);
    assert_eq!(
        report.degraded,
        vec![format!("degraded.{PROOF_TOOL_MISSING_CODE}")]
    );
    assert!(report.checks.iter().all(|check| {
        check.status == ProofCheckStatus::ToolMissing
            && check.exit_code.is_none()
            && check.stderr.contains(check.artifact.kind.default_tool())
    }));
}

#[test]
fn failing_runner_marks_report_failed_with_violation_degradation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs");
    let report = run_proof_checks(&root, &FailingRunner).expect("proof checks should run");

    assert!(!report.success);
    assert_eq!(
        report.degraded,
        vec![format!("degraded.{PROOF_VIOLATION_DETECTED_CODE}")]
    );
    assert!(report.checks.iter().all(|check| {
        check.status == ProofCheckStatus::Violation
            && check.exit_code == Some(1)
            && check.stderr.contains(check.artifact.kind.as_str())
    }));
}
