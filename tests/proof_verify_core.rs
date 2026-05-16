#[path = "../src/core/proof_verify.rs"]
mod proof_verify;

use std::path::Path;

use proof_verify::{
    PROOF_CHECK_SCHEMA_V1, ProofArtifactKind, ProofCheckStatus, ProofCommandOutcome,
    ProofCommandRunner, discover_proof_artifacts, run_proof_checks,
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
