//! Gate 13 submodular pack-selection contract coverage.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::output::{
    CardsProfile, pack_budget_card, render_cards_json, render_context_response_json,
    selection_score_card,
};
use ee::pack::{
    ContextPackProfile, ContextRequest, ContextRequestInput, ContextResponse, PackCandidate,
    PackCandidateInput, PackProvenance, PackSection, TokenBudget, assemble_draft_with_profile,
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

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("certificates")
        .join(format!("{name}.json.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!(
            "golden mismatch {}\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ),
    )
}

fn assert_cards_golden(name: &str, actual: &str) -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("cards")
        .join(format!("{name}.json.golden"));
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!(
            "golden mismatch {}\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ),
    )
}

fn pretty(value: &Value) -> Result<String, String> {
    let mut rendered = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    rendered.push('\n');
    Ok(rendered)
}

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn score(value: f32) -> Result<UnitScore, String> {
    UnitScore::parse(value).map_err(|error| format!("{error:?}"))
}

fn provenance() -> Result<PackProvenance, String> {
    let uri = ProvenanceUri::from_str("file://AGENTS.md#L1")
        .map_err(|error| format!("provenance URI rejected: {error:?}"))?;
    PackProvenance::new(uri, "source evidence").map_err(|error| format!("{error:?}"))
}

fn candidate(
    seed: u128,
    relevance: f32,
    utility: f32,
    content: &str,
    diversity_key: &str,
) -> Result<PackCandidate, String> {
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section: PackSection::ProceduralRules,
        content: content.to_string(),
        estimated_tokens: 10,
        relevance: score(relevance)?,
        utility: score(utility)?,
        provenance: vec![provenance()?],
        why: format!("memory {seed} matches release preparation"),
    })
    .map(|candidate| candidate.with_diversity_key(diversity_key))
    .map_err(|error| format!("{error:?}"))
}

pub(crate) fn fixture_candidates() -> Result<Vec<PackCandidate>, String> {
    Ok(vec![
        candidate(
            1,
            0.95,
            0.70,
            "Run cargo fmt --check before release.",
            "formatting",
        )?,
        candidate(
            2,
            0.90,
            0.65,
            "Run cargo clippy --all-targets -- -D warnings.",
            "linting",
        )?,
        candidate(
            3,
            0.72,
            0.80,
            "Verify signed release checksums after packaging.",
            "release-artifacts",
        )?,
    ])
}

fn submodular_draft() -> Result<ee::pack::PackDraft, String> {
    assemble_draft_with_profile(
        ContextPackProfile::Submodular,
        "prepare release",
        TokenBudget::new(100).map_err(|error| format!("{error:?}"))?,
        fixture_candidates()?,
    )
    .map_err(|error| format!("{error:?}"))
}

#[derive(Clone)]
struct TestSignature {
    memory_id: MemoryId,
    diversity_key: Option<String>,
    normalized_content: String,
}

fn signature(candidate: &PackCandidate) -> TestSignature {
    TestSignature {
        memory_id: candidate.memory_id,
        diversity_key: candidate.diversity_key.clone(),
        normalized_content: normalize_content(&candidate.content),
    }
}

fn test_facility_value(selected: &[PackCandidate], universe: &[PackCandidate]) -> f32 {
    if selected.is_empty() {
        return 0.0;
    }
    let signatures: Vec<TestSignature> = selected.iter().map(signature).collect();
    universe
        .iter()
        .map(|candidate| {
            let coverage = signatures
                .iter()
                .map(|sig| facility_similarity(candidate, sig))
                .fold(0.0_f32, f32::max);
            candidate_weight(candidate) * coverage
        })
        .sum()
}

fn candidate_weight(candidate: &PackCandidate) -> f32 {
    (0.70 * candidate.relevance.into_inner()) + (0.30 * candidate.utility.into_inner())
}

fn facility_similarity(candidate: &PackCandidate, selected: &TestSignature) -> f32 {
    if candidate.memory_id == selected.memory_id {
        return 1.0;
    }
    if normalize_content(&candidate.content) == selected.normalized_content {
        return 1.0;
    }
    let mut similarity = 0.0_f32;
    if let Some(diversity_key) = &candidate.diversity_key
        && selected.diversity_key.as_ref() == Some(diversity_key)
    {
        similarity = similarity.max(0.85);
    }
    similarity.max(content_overlap_similarity(
        &candidate.content,
        &selected.normalized_content,
    ))
}

fn content_overlap_similarity(left: &str, right_normalized: &str) -> f32 {
    let left_terms = normalized_terms(left);
    let right_terms = normalized_terms(right_normalized);
    if left_terms.is_empty() || right_terms.is_empty() {
        return 0.0;
    }
    let intersection = left_terms.intersection(&right_terms).count();
    let union = left_terms.union(&right_terms).count();
    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

fn normalized_terms(content: &str) -> BTreeSet<String> {
    content
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
                .to_ascii_lowercase()
        })
        .filter(|term| !term.is_empty())
        .collect()
}

fn normalize_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn pack_selection_certificate_has_gate13_frontier_fields() -> TestResult {
    let draft = submodular_draft()?;
    let certificate = &draft.selection_certificate;
    ensure(
        !certificate.selected_items.is_empty(),
        "selected items present",
    )?;
    ensure(
        !certificate.rejected_frontier.is_empty(),
        "rejected frontier present",
    )?;
    ensure(
        certificate.steps.iter().all(|step| step.token_cost > 0),
        "steps include token costs",
    )?;
    ensure(
        certificate.steps.iter().all(|step| step.feasible),
        "selected steps are feasible",
    )?;
    ensure(
        certificate
            .rejected_frontier
            .iter()
            .all(|item| !item.feasible),
        "rejected frontier records infeasible candidates",
    )?;
    ensure(
        certificate.guarantee_status.as_str() == "conditional",
        "certificate has guarantee status",
    )
}

#[test]
fn pack_selection_certificate_golden_is_stable() -> TestResult {
    let request = ContextRequest::new(ContextRequestInput {
        query: "prepare release".to_string(),
        profile: Some(ContextPackProfile::Submodular),
        max_tokens: Some(100),
        candidate_pool: Some(3),
        sections: vec![PackSection::ProceduralRules],
    })
    .map_err(|error| format!("{error:?}"))?;
    let response = ContextResponse::new(request, submodular_draft()?, Vec::new())
        .map_err(|error| format!("{error:?}"))?;
    let rendered = render_context_response_json(&response);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;
    let certificate = value
        .pointer("/data/pack/selectionCertificate")
        .ok_or_else(|| "selection certificate missing".to_string())?;
    assert_golden("pack_selection", &pretty(certificate)?)
}

#[test]
fn sampled_diminishing_returns_are_recorded_in_certificate_steps() -> TestResult {
    let draft = submodular_draft()?;
    let steps = &draft.selection_certificate.steps;
    ensure(steps.len() >= 2, "need at least two sampled steps")?;
    for window in steps.windows(2) {
        ensure(
            window[1].marginal_gain <= window[0].marginal_gain + 0.000_001,
            format!(
                "diminishing returns: next gain {} should not exceed previous gain {}",
                window[1].marginal_gain, window[0].marginal_gain
            ),
        )?;
    }
    Ok(())
}

#[test]
fn tiny_fixture_greedy_matches_exact_budgeted_subset_optimum() -> TestResult {
    let candidates = fixture_candidates()?;
    let budget = TokenBudget::new(100).map_err(|error| format!("{error:?}"))?;
    let greedy = assemble_draft_with_profile(
        ContextPackProfile::Submodular,
        "prepare release",
        budget,
        candidates.clone(),
    )
    .map_err(|error| format!("{error:?}"))?;

    let token_costs = [10_u32, 10, 10];
    let mut exact_best = 0.0_f32;
    for mask in 0_u8..8 {
        let mut subset = Vec::new();
        let mut tokens = 0_u32;
        for (index, token_cost) in token_costs.iter().copied().enumerate() {
            if (mask >> index) & 1 == 1 {
                tokens = tokens.saturating_add(token_cost);
                subset.push(candidates[index].clone());
            }
        }
        if tokens > 20 {
            continue;
        }
        exact_best = exact_best.max(test_facility_value(&subset, &candidates));
    }

    ensure(
        (greedy.selection_certificate.total_objective_value - exact_best).abs() <= 0.000_001,
        format!(
            "greedy objective {} should equal exact optimum {}",
            greedy.selection_certificate.total_objective_value, exact_best
        ),
    )
}

#[test]
fn math_pack_selection_cards_do_not_change_selected_memories() -> TestResult {
    let draft = submodular_draft()?;
    let selected_before: Vec<String> = draft
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect();
    let cards = vec![
        selection_score_card(0.95, 0.70, 0.80, 0.825),
        pack_budget_card(draft.used_tokens, draft.budget.max_tokens(), 2, 1),
    ];
    let rendered = render_cards_json(&cards, CardsProfile::Math);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;
    let selected_after: Vec<String> = draft
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect();
    ensure(
        selected_before == selected_after,
        "adding math cards does not change selected memories",
    )?;
    for item in value
        .as_array()
        .ok_or_else(|| "cards should render as array".to_string())?
    {
        let math = &item["math"];
        ensure(math["formula"].is_string(), "card has equation")?;
        ensure(
            math["substitutedValues"].is_string(),
            "card has substituted values",
        )?;
        ensure(math["intuition"].is_string(), "card has intuition")?;
        ensure(math["assumptions"].is_array(), "card has assumptions")?;
        ensure(
            math["decisionChange"].is_string(),
            "card has decision change condition",
        )?;
    }
    assert_cards_golden("math_pack_selection", &pretty(&value)?)
}
