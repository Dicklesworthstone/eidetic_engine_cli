#![allow(clippy::expect_used)]

use ee::core::profile::{OperatingProfile, RuntimeProfileReport};
use ee::core::search::{ScoreSource, SearchHit, SearchReport, SearchSourceMode, SearchStatus};
use ee::models::{MemoryId, MemoryScope, MemoryScopeStats, ProvenanceUri, TrustClass, UnitScore};
use ee::output::{render_context_response_json, render_context_response_markdown};
use ee::pack::{
    ConflictKind, ConflictRecommendedAction, ContextPackProfile, ContextRequest, ContextResponse,
    PackDraft, PackDraftItem, PackItemLifecycle, PackProvenance, PackSection, PackSelectedItem,
    PackSelectionAudit, PackSelectionObjective, PackSelectionPhase, PackSelectionStep,
    PackTrustSignal, TokenBudget, analyze_pack_consensus_conflicts,
};
use std::time::Instant;
use uuid::Uuid;

type TestResult = Result<(), String>;

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn score(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("test score should be valid")
}

fn lifecycle(valid_from: &str) -> PackItemLifecycle {
    PackItemLifecycle {
        validity_status: "current".to_string(),
        validity_window_kind: "starts_at".to_string(),
        valid_from: Some(valid_from.to_string()),
        valid_to: None,
    }
}

fn item(
    seed: u128,
    rank: u32,
    content: &str,
    subject: &str,
    trust_class: TrustClass,
    trust_subclass: &str,
    valid_from: Option<&str>,
) -> PackDraftItem {
    let memory_id = memory_id(seed);
    PackDraftItem {
        rank,
        memory_id,
        section: PackSection::ProceduralRules,
        content: content.to_string(),
        estimated_tokens: 8,
        relevance: score(0.9),
        utility: score(0.7),
        provenance: vec![
            PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "test evidence")
                .expect("provenance should be valid"),
        ],
        why: "selected by consensus/conflict unit fixture".to_string(),
        diversity_key: Some(subject.to_string()),
        trust: PackTrustSignal::new(trust_class, Some(trust_subclass.to_string())),
        redactions: Vec::new(),
        tombstoned_at: None,
        lifecycle: valid_from.map(lifecycle),
        selected_in: PackSelectionPhase::StrictMmr,
    }
}

fn draft(items: Vec<PackDraftItem>) -> PackDraft {
    let selected_items = items
        .iter()
        .map(|item| PackSelectedItem {
            rank: item.rank,
            memory_id: item.memory_id,
            token_cost: item.estimated_tokens,
            feasible: true,
        })
        .collect::<Vec<_>>();
    let steps = items
        .iter()
        .map(|item| PackSelectionStep {
            rank: item.rank,
            memory_id: item.memory_id,
            marginal_gain: 0.5,
            objective_value: 0.5,
            token_cost: item.estimated_tokens,
            feasible: true,
            covered_features: vec!["s8".to_string()],
        })
        .collect::<Vec<_>>();

    PackDraft {
        query: "release prep".to_string(),
        budget: TokenBudget::new(400).expect("budget should be valid"),
        used_tokens: items.iter().map(|item| item.estimated_tokens).sum(),
        selection_audit: PackSelectionAudit {
            profile: ContextPackProfile::Balanced,
            objective: PackSelectionObjective::MmrRedundancy,
            algorithm_id: "test",
            algorithm_description: "Test-only consensus/conflict selection audit.",
            candidate_count: items.len(),
            selected_count: items.len(),
            omitted_count: 0,
            budget_limit: 400,
            budget_used: items.iter().map(|item| item.estimated_tokens).sum(),
            total_objective_value: 1.0,
            monotone: false,
            submodular: false,
            selected_items,
            steps,
        },
        items,
        omitted: Vec::new(),
        hash: None,
    }
}

fn response_with_analysis(pack: PackDraft) -> ContextResponse {
    let request = ContextRequest::from_query(&pack.query).expect("request should be valid");
    let analysis = analyze_pack_consensus_conflicts(&pack);
    let mut response = ContextResponse::new(request, pack, Vec::new())
        .expect("response should match request query");
    response.data.consensus = analysis.consensus;
    response.data.conflicts = analysis.conflicts;
    response
}

fn search_hit(
    seed: u128,
    content: &str,
    subject_tag: &str,
    trust_class: TrustClass,
    producer: &str,
) -> SearchHit {
    let memory_id = memory_id(seed);
    SearchHit {
        doc_id: memory_id.to_string(),
        score: 0.9,
        source: ScoreSource::Hybrid,
        fast_score: None,
        quality_score: None,
        lexical_score: None,
        rerank_score: None,
        metadata: Some(serde_json::json!({
            "_ee_analysis_content": content,
            "_ee_analysis_utility": 0.7,
            "_ee_analysis_created_at": "2026-05-13T00:00:00Z",
            "level": "procedural",
            "kind": "rule",
            "tags": subject_tag,
            "trust_class": trust_class.as_str(),
            "producerAgent": producer,
            "provenanceUri": format!("ee-mem://{memory_id}")
        })),
        explanation: None,
    }
}

fn search_report(results: Vec<SearchHit>) -> SearchReport {
    SearchReport {
        status: SearchStatus::Success,
        query: "release prep".to_string(),
        requested_limit: 10,
        results,
        elapsed_ms: 1.0,
        errors: Vec::new(),
        degraded: Vec::new(),
        runtime_profile: RuntimeProfileReport::for_profile(
            OperatingProfile::Workstation,
            "s8-test",
        ),
        relevance_floor_applied: Some(0.005),
        candidates_below_floor: 0,
        source_mode_requested: SearchSourceMode::Hybrid,
        source_mode_applied: SearchSourceMode::Hybrid,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
    }
}

#[test]
fn agreement_cluster_surfaces_consensus() -> TestResult {
    let report = analyze_pack_consensus_conflicts(&draft(vec![
        item(
            1,
            1,
            "Run cargo fmt before release.",
            "release:fmt",
            TrustClass::AgentValidated,
            "agent-a",
            Some("2026-05-01T00:00:00Z"),
        ),
        item(
            2,
            2,
            "Run cargo fmt before release.",
            "release:fmt",
            TrustClass::HumanExplicit,
            "human",
            Some("2026-05-02T00:00:00Z"),
        ),
        item(
            3,
            3,
            "Run cargo fmt before release.",
            "release:fmt",
            TrustClass::AgentAssertion,
            "agent-c",
            Some("2026-05-03T00:00:00Z"),
        ),
    ]));

    assert_eq!(report.conflicts.len(), 0);
    assert_eq!(report.consensus.len(), 1);
    let consensus = &report.consensus[0];
    assert!(consensus.agreement_score >= 0.85);
    assert_eq!(consensus.member_memory_ids.len(), 3);
    assert_eq!(
        consensus.first_recorded_at.as_deref(),
        Some("2026-05-01T00:00:00Z")
    );
    assert_eq!(
        consensus.last_reinforced_at.as_deref(),
        Some("2026-05-03T00:00:00Z")
    );
    Ok(())
}

#[test]
fn direct_trust_mismatch_recommends_promote_one() -> TestResult {
    let report = analyze_pack_consensus_conflicts(&draft(vec![
        item(
            4,
            1,
            "Always use HTTPS for callbacks.",
            "transport:https",
            TrustClass::HumanExplicit,
            "human",
            None,
        ),
        item(
            5,
            2,
            "Never use HTTPS for callbacks.",
            "transport:https",
            TrustClass::AgentAssertion,
            "agent-b",
            None,
        ),
    ]));

    assert_eq!(report.consensus.len(), 0);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, ConflictKind::Direct);
    assert_eq!(
        report.conflicts[0].recommended_action,
        ConflictRecommendedAction::PromoteOne
    );
    Ok(())
}

#[test]
fn stale_replacement_prefers_tombstone_older_action() -> TestResult {
    let report = analyze_pack_consensus_conflicts(&draft(vec![
        item(
            6,
            1,
            "Use API v1 for release uploads.",
            "release:api",
            TrustClass::AgentValidated,
            "agent-a",
            Some("2025-01-01T00:00:00Z"),
        ),
        item(
            7,
            2,
            "Use API v2 for release uploads.",
            "release:api",
            TrustClass::AgentValidated,
            "agent-b",
            Some("2026-04-01T00:00:00Z"),
        ),
    ]));

    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, ConflictKind::StaleReplacement);
    assert_eq!(
        report.conflicts[0].recommended_action,
        ConflictRecommendedAction::TombstoneOlder
    );
    Ok(())
}

#[test]
fn partial_overlap_is_visible_but_not_consensus() -> TestResult {
    let report = analyze_pack_consensus_conflicts(&draft(vec![
        item(
            8,
            1,
            "Run cargo fmt before release.",
            "release:checks",
            TrustClass::AgentAssertion,
            "agent-a",
            None,
        ),
        item(
            9,
            2,
            "Run cargo check before release.",
            "release:checks",
            TrustClass::AgentAssertion,
            "agent-b",
            None,
        ),
    ]));

    assert_eq!(report.consensus.len(), 0);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, ConflictKind::PartialOverlap);
    assert_eq!(
        report.conflicts[0].recommended_action,
        ConflictRecommendedAction::Review
    );
    Ok(())
}

#[test]
fn irrelevant_conflict_is_omitted_by_query_selected_items() -> TestResult {
    let report = analyze_pack_consensus_conflicts(&draft(vec![item(
        10,
        1,
        "Run cargo fmt before release.",
        "release:fmt",
        TrustClass::HumanExplicit,
        "human",
        None,
    )]));

    assert!(report.consensus.is_empty());
    assert!(report.conflicts.is_empty());
    Ok(())
}

#[test]
fn analysis_is_deterministic_for_same_pack() -> TestResult {
    let pack = draft(vec![
        item(
            11,
            1,
            "Always use HTTPS for callbacks.",
            "transport:https",
            TrustClass::HumanExplicit,
            "human",
            None,
        ),
        item(
            12,
            2,
            "Never use HTTPS for callbacks.",
            "transport:https",
            TrustClass::AgentAssertion,
            "agent-b",
            None,
        ),
    ]);

    let first = analyze_pack_consensus_conflicts(&pack);
    let second = analyze_pack_consensus_conflicts(&pack);
    let third = analyze_pack_consensus_conflicts(&pack);
    assert_eq!(first, second);
    assert_eq!(second, third);
    Ok(())
}

#[test]
fn context_json_renders_consensus_and_conflicts() -> TestResult {
    let response = response_with_analysis(draft(vec![
        item(
            13,
            1,
            "Always use HTTPS for callbacks.",
            "transport:https",
            TrustClass::HumanExplicit,
            "human",
            None,
        ),
        item(
            14,
            2,
            "Never use HTTPS for callbacks.",
            "transport:https",
            TrustClass::AgentAssertion,
            "agent-b",
            None,
        ),
    ]));
    let json: serde_json::Value = serde_json::from_str(&render_context_response_json(&response))
        .map_err(|error| error.to_string())?;

    assert_eq!(json["data"]["consensus"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["data"]["conflicts"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["data"]["conflicts"][0]["schema"], "ee.conflict.v1");
    assert_eq!(json["data"]["conflicts"][0]["kind"], "direct");
    assert_eq!(
        json["data"]["conflicts"][0]["recommendedAction"],
        "promote_one"
    );
    Ok(())
}

#[test]
fn search_json_renders_conflicts_without_public_internal_metadata() -> TestResult {
    let report = search_report(vec![
        search_hit(
            17,
            "Always use HTTPS for callbacks.",
            "transport:https",
            TrustClass::HumanExplicit,
            "human",
        ),
        search_hit(
            18,
            "Never use HTTPS for callbacks.",
            "transport:https",
            TrustClass::AgentAssertion,
            "agent-b",
        ),
    ]);
    let json = report.data_json();

    assert_eq!(json["consensus"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["conflicts"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["conflicts"][0]["kind"], "direct");
    assert_eq!(json["conflicts"][0]["recommendedAction"], "promote_one");
    assert!(
        json["results"][0]["metadata"]
            .as_object()
            .is_some_and(|metadata| !metadata.contains_key("_ee_analysis_content"))
    );
    Ok(())
}

#[test]
fn consensus_analysis_1k_items_completes_under_slo() -> TestResult {
    let mut items = Vec::with_capacity(1_000);
    for index in 0..1_000_u128 {
        let group = index / 10;
        items.push(item(
            1_000 + index,
            u32::try_from(index + 1).map_err(|error| error.to_string())?,
            &format!("Run cargo fmt before release group {group}."),
            &format!("release:fmt:{group}"),
            TrustClass::AgentValidated,
            "agent-slo",
            Some("2026-05-13T00:00:00Z"),
        ));
    }
    let pack = draft(items);

    let started = Instant::now();
    let report = analyze_pack_consensus_conflicts(&pack);
    let elapsed = started.elapsed();

    assert_eq!(report.conflicts.len(), 0);
    assert_eq!(report.consensus.len(), 100);
    assert!(
        elapsed.as_millis() < 500,
        "consensus analysis should stay below 500ms for 1K selected items, got {elapsed:?}"
    );
    Ok(())
}

#[test]
fn context_markdown_includes_concise_conflict_notes() -> TestResult {
    let response = response_with_analysis(draft(vec![
        item(
            15,
            1,
            "Always use HTTPS for callbacks.",
            "transport:https",
            TrustClass::HumanExplicit,
            "human",
            None,
        ),
        item(
            16,
            2,
            "Never use HTTPS for callbacks.",
            "transport:https",
            TrustClass::AgentAssertion,
            "agent-b",
            None,
        ),
    ]));
    let markdown = render_context_response_markdown(&response);

    assert!(markdown.contains("## Consensus and Conflicts"));
    assert!(markdown.contains("`direct`"));
    assert!(markdown.contains("`promote_one`"));
    Ok(())
}
