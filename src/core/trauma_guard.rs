//! bd-1n0np.18.1 — trauma-guard bypass-evidence collector.
//!
//! The trauma guard (`core::preflight_guard`) already looks up risk / anti-pattern
//! / failure memories for risky commands, but it never *learns* from how a human
//! actually resolved a halt. This collector adds the **high-precision** signal
//! only: when the guard policy-DENIED a preflight (`preflight.halt`) and a human
//! then issued a one-shot bypass for the **exact same command** (`preflight.bypass`)
//! shortly after, that correlation is audited evidence — so the guard's next
//! prompt for that command class can be calmer + cited (reducing guard fatigue
//! without weakening safety).
//!
//! This is the pure, deterministic correlator over explicit time windows; the
//! caller loads the two audit-event streams (via
//! `list_audit_by_action(PREFLIGHT_HALT/PREFLIGHT_BYPASS)`) and parses the
//! `command_hash` + timestamp. Correlation is by EXACT `command_hash` only.
//!
//! EXPLICITLY OUT OF SCOPE (per the duel): the confound-prone
//! "allowed-then-damaging" auto-detector. This module never infers harm from an
//! allowed command — it only records the human-confirmed bypass of a denied one.

use std::collections::BTreeMap;

use crate::curate::CandidateType;
use crate::db::CreateCurationCandidateInput;

pub const TRAUMA_GUARD_BYPASS_EVIDENCE_SCHEMA_V1: &str = "ee.trauma_guard.bypass_evidence.v1";

/// Source-type tag on calibration candidates this module proposes, so curate can
/// distinguish trauma-guard-driven calibrations.
pub const TRAUMA_GUARD_CALIBRATION_SOURCE_TYPE: &str = "trauma_guard_bypass_evidence";

/// Default window: a bypass counts as resolving a halt only if it lands within
/// this many seconds AFTER the halt (a human acting on the same denied command).
pub const BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS: i64 = 3_600;

/// A policy-denied preflight (`preflight.halt` audit event).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightHaltEvent {
    pub command_hash: String,
    pub occurred_at_epoch: i64,
}

/// A one-shot human bypass (`preflight.bypass` audit event).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightBypassEvent {
    pub command_hash: String,
    pub occurred_at_epoch: i64,
}

/// Correlated bypass evidence for one exact command hash: how many policy-denied
/// halts the human subsequently bypassed (within the window), and when the most
/// recent such bypass occurred. The guard cites this to calm repeat prompts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandBypassEvidence {
    pub command_hash: String,
    /// Number of (halt -> human bypass within window) correlations for this hash.
    pub correlated_bypass_count: u32,
    /// Epoch of the most recent correlated bypass.
    pub last_bypass_at_epoch: i64,
}

/// Correlate policy-denied halts with subsequent one-shot human bypasses for the
/// EXACT same command hash (bd-1n0np.18.1).
///
/// Deterministic: events are grouped by `command_hash`; within each group, halts
/// and bypasses are sorted by time and greedily matched — each halt consumes the
/// earliest still-unused bypass at `t in [t_halt, t_halt + window]` (a one-shot
/// resolution). Output is sorted by descending correlation count, then hash.
/// A non-positive window or empty input yields no evidence.
#[must_use]
pub fn correlate_bypass_evidence(
    halts: &[PreflightHaltEvent],
    bypasses: &[PreflightBypassEvent],
    window_seconds: i64,
) -> Vec<CommandBypassEvidence> {
    if window_seconds <= 0 || halts.is_empty() || bypasses.is_empty() {
        return Vec::new();
    }

    // command hash -> (sorted halt times, sorted bypass times)
    let mut halts_by_hash: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
    for halt in halts {
        let hash = halt.command_hash.trim();
        if !hash.is_empty() {
            halts_by_hash
                .entry(hash)
                .or_default()
                .push(halt.occurred_at_epoch);
        }
    }
    let mut bypasses_by_hash: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
    for bypass in bypasses {
        let hash = bypass.command_hash.trim();
        if !hash.is_empty() {
            bypasses_by_hash
                .entry(hash)
                .or_default()
                .push(bypass.occurred_at_epoch);
        }
    }

    let mut evidence: Vec<CommandBypassEvidence> = Vec::new();
    for (hash, halt_times) in halts_by_hash {
        let Some(bypass_times) = bypasses_by_hash.get(hash) else {
            continue;
        };
        let mut halt_times = halt_times;
        halt_times.sort_unstable();
        let mut bypass_times = bypass_times.clone();
        bypass_times.sort_unstable();

        let mut used = vec![false; bypass_times.len()];
        let mut count = 0_u32;
        let mut last_bypass = i64::MIN;
        for halt_at in halt_times {
            // earliest unused bypass at or after the halt, within the window.
            for (index, &bypass_at) in bypass_times.iter().enumerate() {
                if used[index] || bypass_at < halt_at {
                    continue;
                }
                if bypass_at > halt_at.saturating_add(window_seconds) {
                    break; // sorted: no later bypass can be in-window either.
                }
                used[index] = true;
                count = count.saturating_add(1);
                last_bypass = last_bypass.max(bypass_at);
                break;
            }
        }
        if count > 0 {
            evidence.push(CommandBypassEvidence {
                command_hash: hash.to_string(),
                correlated_bypass_count: count,
                last_bypass_at_epoch: last_bypass,
            });
        }
    }

    evidence.sort_by(|left, right| {
        right
            .correlated_bypass_count
            .cmp(&left.correlated_bypass_count)
            .then_with(|| left.command_hash.cmp(&right.command_hash))
    });
    evidence
}

/// Propose a PENDING curate calibration candidate from bypass evidence
/// (bd-1n0np.18.2 core). It records that this EXACT command was policy-denied then
/// human-bypassed, as a derived context memory the guard can cite to calm the
/// next prompt — `ee preflight learn`'s propose stage. NEVER auto-applied (status
/// `pending`, applied only via an explicit `ee curate accept`); the bypass
/// override itself stays one-shot (this never creates an auto-permanent allowlist).
#[must_use]
pub fn propose_calibration_candidate(
    evidence: &CommandBypassEvidence,
    workspace_id: &str,
) -> CreateCurationCandidateInput {
    let reason = format!(
        "Trauma-guard calibration: command {} was policy-denied then human-bypassed {} time(s) within the evidence window (last at epoch {}). Cite this so the next preflight prompt for this exact command is calmer; the override stays one-shot.",
        evidence.command_hash, evidence.correlated_bypass_count, evidence.last_bypass_at_epoch,
    );
    let proposed_content = format!(
        "Preflight calibration context for command {}: {} human bypass(es) recorded after a policy-deny. Treat repeat prompts for this exact command as informed, not novel.",
        evidence.command_hash, evidence.correlated_bypass_count,
    );
    CreateCurationCandidateInput {
        workspace_id: workspace_id.to_string(),
        candidate_type: CandidateType::CreateDerivedMemory.as_str().to_string(),
        target_memory_id: None,
        proposed_content: Some(proposed_content),
        proposed_confidence: None,
        proposed_trust_class: None,
        source_type: TRAUMA_GUARD_CALIBRATION_SOURCE_TYPE.to_string(),
        source_id: Some(evidence.command_hash.clone()),
        reason,
        confidence: 0.5,
        status: Some("pending".to_string()),
        created_at: None,
        ttl_expires_at: None,
        derivation_source_refs_json: None,
        derivation_metadata_json: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS, CommandBypassEvidence, PreflightBypassEvent,
        PreflightHaltEvent, TRAUMA_GUARD_CALIBRATION_SOURCE_TYPE, correlate_bypass_evidence,
        propose_calibration_candidate,
    };

    #[test]
    fn calibration_candidate_is_pending_derived_memory_citing_the_command() {
        let evidence = CommandBypassEvidence {
            command_hash: "blake3:cmd_a".to_string(),
            correlated_bypass_count: 3,
            last_bypass_at_epoch: 1_500,
        };
        let input = propose_calibration_candidate(&evidence, "wsp_test");
        assert_eq!(input.candidate_type, "create_derived_memory");
        assert_eq!(input.source_type, TRAUMA_GUARD_CALIBRATION_SOURCE_TYPE);
        assert_eq!(input.source_id.as_deref(), Some("blake3:cmd_a"));
        // Never auto-applied: pending until an explicit curate accept.
        assert_eq!(input.status.as_deref(), Some("pending"));
        assert!(
            input
                .proposed_content
                .unwrap_or_default()
                .contains("3 human bypass")
        );
    }

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
    fn correlates_halt_with_subsequent_exact_command_bypass() {
        let halts = vec![halt("blake3:cmd_a", 1_000)];
        let bypasses = vec![bypass("blake3:cmd_a", 1_500)];
        let evidence = correlate_bypass_evidence(
            &halts,
            &bypasses,
            BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS,
        );
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].command_hash, "blake3:cmd_a");
        assert_eq!(evidence[0].correlated_bypass_count, 1);
        assert_eq!(evidence[0].last_bypass_at_epoch, 1_500);
    }

    #[test]
    fn does_not_correlate_different_command_or_bypass_before_halt() {
        let halts = vec![halt("blake3:cmd_a", 1_000)];
        // Bypass is for a DIFFERENT command, and another is BEFORE the halt.
        let bypasses = vec![bypass("blake3:cmd_b", 1_500), bypass("blake3:cmd_a", 900)];
        let evidence = correlate_bypass_evidence(
            &halts,
            &bypasses,
            BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS,
        );
        assert!(
            evidence.is_empty(),
            "only an exact-command bypass AFTER the halt counts"
        );
    }

    #[test]
    fn bypass_outside_window_is_not_correlated() {
        let halts = vec![halt("blake3:cmd_a", 1_000)];
        let bypasses = vec![bypass("blake3:cmd_a", 1_000 + 10_000)];
        let evidence = correlate_bypass_evidence(&halts, &bypasses, 3_600);
        assert!(
            evidence.is_empty(),
            "a bypass past the window does not count"
        );
    }

    #[test]
    fn one_shot_each_bypass_resolves_at_most_one_halt() {
        // Two halts for the same command, only one in-window bypass -> count 1.
        let halts = vec![halt("blake3:cmd_a", 1_000), halt("blake3:cmd_a", 1_100)];
        let bypasses = vec![bypass("blake3:cmd_a", 1_200)];
        let evidence = correlate_bypass_evidence(&halts, &bypasses, 3_600);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].correlated_bypass_count, 1);
    }

    #[test]
    fn evidence_is_deterministic_and_ranked() {
        let halts = vec![
            halt("blake3:rare", 1_000),
            halt("blake3:common", 1_000),
            halt("blake3:common", 2_000),
        ];
        let bypasses = vec![
            bypass("blake3:rare", 1_100),
            bypass("blake3:common", 1_100),
            bypass("blake3:common", 2_100),
        ];
        let mut reversed_halts = halts.clone();
        reversed_halts.reverse();
        let first = correlate_bypass_evidence(&halts, &bypasses, 3_600);
        let second = correlate_bypass_evidence(&reversed_halts, &bypasses, 3_600);
        assert_eq!(first, second, "correlation is independent of input order");
        // `common` (2 correlations) outranks `rare` (1).
        assert_eq!(first[0].command_hash, "blake3:common");
        assert_eq!(first[0].correlated_bypass_count, 2);
        assert_eq!(first[1].command_hash, "blake3:rare");
    }

    #[test]
    fn non_positive_window_yields_no_evidence() {
        let halts = vec![halt("blake3:cmd_a", 1_000)];
        let bypasses = vec![bypass("blake3:cmd_a", 1_100)];
        assert!(correlate_bypass_evidence(&halts, &bypasses, 0).is_empty());
    }
}
