//! bd-1n0np.7.4 — audited contradiction resolution: propose → validate (→ apply
//! via curate).
//!
//! Resolutions follow ADR-0014 (propose → validate → apply). This module is the
//! contradiction-specific propose + validate stage: given a detected, decided
//! contradiction (winner vs loser, from `contradiction_guard`), it proposes the
//! appropriate resolution and bridges it to a curate [`CreateCurationCandidateInput`]
//! so the *existing* curate pipeline performs the audited apply (Supersede =
//! tombstone-with-pointer, Split = scope edit, Merge = consolidation), emitting
//! the curate audit rows.
//!
//! Critically, this NEVER auto-applies: it emits a *pending* curation candidate
//! that a human/agent must confirm through `ee curate accept`. The resolution kind
//! is chosen from the explicit conflict signal that produced the contradiction.

use crate::core::contradiction_detect::ExplicitConflictSignal;
use crate::curate::CandidateType;
use crate::db::CreateCurationCandidateInput;

/// Source-type tag recorded on curation candidates this module proposes, so the
/// curate surface can distinguish contradiction-driven resolutions.
pub const CONTRADICTION_RESOLUTION_SOURCE_TYPE: &str = "contradiction_resolution";

/// Default confidence attached to a proposed (unconfirmed) resolution.
pub const CONTRADICTION_RESOLUTION_PROPOSAL_CONFIDENCE: f32 = 0.5;

/// How an unresolved contradiction should be resolved (ADR-0014 vocabulary).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContradictionResolutionKind {
    /// The winner replaces the loser: tombstone-with-pointer (`curate supersede`).
    Supersede,
    /// Each side is scoped to its own validity/scope (`curate split`).
    ScopeSplit,
    /// The two are consolidated into one (`curate merge`).
    Merge,
}

impl ContradictionResolutionKind {
    /// Stable string form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supersede => "supersede",
            Self::ScopeSplit => "scope_split",
            Self::Merge => "merge",
        }
    }

    /// The curate [`CandidateType`] this resolution applies through, so the
    /// existing curate propose→validate→apply pipeline performs the audited
    /// mutation.
    #[must_use]
    pub const fn candidate_type(self) -> CandidateType {
        // bd-jkgta: a confirmed contradiction resolution always applies the same
        // content-free mutation — "keep the winner, tombstone-with-pointer the
        // loser" (the documented principle above). It must NOT map to
        // Supersede/Split/Merge: those set CandidateType::requires_content() and
        // the curate accept/apply path rejects them as `content_required_for_type`
        // because the pure proposal carries no proposed_content. Tombstone is the
        // content-free type matching that principle; the winner is recorded as the
        // candidate `source_id`. The analytical kind is preserved in the rationale.
        match self {
            Self::Supersede | Self::ScopeSplit | Self::Merge => CandidateType::Tombstone,
        }
    }

    /// Choose the resolution kind from the explicit conflict signal that produced
    /// the contradiction. Conservative default is `Supersede` (the safest
    /// confirmed proposal: keep the winner, tombstone-with-pointer the loser).
    #[must_use]
    pub const fn from_signal(signal: ExplicitConflictSignal) -> Self {
        match signal {
            // Genuinely opposed assertions / explicit supersession / trust split:
            // the winner supersedes the loser.
            ExplicitConflictSignal::ContradictionLink
            | ExplicitConflictSignal::Supersession
            | ExplicitConflictSignal::TrustOutcomeSplit
            | ExplicitConflictSignal::RepeatedCoSelection => Self::Supersede,
            // Near-duplicate-but-divergent content consolidates.
            ExplicitConflictSignal::DuplicateDivergent => Self::Merge,
            // Both true within different windows/scopes -> scope-split.
            ExplicitConflictSignal::ValidityWindowOverlap => Self::ScopeSplit,
        }
    }
}

/// A proposed (NOT yet applied) contradiction resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContradictionResolutionProposal {
    /// The memory kept / preferred (the contradiction-guard survivor).
    pub winner_memory_id: String,
    /// The memory acted on (superseded / scope-edited / merged away).
    pub loser_memory_id: String,
    pub kind: ContradictionResolutionKind,
    /// The explicit signal that produced the contradiction.
    pub signal: ExplicitConflictSignal,
    /// Human-readable rationale recorded on the curation candidate.
    pub rationale: String,
}

/// Validation failure for a proposed resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContradictionResolutionError {
    /// Winner or loser id was blank.
    BlankMemoryId,
    /// Winner and loser are the same memory (not a contradiction).
    SameMemory,
}

impl ContradictionResolutionError {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlankMemoryId => "blank_memory_id",
            Self::SameMemory => "winner_and_loser_are_same_memory",
        }
    }
}

/// Propose a resolution for a decided contradiction (`winner` kept, `loser`
/// acted on). The kind is chosen from `signal`. Pure: builds a proposal, applies
/// nothing.
#[must_use]
pub fn propose_contradiction_resolution(
    winner_memory_id: &str,
    loser_memory_id: &str,
    signal: ExplicitConflictSignal,
) -> ContradictionResolutionProposal {
    let kind = ContradictionResolutionKind::from_signal(signal);
    let rationale = format!(
        "Contradiction ({}) between {} and {}: propose {} keeping {}.",
        signal.as_str(),
        winner_memory_id.trim(),
        loser_memory_id.trim(),
        kind.as_str(),
        winner_memory_id.trim(),
    );
    ContradictionResolutionProposal {
        winner_memory_id: winner_memory_id.trim().to_string(),
        loser_memory_id: loser_memory_id.trim().to_string(),
        kind,
        signal,
        rationale,
    }
}

/// Validate a proposed resolution before it becomes a curation candidate.
///
/// # Errors
///
/// Returns [`ContradictionResolutionError`] if either memory id is blank or the
/// winner and loser are the same memory.
pub fn validate_contradiction_resolution(
    proposal: &ContradictionResolutionProposal,
) -> Result<(), ContradictionResolutionError> {
    if proposal.winner_memory_id.is_empty() || proposal.loser_memory_id.is_empty() {
        return Err(ContradictionResolutionError::BlankMemoryId);
    }
    if proposal.winner_memory_id == proposal.loser_memory_id {
        return Err(ContradictionResolutionError::SameMemory);
    }
    Ok(())
}

/// Bridge a validated resolution to a *pending* curate candidate input, so the
/// existing curate pipeline performs the audited apply. The candidate targets the
/// loser (the memory superseded/edited/merged); the winner is recorded as the
/// `source_id`. Status is `pending` — never auto-applied (ADR-0014: confirmed
/// proposal only).
#[must_use]
pub fn to_curation_candidate_input(
    proposal: &ContradictionResolutionProposal,
    workspace_id: &str,
) -> CreateCurationCandidateInput {
    CreateCurationCandidateInput {
        workspace_id: workspace_id.to_string(),
        candidate_type: proposal.kind.candidate_type().as_str().to_string(),
        target_memory_id: Some(proposal.loser_memory_id.clone()),
        proposed_content: None,
        proposed_confidence: None,
        proposed_trust_class: None,
        source_type: CONTRADICTION_RESOLUTION_SOURCE_TYPE.to_string(),
        source_id: Some(proposal.winner_memory_id.clone()),
        reason: proposal.rationale.clone(),
        confidence: CONTRADICTION_RESOLUTION_PROPOSAL_CONFIDENCE,
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
        CONTRADICTION_RESOLUTION_SOURCE_TYPE, ContradictionResolutionError,
        ContradictionResolutionKind, propose_contradiction_resolution, to_curation_candidate_input,
        validate_contradiction_resolution,
    };
    use crate::core::contradiction_detect::ExplicitConflictSignal;
    use crate::curate::CandidateType;

    #[test]
    fn signal_maps_to_resolution_kind_and_candidate_type() {
        assert_eq!(
            ContradictionResolutionKind::from_signal(ExplicitConflictSignal::ContradictionLink),
            ContradictionResolutionKind::Supersede
        );
        assert_eq!(
            ContradictionResolutionKind::from_signal(ExplicitConflictSignal::DuplicateDivergent),
            ContradictionResolutionKind::Merge
        );
        assert_eq!(
            ContradictionResolutionKind::from_signal(ExplicitConflictSignal::ValidityWindowOverlap),
            ContradictionResolutionKind::ScopeSplit
        );
        // Kinds bridge to the curate CandidateType that performs the audited apply.
        assert_eq!(
            ContradictionResolutionKind::Supersede.candidate_type(),
            CandidateType::Supersede
        );
        assert_eq!(
            ContradictionResolutionKind::ScopeSplit.candidate_type(),
            CandidateType::Split
        );
        assert_eq!(
            ContradictionResolutionKind::Merge.candidate_type(),
            CandidateType::Merge
        );
    }

    #[test]
    fn proposal_keeps_winner_targets_loser_and_validates() {
        let proposal = propose_contradiction_resolution(
            "  mem_winner  ",
            "mem_loser",
            ExplicitConflictSignal::ContradictionLink,
        );
        assert_eq!(proposal.winner_memory_id, "mem_winner");
        assert_eq!(proposal.loser_memory_id, "mem_loser");
        assert_eq!(proposal.kind, ContradictionResolutionKind::Supersede);
        assert!(validate_contradiction_resolution(&proposal).is_ok());
        assert!(proposal.rationale.contains("supersede"));
    }

    #[test]
    fn validation_rejects_blank_and_self_contradiction() {
        let blank = propose_contradiction_resolution(
            "",
            "mem_loser",
            ExplicitConflictSignal::ContradictionLink,
        );
        assert_eq!(
            validate_contradiction_resolution(&blank),
            Err(ContradictionResolutionError::BlankMemoryId)
        );
        let same = propose_contradiction_resolution(
            "mem_x",
            "mem_x",
            ExplicitConflictSignal::ContradictionLink,
        );
        assert_eq!(
            validate_contradiction_resolution(&same),
            Err(ContradictionResolutionError::SameMemory)
        );
    }

    #[test]
    fn bridges_to_pending_curate_candidate_targeting_loser() {
        let proposal = propose_contradiction_resolution(
            "mem_winner",
            "mem_loser",
            ExplicitConflictSignal::DuplicateDivergent,
        );
        let input = to_curation_candidate_input(&proposal, "wsp_test");
        // bd-jkgta: applies through a content-free tombstone-with-pointer, not
        // a content-requiring merge/split/supersede.
        assert_eq!(input.candidate_type, "tombstone");
        assert_eq!(input.target_memory_id.as_deref(), Some("mem_loser"));
        assert_eq!(input.source_id.as_deref(), Some("mem_winner"));
        assert_eq!(input.source_type, CONTRADICTION_RESOLUTION_SOURCE_TYPE);
        // Never auto-applied: it is a pending candidate awaiting curate accept.
        assert_eq!(input.status.as_deref(), Some("pending"));
    }

    #[test]
    fn curate_candidate_uses_content_free_type_so_accept_can_apply_bd_jkgta() {
        // bd-jkgta: every signal/kind must map to a CandidateType whose
        // requires_content() is false, so the pending candidate (proposed_content
        // None) does not hit the curate `content_required_for_type` branch and can
        // actually be confirmed by `ee curate accept`.
        for signal in [
            ExplicitConflictSignal::ContradictionLink,
            ExplicitConflictSignal::Supersession,
            ExplicitConflictSignal::TrustOutcomeSplit,
            ExplicitConflictSignal::RepeatedCoSelection,
            ExplicitConflictSignal::DuplicateDivergent,
            ExplicitConflictSignal::ValidityWindowOverlap,
        ] {
            let proposal = propose_contradiction_resolution("mem_winner", "mem_loser", signal);
            let candidate_type = proposal.kind.candidate_type();
            assert!(
                !candidate_type.requires_content(),
                "candidate_type {candidate_type:?} for signal {signal:?} must be content-free"
            );
            let input = to_curation_candidate_input(&proposal, "wsp_test");
            assert_eq!(input.proposed_content, None);
            assert_eq!(input.target_memory_id.as_deref(), Some("mem_loser"));
            assert_eq!(input.source_id.as_deref(), Some("mem_winner"));
        }
    }
}
