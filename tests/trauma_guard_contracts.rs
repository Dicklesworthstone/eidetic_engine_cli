//! bd-1n0np.18.3 — trauma-guard bypass-evidence contract tests.
//!
//! Pins the *public* contracts of the trauma-guard learn loop (18.1 correlator +
//! 18.2 calibration proposer) from an external consumer's view, complementing the
//! per-module unit tests. The high-precision safety invariants that must never
//! regress: exact-command-only correlation, human-confirmed (bypass-after-halt)
//! only, never an auto-permanent allowlist (pending candidates).
//!
//! Golden bodies for the `ee.trauma_guard.bypass_evidence.v1` JSON surface are
//! RCH-remote-regen only and owed there (bd-17c65.10.17).

use ee::core::trauma_guard::{
    BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS, CommandBypassEvidence, PreflightBypassEvent,
    PreflightHaltEvent, TRAUMA_GUARD_BYPASS_EVIDENCE_SCHEMA_V1,
    TRAUMA_GUARD_CALIBRATION_SOURCE_TYPE, correlate_bypass_evidence, propose_calibration_candidate,
};

fn halt(hash: &str, at: i64) -> PreflightHaltEvent {
    PreflightHaltEvent {
        command_hash: hash.to_string(),
        occurred_at_epoch: at,
    }
}

fn bypass(hash: &str, at: i64) -> PreflightBypassEvent {
    PreflightBypassEvent {
        command_hash: hash.to_string(),
        occurred_at_epoch: at,
    }
}

#[test]
fn schema_and_window_contract_is_stable() {
    assert_eq!(
        TRAUMA_GUARD_BYPASS_EVIDENCE_SCHEMA_V1,
        "ee.trauma_guard.bypass_evidence.v1"
    );
    assert_eq!(BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS, 3_600);
}

#[test]
fn correlation_is_exact_command_and_human_confirmed_only() {
    let halts = vec![halt("blake3:risky", 1_000)];
    let bypasses = vec![
        // wrong command -> never correlates another command's calibration.
        bypass("blake3:other", 1_100),
        // a bypass BEFORE the halt is not a resolution of it.
        bypass("blake3:risky", 500),
    ];
    assert!(
        correlate_bypass_evidence(
            &halts,
            &bypasses,
            BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS
        )
        .is_empty(),
        "only an exact-command bypass occurring AFTER the halt is evidence"
    );

    // The human-confirmed case does correlate.
    let confirmed = correlate_bypass_evidence(
        &halts,
        &[bypass("blake3:risky", 1_200)],
        BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS,
    );
    assert_eq!(confirmed.len(), 1);
    assert_eq!(confirmed[0].command_hash, "blake3:risky");
}

#[test]
fn calibration_proposal_is_never_an_auto_permanent_allowlist() {
    let evidence = CommandBypassEvidence {
        command_hash: "blake3:risky".to_string(),
        correlated_bypass_count: 4,
        last_bypass_at_epoch: 2_000,
    };
    let candidate = propose_calibration_candidate(&evidence, "wsp_contract_test");
    // Pending only — applied solely via an explicit `ee curate accept`.
    assert_eq!(candidate.status.as_deref(), Some("pending"));
    assert_eq!(candidate.source_type, TRAUMA_GUARD_CALIBRATION_SOURCE_TYPE);
    // Cites the exact command it calibrates.
    assert_eq!(candidate.source_id.as_deref(), Some("blake3:risky"));
    // It adds calibration CONTEXT (a derived memory), it does not delete/allow.
    assert_eq!(candidate.candidate_type, "create_derived_memory");
}
