//! Counterfactual-hint ("minimal change to include") coverage for
//! `explain_why_not_selected` (bd-1n0np.1.5).
//!
//! The reverse-`why` headline feature is the counterfactual hint: the smallest
//! change that would flip a memory from excluded to included. The schema test
//! only checks the field exists; these tests assert the actual hint *kind* per
//! exclusion cause, so the "minimal change" guidance can't silently regress.

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    ContextPackProfile, PackCandidate, PackCandidateInput, PackProvenance, PackSection,
    TokenBudget, WhyNotSelectedInput, explain_why_not_selected,
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

fn candidate(
    seed: u128,
    relevance: f32,
    tokens: u32,
    content: &str,
) -> Result<PackCandidate, String> {
    let uri = "file://AGENTS.md#L1"
        .parse::<ProvenanceUri>()
        .map_err(|error| format!("{error:?}"))?;
    let provenance =
        PackProvenance::new(uri, "source evidence").map_err(|error| format!("{error:?}"))?;
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section: PackSection::ProceduralRules,
        content: content.to_string(),
        estimated_tokens: tokens,
        relevance: score(relevance)?,
        utility: score(0.5)?,
        provenance: vec![provenance],
        why: format!("memory {seed} matched the task"),
    })
    .map_err(|error| format!("{error:?}"))
}

fn hint_kinds(json: &Value) -> Vec<String> {
    json["counterfactualHints"]
        .as_array()
        .map(|hints| {
            hints
                .iter()
                .filter_map(|hint| hint["kind"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn token_budget_omission_suggests_raising_the_budget() -> TestResult {
    let selected = candidate(1, 1.0, 20, "format before release")?;
    let target = candidate(2, 0.95, 20, "target reaches selection but does not fit")?;
    let report = explain_why_not_selected(WhyNotSelectedInput::new(
        "prepare release",
        target.clone(),
        TokenBudget::new(60).map_err(|error| format!("{error:?}"))?,
        ContextPackProfile::Compact,
        vec![selected, target],
    ))
    .map_err(|error| format!("{error:?}"))?;
    let json = serde_json::to_value(&report).map_err(|error| error.to_string())?;

    ensure(
        json["primaryReason"] == "omitted_by_token_budget",
        format!(
            "fixture should omit by token budget, got {}",
            json["primaryReason"]
        ),
    )?;
    let kinds = hint_kinds(&json);
    ensure(
        kinds.iter().any(|kind| kind == "raise_token_budget"),
        format!("budget omission must suggest raising the token budget; got {kinds:?}"),
    )
}

#[test]
fn not_retrieved_target_suggests_repairing_retrieval() -> TestResult {
    let selected = candidate(1, 1.0, 20, "format before release")?;
    let target = candidate(2, 0.95, 20, "target absent from the candidate universe")?;
    let report = explain_why_not_selected(WhyNotSelectedInput::new(
        "prepare release",
        target,
        TokenBudget::new(120).map_err(|error| format!("{error:?}"))?,
        ContextPackProfile::Compact,
        vec![selected],
    ))
    .map_err(|error| format!("{error:?}"))?;
    let json = serde_json::to_value(&report).map_err(|error| error.to_string())?;

    ensure(
        json["primaryReason"] == "not_retrieved",
        format!(
            "absent target should report not_retrieved, got {}",
            json["primaryReason"]
        ),
    )?;
    let kinds = hint_kinds(&json);
    ensure(
        kinds.iter().any(|kind| kind == "repair_retrieval"),
        format!("a retrieval miss must suggest repairing retrieval; got {kinds:?}"),
    )
}
