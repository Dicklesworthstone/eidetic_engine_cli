//! Schema round-trip contract test (eidetic_engine_cli-6e05d).
//!
//! For every ee.*.v1 schema this test exercises, the contract is:
//!
//!   render() -> JSON string s1
//!   serde_json::from_str(s1) -> Value v
//!   serde_json::to_string(v) -> JSON string s2
//!   serde_json::from_str(s2) -> Value v2 == v
//!
//! In other words: the rendered JSON survives an out-and-back parse-and-
//! reserialize cycle with structural equivalence. We do NOT require
//! byte-identity because serde_json::Value uses a sorted-key BTreeMap
//! internally (no preserve_order feature in this project), so the
//! re-serialization always emits alphabetically. The structural
//! equivalence (`from_str(s2) == v`) is the real invariant.
//!
//! We also assert:
//! - the rendered string is itself valid JSON (parses without error);
//! - the top-level `schema` field has the documented value;
//! - calling the same renderer twice produces byte-identical output
//!   (determinism); some renderers fold non-deterministic system state
//!   like timestamps into the response, so a single fixture build is
//!   measured before two consecutive renders.
//!
//! Where a renderer requires a workspace on disk, that schema is left
//! to the existing JSON snapshot tests in tests/json_contract_snapshots.rs
//! — this contract focuses on the schemas we can stand up purely in
//! memory: context pack response (ee.response.v1 with command=context),
//! `ee why` response, and `ee health` response.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::{render_context_response_json, render_health_json};
use ee::pack::{
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
    PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use serde_json::Value;
use uuid::Uuid;

type TestResult = Result<(), String>;

fn assert_roundtrip(label: &str, rendered: &str, expected_schema: &str) -> TestResult {
    let parsed: Value = serde_json::from_str(rendered)
        .map_err(|error| format!("{label}: rendered JSON did not parse: {error}"))?;

    let schema = parsed
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label}: response is missing top-level `schema` field"))?;
    if schema != expected_schema {
        return Err(format!(
            "{label}: top-level schema mismatch (got {schema:?}, expected {expected_schema:?})"
        ));
    }

    let reserialized = serde_json::to_string(&parsed)
        .map_err(|error| format!("{label}: failed to re-serialize parsed value: {error}"))?;
    let reparsed: Value = serde_json::from_str(&reserialized)
        .map_err(|error| format!("{label}: re-serialized JSON did not re-parse: {error}"))?;
    if parsed != reparsed {
        return Err(format!(
            "{label}: structural equivalence broken across parse/reserialize/reparse"
        ));
    }

    let reserialized_again = serde_json::to_string(&reparsed)
        .map_err(|error| format!("{label}: failed second re-serialize: {error}"))?;
    if reserialized != reserialized_again {
        return Err(format!(
            "{label}: re-serialization is not idempotent (Value -> String drifts on a second \
             call)"
        ));
    }
    Ok(())
}

fn assert_renderer_is_deterministic<F>(label: &str, render: F) -> TestResult
where
    F: Fn() -> String,
{
    let first = render();
    let second = render();
    if first != second {
        return Err(format!(
            "{label}: renderer is not deterministic — two consecutive calls produced \
             different output:\n  first:  {first}\n  second: {second}"
        ));
    }
    Ok(())
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("unit score in [0,1]")
}

fn provenance(uri: &str) -> PackProvenance {
    PackProvenance::new(
        ProvenanceUri::from_str(uri).expect("provenance URI parses"),
        "test evidence",
    )
    .expect("pack provenance constructs")
}

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn context_response_fixture() -> ContextResponse {
    let request =
        ContextRequest::from_query("schema round-trip target").expect("request query accepts");
    let budget = TokenBudget::new(2000).expect("budget accepts");
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(0x6e05d),
        section: PackSection::ProceduralRules,
        content: "Run cargo fmt --check before release.".to_owned(),
        estimated_tokens: 12,
        relevance: unit(0.8),
        utility: unit(0.6),
        provenance: vec![provenance("file://AGENTS.md#L42")],
        why: "release prep needs formatting guardrail".to_owned(),
    })
    .expect("candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("project-rule".to_owned()),
    ));
    let draft = assemble_draft(&request.query, budget, vec![candidate]).expect("draft assembles");
    ContextResponse::new(request, draft, Vec::new()).expect("response constructs")
}

// ============================================================================
// Context pack response (ee.response.v1, command=context)
// ============================================================================

#[test]
fn context_response_json_roundtrips() -> TestResult {
    let response = context_response_fixture();
    let rendered = render_context_response_json(&response);
    assert_roundtrip("context_response_json", &rendered, "ee.response.v1")
}

#[test]
fn context_response_json_is_deterministic() -> TestResult {
    let response = context_response_fixture();
    assert_renderer_is_deterministic("context_response_json", || {
        render_context_response_json(&response)
    })
}

// ============================================================================
// Health response (ee.response.v1, command=health)
// ============================================================================
//
// HealthReport::gather() probes the live host. The probe results vary by
// machine but the *shape* of the response is stable, which is what we
// exercise here. We do NOT assert against pinned content — only that the
// envelope parses, has the expected top-level schema, and round-trips.

#[test]
fn health_response_json_roundtrips() -> TestResult {
    let report = ee::core::health::HealthReport::gather();
    let rendered = render_health_json(&report);
    assert_roundtrip("health_response_json", &rendered, "ee.response.v1")
}

#[test]
fn health_response_json_is_deterministic_for_one_probe() -> TestResult {
    // The health report itself is probed once. Two render() calls against
    // the same gathered report must produce byte-identical output.
    let report = ee::core::health::HealthReport::gather();
    assert_renderer_is_deterministic("health_response_json", || render_health_json(&report))
}

// ============================================================================
// Error envelope (ee.error.v2)
// ============================================================================

#[test]
fn error_envelope_json_roundtrips() -> TestResult {
    use ee::models::DomainError;
    use ee::output::error_response_json;

    let error = DomainError::NotFound {
        resource: "memory".to_owned(),
        id: "mem_doesnotexist".to_owned(),
        repair: Some("ee memory list".to_owned()),
    };
    let rendered = error_response_json(&error);
    assert_roundtrip("error_envelope_json", &rendered, "ee.error.v2")
}

#[test]
fn error_envelope_json_is_deterministic() -> TestResult {
    use ee::models::DomainError;
    use ee::output::error_response_json;

    let error = DomainError::Storage {
        message: "disk pressure".to_owned(),
        repair: Some("ee doctor".to_owned()),
    };
    assert_renderer_is_deterministic("error_envelope_json", || error_response_json(&error))
}

// ============================================================================
// Cross-renderer property: every renderer's output is valid JSON whose
// top-level keys are a stable superset.
// ============================================================================

#[test]
fn every_response_envelope_has_top_level_schema_field() -> TestResult {
    let response = context_response_fixture();
    let context_rendered = render_context_response_json(&response);

    let health_report = ee::core::health::HealthReport::gather();
    let health_rendered = render_health_json(&health_report);

    for (label, json_str) in [
        ("context_response_json", &context_rendered),
        ("health_response_json", &health_rendered),
    ] {
        let parsed: Value = serde_json::from_str(json_str)
            .map_err(|error| format!("{label}: rendered JSON did not parse: {error}"))?;
        if parsed.get("schema").and_then(Value::as_str).is_none() {
            return Err(format!(
                "{label}: response envelope is missing required top-level `schema` field"
            ));
        }
        // The `success` field is part of ee.response.v1's envelope contract.
        if parsed.get("success").and_then(Value::as_bool).is_none() {
            return Err(format!(
                "{label}: response envelope is missing required top-level `success` field"
            ));
        }
    }
    Ok(())
}
