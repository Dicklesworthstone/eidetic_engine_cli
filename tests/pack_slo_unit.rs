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

fn assert_published_slo_fields(value: &Value) -> TestResult {
    let schema: Value =
        serde_json::from_str(include_str!("../docs/schemas/swarm/ee.pack.slo.v1.json"))
            .map_err(|error| error.to_string())?;
    for (instance, definition) in [
        (value, &schema),
        (&value["admission"], &schema["properties"]["admission"]),
    ] {
        let object = instance
            .as_object()
            .ok_or_else(|| "rendered SLO/admission must be an object".to_owned())?;
        let properties = definition["properties"]
            .as_object()
            .ok_or_else(|| "published SLO properties missing".to_owned())?;
        assert_eq!(definition["additionalProperties"], false);
        for key in object.keys() {
            assert!(
                properties.contains_key(key),
                "rendered field {key} absent from published SLO schema"
            );
        }
        for required in definition["required"]
            .as_array()
            .ok_or_else(|| "published SLO required fields missing".to_owned())?
        {
            let key = required
                .as_str()
                .ok_or_else(|| "schema field name must be a string".to_owned())?;
            assert!(
                object.contains_key(key),
                "rendered SLO misses required {key}"
            );
        }
    }
    for field in ["resourceStatus", "elapsedStatus", "status"] {
        let vocabulary = schema["properties"][field]["enum"]
            .as_array()
            .ok_or_else(|| format!("published {field} vocabulary missing"))?;
        assert!(
            vocabulary.contains(&value[field]),
            "{field} violates published vocabulary"
        );
    }
    Ok(())
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
    let slo = PackAssemblySlo::concurrent_limit_reached(
        PackResourceProfile::Lean,
        actuals(0, 0, 1),
        250,
        1,
    );
    assert_eq!(slo.status, PackAssemblySloStatus::Warning);
    let admission = slo
        .admission
        .expect("concurrent limit records admission posture");
    assert_eq!(admission.outcome.as_str(), "backoff");
    assert_eq!(admission.queue_depth, 1);
    assert_eq!(admission.concurrent_pack_max, 1);
    assert_eq!(admission.retry_after_ms, Some(250));
    assert_eq!(admission.waited_ms, 0);
    assert_eq!(slo.degradations.len(), 1);
    assert_eq!(slo.degradations[0].code, PACK_CONCURRENT_LIMIT_REACHED_CODE);
    assert_eq!(slo.degradations[0].severity.as_str(), "low");
    assert!(slo.degradations[0].message.contains("Concurrent pack"));
    assert!(slo.degradations[0].message.contains("queue depth 1"));
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
fn pack_slo_warns_at_elapsed_threshold_without_changing_resource_status() {
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::Lean, actuals(20, 0, 100));
    assert_eq!(slo.status, PackAssemblySloStatus::Warning);
    assert_eq!(slo.elapsed_status, PackAssemblySloStatus::Warning);
    assert_eq!(slo.resource_status, PackAssemblySloStatus::WithinBudget);
    assert!(slo.degradations.is_empty());
    assert!(slo.context_degradations().is_empty());
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
fn pack_slo_reports_elapsed_failure_without_degrading_pack_content() {
    let slo = PackAssemblySlo::evaluate(PackResourceProfile::Lean, actuals(20, 0, 200));
    assert_eq!(slo.status, PackAssemblySloStatus::Failure);
    assert_eq!(slo.elapsed_status, PackAssemblySloStatus::Failure);
    assert_eq!(slo.resource_status, PackAssemblySloStatus::WithinBudget);
    assert!(slo.degradations.is_empty());
    assert!(slo.context_degradations().is_empty());
    assert_eq!(slo.actuals.elapsed_ms, 200);
}

#[test]
fn pack_slo_elapsed_threshold_edges_and_resource_precedence() {
    for profile in [
        PackResourceProfile::Lean,
        PackResourceProfile::Standard,
        PackResourceProfile::SwarmHeavy,
    ] {
        let budget = profile.budget_class();
        for (elapsed, expected) in [
            (0, PackAssemblySloStatus::WithinBudget),
            (
                budget.elapsed_ms_target,
                PackAssemblySloStatus::WithinBudget,
            ),
            (
                budget.elapsed_ms_warning - 1,
                PackAssemblySloStatus::WithinBudget,
            ),
            (budget.elapsed_ms_warning, PackAssemblySloStatus::Warning),
            (
                budget.elapsed_ms_warning + 1,
                PackAssemblySloStatus::Warning,
            ),
            (
                budget.elapsed_ms_failure - 1,
                PackAssemblySloStatus::Warning,
            ),
            (budget.elapsed_ms_failure, PackAssemblySloStatus::Failure),
            (
                budget.elapsed_ms_failure + 1,
                PackAssemblySloStatus::Failure,
            ),
            (u64::MAX, PackAssemblySloStatus::Failure),
        ] {
            let slo = PackAssemblySlo::evaluate(profile, actuals(1, 0, elapsed));
            assert_eq!(slo.elapsed_status, expected, "{profile} elapsed={elapsed}");
            assert_eq!(slo.status, expected, "{profile} elapsed={elapsed}");
            assert_eq!(slo.resource_status, PackAssemblySloStatus::WithinBudget);
            assert!(slo.context_degradations().is_empty());
        }
        for elapsed in [0, budget.elapsed_ms_failure] {
            let over = PackAssemblySlo::evaluate(
                profile,
                actuals(budget.candidates_scanned_max + 1, 0, elapsed),
            );
            assert_eq!(over.resource_status, PackAssemblySloStatus::Failure);
            assert_eq!(over.status, PackAssemblySloStatus::Failure);
            let at = PackAssemblySlo::evaluate(
                profile,
                actuals(budget.candidates_scanned_max, 0, elapsed),
            );
            assert_eq!(at.resource_status, PackAssemblySloStatus::Warning);
            assert_eq!(
                at.status,
                if elapsed == 0 {
                    PackAssemblySloStatus::Warning
                } else {
                    PackAssemblySloStatus::Failure
                }
            );
            let backoff = PackAssemblySlo::concurrent_limit_reached(
                profile,
                actuals(0, 0, elapsed),
                250,
                budget.concurrent_pack_max,
            );
            assert_eq!(backoff.resource_status, PackAssemblySloStatus::Warning);
            assert_eq!(backoff.status, at.status);
            assert_eq!(
                backoff.context_degradations()[0].code,
                PACK_CONCURRENT_LIMIT_REACHED_CODE
            );
        }
    }
}

#[test]
fn pack_slo_measured_failure_preserves_signed_resource_evidence_and_cached_producer() -> TestResult
{
    let request = ContextRequest::from_query("resource-aware pack assembly")
        .map_err(|error| error.to_string())?;
    let draft = assemble_draft_with_profile_and_options(
        ContextPackProfile::Balanced,
        request.query.clone(),
        request.budget,
        vec![candidate(
            1,
            "Keep pack assembly bounded for large workspaces.",
        )],
        PackAssemblyOptions::default(),
    )
    .map_err(|error| error.to_string())?;
    for scanned in [12, 240, 241] {
        let fast =
            PackAssemblySlo::evaluate(PackResourceProfile::Standard, actuals(scanned, 0, 18));
        let slow =
            PackAssemblySlo::evaluate(PackResourceProfile::Standard, actuals(scanned, 0, 24_457));
        assert_eq!(slow.status, PackAssemblySloStatus::Failure);
        assert_eq!(fast.resource_status, slow.resource_status);
        assert_eq!(fast.context_degradations(), slow.context_degradations());
        let mut responses = Vec::new();
        for slo in [fast, slow] {
            let degraded = slo.context_degradations();
            let mut measured_draft = draft.clone();
            measured_draft.hash = Some(ee::core::context::compute_pack_hash(
                &request,
                &measured_draft,
                &degraded,
            ));
            let mut response = ContextResponse::new(request.clone(), measured_draft, degraded)
                .map_err(|error| error.to_string())?;
            response.data.slo = Some(slo);
            responses.push(response);
        }
        assert_eq!(responses[0].data.pack.hash, responses[1].data.pack.hash);
        assert_eq!(
            ee::output::render_context_response_markdown(&responses[0]),
            ee::output::render_context_response_markdown(&responses[1])
        );
        let produced = render_context_response_json(&responses[1]);
        let parsed: Value = serde_json::from_str(&produced).map_err(|error| error.to_string())?;
        assert_published_slo_fields(&parsed["data"]["pack"]["slo"])?;
        assert_eq!(
            parsed.pointer("/data/pack/slo/actuals/elapsedMs"),
            Some(&Value::from(24_457))
        );
        assert_eq!(
            parsed
                .pointer("/data/pack/slo/elapsedStatus")
                .and_then(Value::as_str),
            Some("failure")
        );
        assert_eq!(
            parsed
                .pointer("/data/pack/slo/status")
                .and_then(Value::as_str),
            Some("failure")
        );
        let cached = ContextResponse::from_cached_json_with_command(
            request.clone(),
            produced.clone(),
            "pack",
        );
        let cached_json: Value = serde_json::from_str(&render_context_response_json(&cached))
            .map_err(|error| error.to_string())?;
        assert_eq!(
            cached_json, parsed,
            "cached body preserves producer measurement, not current hit timing"
        );
    }
    Ok(())
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
    assert_published_slo_fields(&json["data"]["pack"]["slo"])?;
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
        json.pointer("/data/pack/slo/resourceStatus")
            .and_then(Value::as_str),
        Some("within_budget")
    );
    assert_eq!(
        json.pointer("/data/pack/slo/elapsedStatus")
            .and_then(Value::as_str),
        Some("within_budget")
    );
    assert_eq!(
        json.pointer("/data/pack/slo/budgetClass/concurrentPackMax"),
        Some(&Value::from(16))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/admission/outcome"),
        Some(&Value::String("admitted".to_owned()))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/admission/concurrentPackMax"),
        Some(&Value::from(16))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/admission/waitedMs"),
        Some(&Value::from(0))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/actuals/scannedCount"),
        Some(&Value::from(1))
    );
    Ok(())
}

#[test]
fn context_json_renders_pack_backoff_admission_and_recovery_shape() -> TestResult {
    let query = "resource-aware pack backoff";
    let request = ContextRequest::from_query(query).map_err(|error| error.to_string())?;
    let budget = TokenBudget::new(400).map_err(|error| error.to_string())?;
    let mut draft = assemble_draft_with_profile_and_options(
        ContextPackProfile::Balanced,
        query,
        budget,
        Vec::new(),
        PackAssemblyOptions::default(),
    )
    .map_err(|error| error.to_string())?;
    draft.hash = Some("blake3:s4-pack-backoff-fixture".to_owned());
    let actuals = PackAssemblySloActuals::from_pack_run(&draft, 0, 0, 1);
    let slo = PackAssemblySlo::concurrent_limit_reached(PackResourceProfile::Lean, actuals, 250, 1);
    let degraded = slo.context_degradations();
    let mut response =
        ContextResponse::new(request, draft, degraded).map_err(|error| error.to_string())?;
    response.data.slo = Some(slo);

    let json: Value = serde_json::from_str(&render_context_response_json(&response))
        .map_err(|error| error.to_string())?;
    assert_eq!(
        json.pointer("/data/pack/slo/admission/outcome"),
        Some(&Value::String("backoff".to_owned()))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/admission/queueDepth"),
        Some(&Value::from(1))
    );
    assert_eq!(
        json.pointer("/data/pack/slo/admission/retryAfterMs"),
        Some(&Value::from(250))
    );
    assert_eq!(
        json.pointer("/data/degraded/0/code"),
        Some(&Value::String(
            PACK_CONCURRENT_LIMIT_REACHED_CODE.to_owned()
        ))
    );
    assert_eq!(
        json.pointer("/data/degraded/0/details/recovery/0/kind"),
        Some(&Value::String("narrow".to_owned()))
    );
    assert_eq!(
        json.pointer("/data/degraded/0/details/recovery/1/flagName"),
        Some(&Value::String("--resource-profile".to_owned()))
    );
    assert_eq!(
        json.pointer("/data/degraded/0/details/recovery/2/example"),
        Some(&Value::String(
            "ee cache prewarm --from-hotset latest --profile lean --json".to_owned()
        ))
    );
    Ok(())
}
