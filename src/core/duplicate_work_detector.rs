//! IW4 (bd-1zb7k.17.4): pure duplicate-work detector that joins three signal
//! sources — pending verifications, file reservations, and Beads claims —
//! into one advisory verdict before an agent spends another RCH slot or
//! edits a file another agent is already covering.
//!
//! Acceptance shape (from bead body):
//! - `duplicateVerificationCandidates[]` keyed by command fingerprint and
//!   source-tree fingerprint.
//! - `duplicateEditCandidates[]` keyed by file-reservation overlap and
//!   Bead/thread overlap.
//! - `knownBlockers[]` with likely owner and evidence hash.
//! - `suggestedAction` is one of:
//!   `reuse_evidence | coordinate_with_owner | wait_for_reservation |
//!   run_new_verification`.
//! - `confidence` plus a rationale string.
//!
//! The detector is intentionally pure: it consumes redacted snapshots of
//! the three signal sources, performs no I/O, and emits a deterministic
//! advisory. It never cancels jobs, edits Beads, or claims ownership.

use std::collections::BTreeSet;

use serde::{Serialize, Serializer};

/// Public schema identifier for the detector's verdict shape.
pub const DUPLICATE_WORK_DETECTOR_SCHEMA_V1: &str = "ee.swarm.duplicate_work.v1";

/// Suggested action emitted by the detector. Closed set so contract tests
/// can pin the vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuggestedAction {
    ReuseEvidence,
    CoordinateWithOwner,
    WaitForReservation,
    RunNewVerification,
}

impl SuggestedAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReuseEvidence => "reuse_evidence",
            Self::CoordinateWithOwner => "coordinate_with_owner",
            Self::WaitForReservation => "wait_for_reservation",
            Self::RunNewVerification => "run_new_verification",
        }
    }
}

impl Serialize for SuggestedAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Confidence band the detector emitted. Stable ordinal so agents can
/// threshold on the band itself rather than parsing the rationale string.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl Serialize for Confidence {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// A verification candidate the caller is considering running. The
/// detector compares the (command_fingerprint, source_tree_fingerprint)
/// pair against the active set in [`DuplicateWorkInputs`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationCandidate {
    pub command_fingerprint: String,
    pub source_tree_fingerprint: String,
}

/// A file reservation the swarm currently holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveFileReservation {
    pub path_pattern: String,
    pub holder_agent: String,
    pub bead_id: Option<String>,
    pub thread_id: Option<String>,
}

/// A claimed Beads item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveBeadClaim {
    pub bead_id: String,
    pub holder_agent: String,
}

/// A known blocker carried forward from the verification ledger; the
/// detector surfaces it so the caller can decide whether to wait or
/// override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownBlockerInput {
    pub blocker_kind: String,
    pub command_fingerprint: String,
    pub evidence_hash: String,
    pub owner_agent: Option<String>,
    pub remediation_bead: Option<String>,
}

/// Active verification matching the caller's candidate, lifted from the
/// in-flight pool tracked by the verification broker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveVerification {
    pub command_fingerprint: String,
    pub source_tree_fingerprint: String,
    pub holder_agent: String,
    pub started_at_rfc3339: Option<String>,
}

/// Caller-facing detector inputs. Everything is pre-redacted by the broker
/// + Agent Mail snapshot upstream; the detector itself never reads paths,
/// queries, or memory bodies.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DuplicateWorkInputs<'a> {
    pub candidate: Option<VerificationCandidate>,
    pub edit_paths: &'a [&'a str],
    pub bead_claim: Option<&'a str>,
    pub active_verifications: &'a [ActiveVerification],
    pub active_reservations: &'a [ActiveFileReservation],
    pub active_bead_claims: &'a [ActiveBeadClaim],
    pub known_blockers: &'a [KnownBlockerInput],
    pub self_agent: &'a str,
}

/// One duplicate-verification finding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateVerificationCandidate {
    pub command_fingerprint: String,
    pub source_tree_fingerprint: String,
    pub holder_agent: String,
    pub started_at_rfc3339: Option<String>,
}

/// One duplicate-edit finding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateEditCandidate {
    pub path_pattern: String,
    pub holder_agent: String,
    pub overlapping_bead_id: Option<String>,
    pub overlapping_thread_id: Option<String>,
}

/// One known-blocker advisory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownBlockerAdvisory {
    pub blocker_kind: String,
    pub command_fingerprint: String,
    pub evidence_hash: String,
    pub owner_agent: Option<String>,
    pub remediation_bead: Option<String>,
}

/// Verdict shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateWorkVerdict {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub duplicate_verification_candidates: Vec<DuplicateVerificationCandidate>,
    pub duplicate_edit_candidates: Vec<DuplicateEditCandidate>,
    pub known_blockers: Vec<KnownBlockerAdvisory>,
    pub suggested_action: SuggestedAction,
    pub confidence: Confidence,
    pub rationale: &'static str,
}

impl DuplicateWorkVerdict {
    /// True iff any duplicate signal was found.
    #[must_use]
    pub fn any_duplicates(&self) -> bool {
        !self.duplicate_verification_candidates.is_empty()
            || !self.duplicate_edit_candidates.is_empty()
    }
}

/// Pure detector entrypoint. Returns a deterministic advisory for the
/// caller without consulting any external state.
#[must_use]
pub fn detect_duplicate_work(inputs: &DuplicateWorkInputs<'_>) -> DuplicateWorkVerdict {
    let duplicate_verifications = find_duplicate_verifications(inputs);
    let duplicate_edits = find_duplicate_edits(inputs);
    let known_blockers = find_relevant_blockers(inputs);

    let (suggested_action, confidence, rationale) = decide_suggested_action(
        &duplicate_verifications,
        &duplicate_edits,
        &known_blockers,
        inputs.candidate.is_some(),
    );

    DuplicateWorkVerdict {
        schema: DUPLICATE_WORK_DETECTOR_SCHEMA_V1,
        side_effect_free: true,
        duplicate_verification_candidates: duplicate_verifications,
        duplicate_edit_candidates: duplicate_edits,
        known_blockers,
        suggested_action,
        confidence,
        rationale,
    }
}

fn find_duplicate_verifications(
    inputs: &DuplicateWorkInputs<'_>,
) -> Vec<DuplicateVerificationCandidate> {
    let Some(candidate) = inputs.candidate.as_ref() else {
        return Vec::new();
    };
    let mut findings: Vec<DuplicateVerificationCandidate> = Vec::new();
    for active in inputs.active_verifications {
        if active.holder_agent == inputs.self_agent {
            continue;
        }
        if active.command_fingerprint != candidate.command_fingerprint {
            continue;
        }
        if active.source_tree_fingerprint != candidate.source_tree_fingerprint {
            continue;
        }
        findings.push(DuplicateVerificationCandidate {
            command_fingerprint: active.command_fingerprint.clone(),
            source_tree_fingerprint: active.source_tree_fingerprint.clone(),
            holder_agent: active.holder_agent.clone(),
            started_at_rfc3339: active.started_at_rfc3339.clone(),
        });
    }
    findings.sort_by(|a, b| {
        a.holder_agent
            .cmp(&b.holder_agent)
            .then_with(|| a.started_at_rfc3339.cmp(&b.started_at_rfc3339))
    });
    findings
}

fn find_duplicate_edits(inputs: &DuplicateWorkInputs<'_>) -> Vec<DuplicateEditCandidate> {
    if inputs.edit_paths.is_empty() {
        return Vec::new();
    }
    let edit_set: BTreeSet<&str> = inputs.edit_paths.iter().copied().collect();
    let bead_set: BTreeSet<&str> = inputs
        .active_bead_claims
        .iter()
        .filter(|claim| claim.holder_agent != inputs.self_agent)
        .map(|claim| claim.bead_id.as_str())
        .collect();
    let mut findings: Vec<DuplicateEditCandidate> = Vec::new();
    for reservation in inputs.active_reservations {
        if reservation.holder_agent == inputs.self_agent {
            continue;
        }
        if !path_pattern_overlaps_any(reservation.path_pattern.as_str(), &edit_set) {
            continue;
        }
        // The reservation overlaps the caller's edit set. Surface the
        // bead/thread linkage so coordinate_with_owner can include them.
        let overlapping_bead_id = reservation.bead_id.clone().filter(|bead| {
            inputs.bead_claim == Some(bead.as_str()) || bead_set.contains(bead.as_str())
        });
        let overlapping_thread_id = reservation.thread_id.clone();
        findings.push(DuplicateEditCandidate {
            path_pattern: reservation.path_pattern.clone(),
            holder_agent: reservation.holder_agent.clone(),
            overlapping_bead_id,
            overlapping_thread_id,
        });
    }
    findings.sort_by(|a, b| {
        a.holder_agent
            .cmp(&b.holder_agent)
            .then_with(|| a.path_pattern.cmp(&b.path_pattern))
    });
    findings
}

fn path_pattern_overlaps_any(pattern: &str, edit_set: &BTreeSet<&str>) -> bool {
    edit_set
        .iter()
        .any(|path| path_patterns_overlap(pattern, path))
}

/// Minimal glob match supporting trailing `**`, leading prefix, and exact
/// equality. Intentionally narrow — the upstream Agent Mail layer already
/// normalises patterns and the detector never needs richer matching.
fn path_patterns_overlap(left: &str, right: &str) -> bool {
    path_pattern_matches(left, right) || path_pattern_matches(right, left)
}

fn path_pattern_matches(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(prefix);
    }
    if let Some(prefix) = pattern.strip_suffix("**") {
        return path.starts_with(prefix);
    }
    if let Some(prefix) = pattern.strip_suffix("*") {
        return path.starts_with(prefix);
    }
    false
}

fn find_relevant_blockers(inputs: &DuplicateWorkInputs<'_>) -> Vec<KnownBlockerAdvisory> {
    let Some(candidate) = inputs.candidate.as_ref() else {
        return inputs
            .known_blockers
            .iter()
            .map(KnownBlockerAdvisory::from_input)
            .collect();
    };
    let mut findings: Vec<KnownBlockerAdvisory> = inputs
        .known_blockers
        .iter()
        .filter(|blocker| blocker.command_fingerprint == candidate.command_fingerprint)
        .map(KnownBlockerAdvisory::from_input)
        .collect();
    findings.sort_by(|a, b| a.evidence_hash.cmp(&b.evidence_hash));
    findings
}

impl KnownBlockerAdvisory {
    fn from_input(input: &KnownBlockerInput) -> Self {
        Self {
            blocker_kind: input.blocker_kind.clone(),
            command_fingerprint: input.command_fingerprint.clone(),
            evidence_hash: input.evidence_hash.clone(),
            owner_agent: input.owner_agent.clone(),
            remediation_bead: input.remediation_bead.clone(),
        }
    }
}

fn decide_suggested_action(
    duplicate_verifications: &[DuplicateVerificationCandidate],
    duplicate_edits: &[DuplicateEditCandidate],
    known_blockers: &[KnownBlockerAdvisory],
    has_candidate: bool,
) -> (SuggestedAction, Confidence, &'static str) {
    if !known_blockers.is_empty() {
        return (
            SuggestedAction::ReuseEvidence,
            Confidence::High,
            "A matching known blocker exists; reuse the recorded evidence instead of launching a new verification.",
        );
    }
    if !duplicate_verifications.is_empty() {
        return (
            SuggestedAction::ReuseEvidence,
            Confidence::High,
            "Another agent is already running this exact verification; wait for its result instead of launching a duplicate.",
        );
    }
    if !duplicate_edits.is_empty() {
        return (
            SuggestedAction::CoordinateWithOwner,
            Confidence::Medium,
            "Another agent holds a file reservation overlapping your planned edits; coordinate via Agent Mail before editing.",
        );
    }
    if has_candidate {
        (
            SuggestedAction::RunNewVerification,
            Confidence::Medium,
            "No duplicate or blocker matched the candidate; running the new verification is appropriate.",
        )
    } else {
        (
            SuggestedAction::WaitForReservation,
            Confidence::Low,
            "No candidate was supplied; the detector cannot suggest a verification action — wait or refine inputs.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> &'static str {
        "GrayForest"
    }

    fn other_active_verif() -> ActiveVerification {
        ActiveVerification {
            command_fingerprint: "cargo_test:rch_verify_contract".to_string(),
            source_tree_fingerprint: "blake3:abc123".to_string(),
            holder_agent: "OtherAgent".to_string(),
            started_at_rfc3339: Some("2026-05-20T01:00:00Z".to_string()),
        }
    }

    fn matching_candidate() -> VerificationCandidate {
        VerificationCandidate {
            command_fingerprint: "cargo_test:rch_verify_contract".to_string(),
            source_tree_fingerprint: "blake3:abc123".to_string(),
        }
    }

    #[test]
    fn two_agents_running_same_focused_cargo_test_after_blocker_reuses_evidence() {
        let blocker = KnownBlockerInput {
            blocker_kind: "rch_verify_topology_blocked".to_string(),
            command_fingerprint: "cargo_test:rch_verify_contract".to_string(),
            evidence_hash: "blake3:def456".to_string(),
            owner_agent: Some("NobleMill".to_string()),
            remediation_bead: Some("bd-17c65.10.17.1".to_string()),
        };
        let active = [other_active_verif()];
        let blockers = [blocker];
        let inputs = DuplicateWorkInputs {
            candidate: Some(matching_candidate()),
            edit_paths: &[],
            bead_claim: None,
            active_verifications: &active,
            active_reservations: &[],
            active_bead_claims: &[],
            known_blockers: &blockers,
            self_agent: agent(),
        };
        let verdict = detect_duplicate_work(&inputs);
        assert_eq!(verdict.suggested_action, SuggestedAction::ReuseEvidence);
        assert_eq!(verdict.confidence, Confidence::High);
        assert_eq!(verdict.duplicate_verification_candidates.len(), 1);
        assert_eq!(verdict.known_blockers.len(), 1);
        assert!(verdict.any_duplicates());
    }

    #[test]
    fn overlapping_reservation_with_different_bead_recommends_coordination_not_block() {
        let reservation = ActiveFileReservation {
            path_pattern: "src/core/**".to_string(),
            holder_agent: "OtherAgent".to_string(),
            bead_id: Some("bd-other".to_string()),
            thread_id: Some("thread-1".to_string()),
        };
        let bead_claim = ActiveBeadClaim {
            bead_id: "bd-other".to_string(),
            holder_agent: "OtherAgent".to_string(),
        };
        let reservations = [reservation];
        let claims = [bead_claim];
        let edits = ["src/core/foo.rs"];
        let inputs = DuplicateWorkInputs {
            candidate: None,
            edit_paths: &edits,
            bead_claim: Some("bd-mine"),
            active_verifications: &[],
            active_reservations: &reservations,
            active_bead_claims: &claims,
            known_blockers: &[],
            self_agent: agent(),
        };
        let verdict = detect_duplicate_work(&inputs);
        assert_eq!(
            verdict.suggested_action,
            SuggestedAction::CoordinateWithOwner
        );
        assert_eq!(verdict.confidence, Confidence::Medium);
        assert_eq!(verdict.duplicate_edit_candidates.len(), 1);
        let finding = &verdict.duplicate_edit_candidates[0];
        assert_eq!(finding.holder_agent, "OtherAgent");
        assert_eq!(finding.path_pattern, "src/core/**");
        // The bead overlap clause: caller's bead != holder's bead, and the
        // active-claims set carries the holder's bead → it counts as a
        // bead-overlap signal.
        assert_eq!(finding.overlapping_bead_id, Some("bd-other".to_string()));
    }

    #[test]
    fn self_held_reservations_and_verifications_are_ignored() {
        let own_active = ActiveVerification {
            holder_agent: agent().to_string(),
            ..other_active_verif()
        };
        let own_reservation = ActiveFileReservation {
            path_pattern: "src/core/**".to_string(),
            holder_agent: agent().to_string(),
            bead_id: Some("bd-mine".to_string()),
            thread_id: None,
        };
        let edits = ["src/core/foo.rs"];
        let actives = [own_active];
        let reservations = [own_reservation];
        let inputs = DuplicateWorkInputs {
            candidate: Some(matching_candidate()),
            edit_paths: &edits,
            bead_claim: Some("bd-mine"),
            active_verifications: &actives,
            active_reservations: &reservations,
            active_bead_claims: &[],
            known_blockers: &[],
            self_agent: agent(),
        };
        let verdict = detect_duplicate_work(&inputs);
        assert!(verdict.duplicate_verification_candidates.is_empty());
        assert!(verdict.duplicate_edit_candidates.is_empty());
        assert_eq!(
            verdict.suggested_action,
            SuggestedAction::RunNewVerification
        );
    }

    #[test]
    fn no_candidate_with_clean_world_suggests_wait() {
        let inputs = DuplicateWorkInputs {
            candidate: None,
            edit_paths: &[],
            bead_claim: None,
            active_verifications: &[],
            active_reservations: &[],
            active_bead_claims: &[],
            known_blockers: &[],
            self_agent: agent(),
        };
        let verdict = detect_duplicate_work(&inputs);
        assert_eq!(
            verdict.suggested_action,
            SuggestedAction::WaitForReservation
        );
        assert_eq!(verdict.confidence, Confidence::Low);
    }

    #[test]
    fn known_blocker_outranks_duplicate_verification_signal() {
        let blocker = KnownBlockerInput {
            blocker_kind: "rch_verify_topology_blocked".to_string(),
            command_fingerprint: "cargo_test:rch_verify_contract".to_string(),
            evidence_hash: "blake3:def456".to_string(),
            owner_agent: None,
            remediation_bead: Some("bd-17c65.10.17.1".to_string()),
        };
        let active = [other_active_verif()];
        let blockers = [blocker];
        let inputs = DuplicateWorkInputs {
            candidate: Some(matching_candidate()),
            edit_paths: &[],
            bead_claim: None,
            active_verifications: &active,
            active_reservations: &[],
            active_bead_claims: &[],
            known_blockers: &blockers,
            self_agent: agent(),
        };
        let verdict = detect_duplicate_work(&inputs);
        // Both signals are present; the blocker is high-priority → suggested
        // action is reuse_evidence with high confidence.
        assert_eq!(verdict.suggested_action, SuggestedAction::ReuseEvidence);
        assert_eq!(verdict.confidence, Confidence::High);
    }

    #[test]
    fn detector_is_deterministic_across_repeat_calls() {
        let active = [other_active_verif()];
        let reservation = ActiveFileReservation {
            path_pattern: "src/core/**".to_string(),
            holder_agent: "OtherAgent".to_string(),
            bead_id: Some("bd-other".to_string()),
            thread_id: None,
        };
        let reservations = [reservation];
        let edits = ["src/core/foo.rs"];
        let inputs = DuplicateWorkInputs {
            candidate: Some(matching_candidate()),
            edit_paths: &edits,
            bead_claim: None,
            active_verifications: &active,
            active_reservations: &reservations,
            active_bead_claims: &[],
            known_blockers: &[],
            self_agent: agent(),
        };
        let a = detect_duplicate_work(&inputs);
        let b = detect_duplicate_work(&inputs);
        assert_eq!(a, b);
        let a_json = serde_json::to_string(&a).expect("serialize a");
        let b_json = serde_json::to_string(&b).expect("serialize b");
        assert_eq!(a_json, b_json);
    }

    #[test]
    fn path_pattern_matches_glob_double_star_and_exact() {
        assert!(path_pattern_matches("src/core/**", "src/core/foo.rs"));
        assert!(path_pattern_matches("src/core/**", "src/core/sub/bar.rs"));
        assert!(path_pattern_matches("src/core/foo.rs", "src/core/foo.rs"));
        assert!(path_pattern_matches("src/*", "src/foo.rs"));
        assert!(!path_pattern_matches("src/core/**", "tests/lib.rs"));
        assert!(!path_pattern_matches("src/core/foo.rs", "src/core/bar.rs"));
    }

    #[test]
    fn path_patterns_overlap_symmetrically_for_candidate_globs() {
        assert!(path_patterns_overlap("src/core/foo.rs", "src/core/**"));
        assert!(path_patterns_overlap("src/core/**", "src/core/foo.rs"));
        assert!(path_patterns_overlap("src/core/*", "src/core/**"));
        assert!(!path_patterns_overlap("src/core/**", "src/db/**"));
    }

    #[test]
    fn candidate_edit_glob_overlapping_exact_reservation_recommends_coordination() {
        let reservation = ActiveFileReservation {
            path_pattern: "src/core/foo.rs".to_string(),
            holder_agent: "OtherAgent".to_string(),
            bead_id: Some("bd-other".to_string()),
            thread_id: Some("thread-1".to_string()),
        };
        let reservations = [reservation];
        let edits = ["src/core/**"];
        let inputs = DuplicateWorkInputs {
            candidate: None,
            edit_paths: &edits,
            bead_claim: Some("bd-mine"),
            active_verifications: &[],
            active_reservations: &reservations,
            active_bead_claims: &[],
            known_blockers: &[],
            self_agent: agent(),
        };

        let verdict = detect_duplicate_work(&inputs);

        assert_eq!(
            verdict.suggested_action,
            SuggestedAction::CoordinateWithOwner
        );
        assert_eq!(verdict.duplicate_edit_candidates.len(), 1);
        assert_eq!(
            verdict.duplicate_edit_candidates[0].path_pattern,
            "src/core/foo.rs"
        );
    }

    #[test]
    fn verdict_serializes_to_camel_case_with_stable_schema() {
        let verdict = detect_duplicate_work(&DuplicateWorkInputs {
            candidate: None,
            edit_paths: &[],
            bead_claim: None,
            active_verifications: &[],
            active_reservations: &[],
            active_bead_claims: &[],
            known_blockers: &[],
            self_agent: agent(),
        });
        let json = serde_json::to_value(&verdict).expect("serialize verdict");
        assert_eq!(
            json.get("schema").and_then(|v| v.as_str()),
            Some(DUPLICATE_WORK_DETECTOR_SCHEMA_V1)
        );
        assert_eq!(
            json.get("sideEffectFree").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(json.get("duplicateVerificationCandidates").is_some());
        assert!(json.get("duplicateEditCandidates").is_some());
        assert!(json.get("knownBlockers").is_some());
        assert!(json.get("suggestedAction").is_some());
        assert!(json.get("confidence").is_some());
        assert!(json.get("rationale").is_some());
    }
}
