//! Exclusion-path coverage for `explain_why_not_selected` (bd-1n0np.1.5).
//!
//! `tests/contracts/why_not_selected_schema.rs` covers the token-budget
//! (authoritative) and not_retrieved (reconstructed) paths. This file covers the
//! distinct pre-selection EXCLUSION branch: when a candidate was retrieved but
//! filtered out by scope / redaction / validity-window / policy / filter, the
//! report must name the specific authoritative reason (never reconstructed) and
//! surface the exclusion in `redactionScopeExclusions`.

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    ContextPackProfile, PackCandidate, PackCandidateInput, PackProvenance, PackSection,
    TokenBudget, WhyNotSelectedInput, WhyNotSelectionExclusion, WhyNotSelectionExclusionKind,
    explain_why_not_selected,
};
use serde_json::Value;
use uuid::Uuid;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn score(value: f32) -> Result<UnitScore, String> {
    UnitScore::parse(value).map_err(|error| format!("{error:?}"))
}

fn candidate(seed: u128, content: &str) -> Result<PackCandidate, String> {
    let uri = "file://AGENTS.md#L1"
        .parse::<ProvenanceUri>()
        .map_err(|error| format!("{error:?}"))?;
    let provenance =
        PackProvenance::new(uri, "source evidence").map_err(|error| format!("{error:?}"))?;
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section: PackSection::ProceduralRules,
        content: content.to_string(),
        estimated_tokens: 20,
        relevance: score(0.9)?,
        utility: score(0.5)?,
        provenance: vec![provenance],
        why: format!("memory {seed} matched the task"),
    })
    .map_err(|error| format!("{error:?}"))
}

fn excluded_report_json(kind: WhyNotSelectionExclusionKind, code: &str) -> Result<Value, String> {
    // The target was retrieved (present in the candidate universe) but excluded
    // by a pre-selection filter — the authoritative exclusion path.
    let target = candidate(2, "target memory present but filtered out")?;
    let other = candidate(1, "another candidate memory")?;
    let report = explain_why_not_selected(
        WhyNotSelectedInput::new(
            "prepare release",
            target.clone(),
            TokenBudget::new(200).map_err(|error| format!("{error:?}"))?,
            ContextPackProfile::Compact,
            vec![other, target],
        )
        .with_exclusions(vec![WhyNotSelectionExclusion::new(
            kind,
            code,
            "excluded for exclusion-path coverage",
            None,
        )]),
    )
    .map_err(|error| format!("{error:?}"))?;
    serde_json::to_value(&report).map_err(|error| error.to_string())
}

fn assert_authoritative_exclusion(
    kind: WhyNotSelectionExclusionKind,
    code: &str,
    expected_reason: &str,
) -> TestResult {
    let json = excluded_report_json(kind, code)?;
    ensure(
        json["primaryReason"] == expected_reason,
        format!(
            "expected primaryReason {expected_reason}, got {}",
            json["primaryReason"]
        ),
    )?;
    ensure(
        json["reasonSource"] == "authoritative",
        format!(
            "exclusion reasons must be authoritative, got {}",
            json["reasonSource"]
        ),
    )?;
    ensure(
        json["retrievalStageReached"] == "candidate_filter",
        format!(
            "exclusion stage must be candidate_filter, got {}",
            json["retrievalStageReached"]
        ),
    )?;
    ensure(
        json["selected"] == false,
        "an excluded memory is not selected",
    )?;
    let exclusions = json["redactionScopeExclusions"]
        .as_array()
        .ok_or_else(|| "redactionScopeExclusions must be an array".to_string())?;
    ensure(
        exclusions.iter().any(|entry| entry["code"] == code),
        "the exclusion must be surfaced in redactionScopeExclusions",
    )
}

#[test]
fn scope_exclusion_is_authoritative() -> TestResult {
    assert_authoritative_exclusion(
        WhyNotSelectionExclusionKind::Scope,
        "scope_filtered",
        "excluded_by_scope",
    )
}

#[test]
fn redaction_exclusion_is_authoritative() -> TestResult {
    assert_authoritative_exclusion(
        WhyNotSelectionExclusionKind::Redaction,
        "redaction_filtered",
        "excluded_by_redaction",
    )
}

#[test]
fn validity_window_exclusion_is_authoritative() -> TestResult {
    assert_authoritative_exclusion(
        WhyNotSelectionExclusionKind::ValidityWindow,
        "validity_filtered",
        "excluded_by_validity_window",
    )
}

#[test]
fn policy_exclusion_is_authoritative() -> TestResult {
    assert_authoritative_exclusion(
        WhyNotSelectionExclusionKind::Policy,
        "policy_filtered",
        "excluded_by_policy",
    )
}
