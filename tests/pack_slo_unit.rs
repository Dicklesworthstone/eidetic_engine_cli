#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::render_context_response_json;
use ee::pack::{
    ContextPackProfile, ContextRequest, ContextResponse, PACK_ASSEMBLY_BUDGET_EXCEEDED_CODE,
    PACK_ASSEMBLY_SLO_SCHEMA_V1, PACK_ASSEMBLY_SLOW_CODE, PACK_CONCURRENT_LIMIT_REACHED_CODE,
    PackAssemblyOptions, PackAssemblySlo, PackAssemblySloActuals, PackAssemblySloStatus,
    PackCandidate, PackCandidateInput, PackProvenance, PackResourceProfile, PackSection,
    PackTrustSignal, TokenBudget, assemble_draft_with_profile_and_options,
};
use serde_json::Value;
use uuid::Uuid;

type TestResult = Result<(), String>;

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("fixture score parses")
}

fn candidate(seed: u128, content: &str) -> PackCandidate {
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section: PackSection::ProceduralRules,
        content: content.to_owned(),
        estimated_tokens: 8,
        relevance: unit(0.9),
        utility: unit(0.8),
        provenance: vec![
            PackProvenance::new(
                ProvenanceUri::from_str("file://tests/pack_slo.md").expect("fixture URI parses"),
                "S4 pack SLO fixture",
            )
            .expect("fixture provenance constructs"),
        ],
        why: "selected because it matches resource-aware context assembly".to_owned(),
    })
    .expect("fixture candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("fixture".to_owned()),
    ))
}

fn actuals(
    scanned_count: usize,
    graph_edges_traversed: usize,
    elapsed_ms: u64,
) -> PackAssemblySloActuals {
    PackAssemblySloActuals {
        candidate_count: scanned_count,
        scanned_count,
        index_generation: Some(8),
        graph_generation: Some(8),
        graph_edges_traversed,
        elapsed_ms,
        memory_bytes_peak: 12_000,
    }
}

#[test]
fn resource_profile_budget_table_matches_s4_contract() -> TestResult {
    let lean = PackResourceProfile::Lean.budget_class();
    assert_eq!(lean.candidates_scanned_max, 80);
    assert_eq!(lean.graph_traversal_max_edges, 1_024);
    assert_eq!(lean.elapsed_ms_target, 50);
    assert_eq!(lean.elapsed_ms_warning, 100);
    assert_eq!(lean.elapsed_ms_failure, 200);
    assert_eq!(lean.concurrent_pack_max, 1);

    let standard = PackResourceProfile::Standard.budget_class();
    assert_eq!(standard.candidates_scanned_max, 240);
    assert_eq!(standard.graph_traversal_max_edges, 8_192);
    assert_eq!(standard.elapsed_ms_target, 200);
    assert_eq!(standard.elapsed_ms_warning, 500);
    assert_eq!(standard.elapsed_ms_failure, 2_000);
    assert_eq!(standard.concurrent_pack_max, 4);

    let swarm_heavy = PackResourceProfile::SwarmHeavy.budget_class();
    assert_eq!(swarm_heavy.candidates_scanned_max, 1_600);
    assert_eq!(swarm_heavy.graph_traversal_max_edges, 65_536);
    assert_eq!(swarm_heavy.elapsed_ms_target, 1_000);
    assert_eq!(swarm_heavy.elapsed_ms_warning, 2_000);
    assert_eq!(swarm_heavy.elapsed_ms_failure, 10_000);
    assert_eq!(swarm_heavy.concurrent_pack_max, 16);

    assert_eq!(
        "swarm-heavy".parse::<PackResourceProfile>().unwrap(),
        PackResourceProfile::SwarmHeavy
    );
    assert_eq!(
        "swarm_heavy".parse::<PackResourceProfile>().unwrap(),
        PackResourceProfile::SwarmHeavy
    );
    Ok(())
}

#[test]
fn pack_concurrent_limit_code_is_stable_for_j6_fixture() {
    assert_eq!(
        PACK_CONCURRENT_LIMIT_REACHED_CODE,
        "pack_concurrent_limit_reached"
    );
}

#[test]
fn pack_slo_warns_when_concurrent_limit_is_reached() {
    let slo =
        PackAssemblySlo::concurrent_limit_reached(PackResourceProfile::Lean, actuals(0, 0, 1), 250);
    assert_eq!(slo.status, PackAssemblySloStatus::Warning);
    assert_eq!(slo.degradations.len(), 1);
    assert_eq!(slo.degradations[0].code, PACK_CONCURRENT_LIMIT_REACHED_CODE);
    assert_eq!(slo.degradations[0].severity.as_str(), "low");
    assert!(slo.degradations[0].message.contains("Concurrent pack"));
    assert!(
        slo.degradations[0]
            .repair
            .as_deref()
            .is_some_and(|repair| repair.contains("retry"))
    );
    assert_eq!(
        slo.context_degradations()[0].code,
        PACK_CONCURRENT_LIMIT_REACHED_CODE
    );
}

#[test]
fn pack_slo_reports_within_budget_without_degradations() {
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::Lean, actuals(12, 0, 10));
    assert_eq!(slo.schema, PACK_ASSEMBLY_SLO_SCHEMA_V1);
    assert_eq!(slo.status, PackAssemblySloStatus::WithinBudget);
    assert!(slo.degradations.is_empty());
    assert!(slo.context_degradations().is_empty());
}

#[test]
fn pack_slo_warns_when_profile_scan_limit_is_hit() {
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::Lean, actuals(80, 0, 20));
    assert_eq!(slo.status, PackAssemblySloStatus::Warning);
    assert_eq!(slo.degradations.len(), 1);
    assert_eq!(slo.degradations[0].code, PACK_ASSEMBLY_SLOW_CODE);
    assert_eq!(slo.context_degradations()[0].code, PACK_ASSEMBLY_SLOW_CODE);
}

#[test]
fn pack_slo_reports_elapsed_time_without_changing_status() {
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::Lean, actuals(20, 0, 100));
    assert_eq!(slo.status, PackAssemblySloStatus::WithinBudget);
    assert!(slo.degradations.is_empty());
    assert_eq!(slo.actuals.elapsed_ms, 100);
}

#[test]
fn pack_slo_fails_when_graph_budget_is_exceeded() {
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::Lean, actuals(20, 1_025, 20));
    assert_eq!(slo.status, PackAssemblySloStatus::Failure);
    assert_eq!(slo.degradations.len(), 1);
    assert_eq!(slo.degradations[0].code, PACK_ASSEMBLY_BUDGET_EXCEEDED_CODE);
    assert_eq!(
        slo.context_degradations()[0].code,
        PACK_ASSEMBLY_BUDGET_EXCEEDED_CODE
    );
}

#[test]
fn pack_slo_does_not_fail_on_elapsed_time_alone() {
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::Lean, actuals(20, 0, 200));
    assert_eq!(slo.status, PackAssemblySloStatus::WithinBudget);
    assert!(slo.degradations.is_empty());
    assert_eq!(slo.actuals.elapsed_ms, 200);
}

#[test]
fn pack_slo_status_is_deterministic_across_repeated_inputs() {
    let cases = [
        (
            PackResourceProfile::Lean,
            actuals(79, 1_023, 99),
            PackAssemblySloStatus::WithinBudget,
        ),
        (
            PackResourceProfile::Standard,
            actuals(240, 8_192, 499),
            PackAssemblySloStatus::Warning,
        ),
        (
            PackResourceProfile::SwarmHeavy,
            actuals(1_601, 65_536, 999),
            PackAssemblySloStatus::Failure,
        ),
    ];

    for (profile, actuals, expected) in cases {
        let statuses = (0..3)
            .map(|_| PackAssemblySlo::evaluate(profile, actuals).status)
            .collect::<Vec<_>>();
        assert_eq!(statuses, vec![expected; 3]);
    }
}

#[test]
fn context_json_renders_pack_slo_surface() -> TestResult {
    let query = "resource-aware pack assembly";
    let request = ContextRequest::from_query(query).map_err(|error| error.to_string())?;
    let budget = TokenBudget::new(400).map_err(|error| error.to_string())?;
    let mut draft = assemble_draft_with_profile_and_options(
        ContextPackProfile::Balanced,
        query,
        budget,
        vec![candidate(
            1,
            "Keep pack assembly bounded for large workspaces.",
        )],
        PackAssemblyOptions::default(),
    )
    .map_err(|error| error.to_string())?;
    draft.hash = Some("blake3:s4-pack-slo-fixture".to_owned());
    let actuals = PackAssemblySloActuals::from_pack_run(&draft, 1, 0, 1);
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::SwarmHeavy, actuals);
    let mut response =
        ContextResponse::new(request, draft, Vec::new()).map_err(|error| error.to_string())?;
    response.data.slo = Some(slo);

    let json: Value = serde_json::from_str(&render_context_response_json(&response))
        .map_err(|error| error.to_string())?;
    assert_eq!(
        json.pointer("/data/pack/slo/schema"),
        Some(&Value::String(PACK_ASSEMBLY_SLO_SCHEMA_V1.to_owned()))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/profile"),
        Some(&Value::String("swarm_heavy".to_owned()))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/status"),
        Some(&Value::String("within_budget".to_owned()))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/budgetClass/concurrentPackMax"),
        Some(&Value::from(16))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/actuals/scannedCount"),
        Some(&Value::from(1))
    );
    Ok(())
}
