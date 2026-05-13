//! N5.1 tests for ADR 0031's selectionCertificate -> selectionAudit rename.

#![allow(clippy::expect_used)]

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::{
    ContextJsonRenderOptions, render_context_response_compact, render_context_response_hook,
    render_context_response_human, render_context_response_json,
    render_context_response_json_with_options, render_context_response_jsonl,
    render_context_response_markdown, render_context_response_mermaid,
    render_context_response_toon,
};
use ee::pack::{
    ContextPackProfile, ContextRequest, ContextResponse, PackDraft, PackDraftItem, PackProvenance,
    PackSection, PackSelectedItem, PackSelectionAudit, PackSelectionObjective, PackSelectionPhase,
    PackSelectionStep, PackTrustSignal, TokenBudget,
};
use serde_json::{Value, json};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, String>;

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn score(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("test score should be valid")
}

fn response() -> ContextResponse {
    let memory_id = memory_id(1);
    let item = PackDraftItem {
        rank: 1,
        memory_id,
        section: PackSection::ProceduralRules,
        content: "Run cargo fmt --check before release.".to_owned(),
        estimated_tokens: 8,
        relevance: score(0.91),
        utility: score(0.72),
        provenance: vec![
            PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "test provenance")
                .expect("provenance should be valid"),
        ],
        why: "selected by N5.1 selection audit fixture".to_owned(),
        diversity_key: Some("formatting".to_owned()),
        trust: PackTrustSignal::new(TrustClass::AgentAssertion, None),
        redactions: Vec::new(),
        tombstoned_at: None,
        lifecycle: None,
        selected_in: PackSelectionPhase::StrictMmr,
    };
    let selected_items = vec![PackSelectedItem {
        rank: 1,
        memory_id,
        token_cost: 8,
        feasible: true,
    }];
    let steps = vec![PackSelectionStep {
        rank: 1,
        memory_id,
        marginal_gain: 0.91,
        objective_value: 0.91,
        token_cost: 8,
        feasible: true,
        covered_features: vec!["section:procedural_rules".to_owned()],
    }];
    let draft = PackDraft {
        query: "prepare release".to_owned(),
        budget: TokenBudget::new(400).expect("budget should be valid"),
        used_tokens: 8,
        items: vec![item],
        omitted: Vec::new(),
        selection_audit: PackSelectionAudit {
            profile: ContextPackProfile::Balanced,
            objective: PackSelectionObjective::MmrRedundancy,
            algorithm_id: "mmr_with_coverage_fill_v1",
            algorithm_description: "Deterministic MMR ranking with coverage-fill.",
            candidate_count: 1,
            selected_count: 1,
            omitted_count: 0,
            budget_limit: 400,
            budget_used: 8,
            total_objective_value: 0.91,
            monotone: false,
            submodular: false,
            selected_items,
            steps,
        },
        hash: Some("pack_n5_fixture".to_owned()),
    };
    let request = ContextRequest::from_query("prepare release").expect("request should be valid");
    ContextResponse::new(request, draft, Vec::new()).expect("response should be valid")
}

#[test]
fn json_uses_selection_audit_and_drops_guarantee_status() -> TestResult {
    let rendered = render_context_response_json(&response());
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;
    let pack = value
        .pointer("/data/pack")
        .ok_or_else(|| "pack missing".to_owned())?;
    let audit = pack
        .get("selectionAudit")
        .ok_or_else(|| "selectionAudit missing".to_owned())?;

    assert_eq!(pack.get("selectionCertificate"), None);
    assert_eq!(audit["algorithmId"], json!("mmr_with_coverage_fill_v1"));
    assert!(
        audit["algorithmDescription"]
            .as_str()
            .is_some_and(|description| !description.is_empty())
    );
    assert_eq!(audit.get("guaranteeStatus"), None);
    assert_eq!(audit.get("guarantee"), None);
    assert_eq!(
        value.pointer("/data/pack/meta/algorithm/algorithmId"),
        Some(&json!("mmr_with_coverage_fill_v1"))
    );
    assert_eq!(
        value.pointer("/data/pack/meta/algorithm/guaranteeStatus"),
        None
    );

    Ok(())
}

#[test]
fn legacy_selection_certificate_requires_explicit_option() -> TestResult {
    let options = ContextJsonRenderOptions {
        include_legacy_selection_certificate: true,
        ..ContextJsonRenderOptions::default()
    };
    let rendered = render_context_response_json_with_options(&response(), options);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;

    assert_eq!(
        value.pointer("/data/pack/deprecation/deprecatedField"),
        Some(&json!("selectionCertificate"))
    );
    assert_eq!(
        value.pointer("/data/pack/deprecation/replacementField"),
        Some(&json!("selectionAudit"))
    );
    assert_eq!(
        value.pointer("/data/pack/selectionCertificate/algorithmId"),
        Some(&json!("mmr_with_coverage_fill_v1"))
    );
    assert_eq!(
        value.pointer("/data/pack/selectionCertificate/guaranteeStatus"),
        None
    );

    Ok(())
}

#[test]
fn default_renderers_do_not_emit_old_field_name() {
    let response = response();
    let outputs = [
        render_context_response_json(&response),
        render_context_response_markdown(&response),
        render_context_response_toon(&response),
        render_context_response_jsonl(&response),
        render_context_response_compact(&response),
        render_context_response_hook(&response),
        render_context_response_mermaid(&response),
        render_context_response_human(&response),
    ];

    for output in outputs {
        assert!(
            !output.contains("selectionCertificate"),
            "renderer emitted old field name: {output}"
        );
        assert!(
            !output.contains("guaranteeStatus"),
            "renderer emitted removed guarantee status: {output}"
        );
    }
}

#[test]
fn pack_schema_requires_selection_audit() {
    let schema = include_str!("../docs/schemas/ee.pack.v2.json");

    assert!(schema.contains("\"selectionAudit\""));
    assert!(!schema.contains("\"selectionCertificate\""));
}
