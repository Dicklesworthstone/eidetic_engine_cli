//! S3 tests for coordination-aware context packs.

#![allow(clippy::expect_used)]

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::{render_context_response_json, render_context_response_markdown};
use ee::pack::{
    ContextRequest, ContextResponse, PackCoordinationSnapshot, PackDraft, PackDraftItem,
    PackProvenance, PackSection, PackSelectedItem, PackSelectionAudit, PackSelectionObjective,
    PackSelectionPhase, PackSelectionStep, PackTrustSignal, TokenBudget,
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

fn base_response() -> ContextResponse {
    let memory_id = memory_id(42);
    let item = PackDraftItem {
        rank: 1,
        memory_id,
        section: PackSection::ProceduralRules,
        content: "Coordinate before editing reserved files.".to_owned(),
        estimated_tokens: 8,
        relevance: score(0.91),
        utility: score(0.72),
        proximity_to_seed: None,
        score_breakdown: None,
        provenance: vec![
            PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "coordination fixture")
                .expect("provenance should be valid"),
        ],
        why: "selected by S3 coordination fixture".to_owned(),
        diversity_key: Some("coordination".to_owned()),
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
        query: "prepare shared edit".to_owned(),
        budget: TokenBudget::new(400).expect("budget should be valid"),
        used_tokens: 8,
        items: vec![item],
        omitted: Vec::new(),
        selection_audit: PackSelectionAudit {
            profile: ee::pack::ContextPackProfile::Balanced,
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
        hash: Some("pack_s3_fixture".to_owned()),
    };
    let request =
        ContextRequest::from_query("prepare shared edit").expect("request should be valid");
    ContextResponse::new(request, draft, Vec::new()).expect("response should be valid")
}

fn response_with_snapshot(snapshot: &str) -> TestResult<ContextResponse> {
    let mut response = base_response();
    response.data.coordination = Some(PackCoordinationSnapshot::from_json_str(
        snapshot,
        ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
    )?);
    Ok(response)
}

#[test]
fn fresh_coordination_snapshot_renders_in_json_and_markdown() -> TestResult {
    let response = response_with_snapshot(
        r#"{
          "schema": "ee.coordination_snapshot.v1",
          "captured_at": "2026-05-13T10:00:00Z",
          "scope": "workspace",
          "sources": [
            {
              "kind": "beads_ready",
              "source_id": "br ready --json",
              "freshness_ms": 1000,
              "entries": [
                {
                  "kind": "bead",
                  "id": "bd-123",
                  "status": "in_progress",
                  "summary": "Implement coordination-aware packs"
                }
              ]
            }
          ]
        }"#,
    )?;
    let rendered = render_context_response_json(&response);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;

    assert_eq!(
        value.pointer("/data/pack/coordination/schema"),
        Some(&json!("ee.coordination_snapshot.v1"))
    );
    assert_eq!(
        value.pointer("/data/pack/coordination/freshness/status"),
        Some(&json!("fresh"))
    );
    assert_eq!(
        value.pointer("/data/pack/coordination/summary/inProgressBeadCount"),
        Some(&json!(1))
    );

    let markdown = render_context_response_markdown(&response);
    assert!(markdown.contains("## Coordination"));
    assert!(markdown.contains("bd-123"));
    Ok(())
}

#[test]
fn stale_and_conflicting_coordination_snapshot_is_compact_but_loud() -> TestResult {
    let response = response_with_snapshot(
        r#"{
          "schema": "ee.coordination_snapshot.v1",
          "captured_at": "2026-05-13T10:00:00Z",
          "scope": "workspace",
          "sources": [
            {
              "kind": "file_reservation",
              "source_id": "agent-mail snapshot",
              "freshness_ms": 90000000,
              "entries": [
                {
                  "path_pattern": "src/pack/**",
                  "holder": "BlueLake",
                  "exclusive": true,
                  "conflict": true
                }
              ]
            }
          ]
        }"#,
    )?;
    let rendered = render_context_response_json(&response);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;

    assert_eq!(
        value.pointer("/data/pack/coordination/freshness/status"),
        Some(&json!("stale"))
    );
    assert_eq!(
        value.pointer("/data/pack/coordination/summary/staleSourceCount"),
        Some(&json!(1))
    );
    assert_eq!(
        value.pointer("/data/pack/coordination/summary/activeConflictCount"),
        Some(&json!(1))
    );
    let markdown = render_context_response_markdown(&response);
    assert!(markdown.contains("**Conflict:**"));
    assert!(markdown.contains("exclusive reservation on src/pack/"));
    assert!(markdown.contains("BlueLake"));
    Ok(())
}

#[test]
fn unavailable_coordination_source_is_preserved_with_repair_hint() -> TestResult {
    let snapshot =
        include_str!("fixtures/coordination_snapshots/coordination_source_unavailable.json");
    let response = response_with_snapshot(snapshot)?;
    let rendered = render_context_response_json(&response);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;

    assert_eq!(
        value.pointer("/data/pack/coordination/freshness/status"),
        Some(&json!("degraded"))
    );
    assert_eq!(
        value.pointer("/data/pack/coordination/summary/unavailableSourceCount"),
        Some(&json!(1))
    );
    assert_eq!(
        value.pointer("/data/pack/coordination/sources/0/degraded/0/code"),
        Some(&json!("agent_mail_unavailable"))
    );
    Ok(())
}

#[test]
fn absent_coordination_snapshot_omits_pack_section() -> TestResult {
    let response = base_response();
    let rendered = render_context_response_json(&response);
    let value: Value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;

    assert_eq!(value.pointer("/data/pack/coordination"), None);
    assert!(!render_context_response_markdown(&response).contains("## Coordination"));
    Ok(())
}

#[test]
fn empty_coordination_snapshot_is_deterministic() -> TestResult {
    let first = PackCoordinationSnapshot::from_json_str(
        r#"{"schema":"ee.coordination_snapshot.v1","scope":"workspace","sources":[]}"#,
        ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
    )?;
    let second = PackCoordinationSnapshot::from_json_str(
        r#"{"schema":"ee.coordination_snapshot.v1","scope":"workspace","sources":[]}"#,
        ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
    )?;

    assert_eq!(first, second);
    assert_eq!(first.summary.source_count, 0);
    assert_eq!(first.freshness.status, "fresh");
    Ok(())
}
