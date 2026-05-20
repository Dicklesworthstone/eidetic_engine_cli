//! Contract coverage for `ee.why_not_selected.v1`.

use std::fs;
use std::path::PathBuf;

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    ContextPackProfile, PackCandidate, PackCandidateInput, PackProvenance, PackSection,
    TokenBudget, WHY_NOT_SELECTED_SCHEMA_V1, WhyNotSelectedInput, explain_why_not_selected,
};
use serde_json::Value;
use uuid::Uuid;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.why_not_selected.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.why_not_selected.v1.json";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn load_schema() -> Result<Value, String> {
    let path = repo_root().join(SCHEMA_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn score(value: f32) -> Result<UnitScore, String> {
    UnitScore::parse(value).map_err(|error| format!("{error:?}"))
}

fn provenance() -> Result<PackProvenance, String> {
    let uri = "file://AGENTS.md#L1"
        .parse::<ProvenanceUri>()
        .map_err(|error| format!("{error:?}"))?;
    PackProvenance::new(uri, "source evidence").map_err(|error| format!("{error:?}"))
}

fn candidate(
    seed: u128,
    relevance: f32,
    tokens: u32,
    content: &str,
) -> Result<PackCandidate, String> {
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section: PackSection::ProceduralRules,
        content: content.to_string(),
        estimated_tokens: tokens,
        relevance: score(relevance)?,
        utility: score(0.5)?,
        provenance: vec![provenance()?],
        why: format!("memory {seed} matches the task"),
    })
    .map_err(|error| format!("{error:?}"))
}

#[test]
fn why_not_selected_schema_pins_read_only_counterfactual_fields() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["$id"] == SCHEMA_ID,
        format!("expected schema id {SCHEMA_ID}, got {}", schema["$id"]),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == WHY_NOT_SELECTED_SCHEMA_V1,
        "schema const should match Rust surface",
    )?;
    let required = schema["required"]
        .as_array()
        .ok_or_else(|| "schema required must be an array".to_string())?;
    for field in [
        "memoryId",
        "taskHash",
        "primaryReason",
        "filtersApplied",
        "tokenBudgetFrontier",
        "counterfactualHints",
        "provenance",
    ] {
        ensure(
            required.iter().any(|value| value.as_str() == Some(field)),
            format!("schema required fields should include {field}"),
        )?;
    }
    ensure(
        schema["properties"].get("task").is_none(),
        "schema must expose taskHash, not raw task text",
    )?;
    ensure(
        schema["properties"].get("content").is_none(),
        "schema must not expose raw memory content",
    )
}

#[test]
fn why_not_selected_report_matches_schema_and_omits_memory_content() -> TestResult {
    let selected = candidate(1, 1.0, 20, "format before release")?;
    let target = candidate(2, 0.95, 20, "secret_token=why-not-contract-fixture")?;
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
        json["schema"] == WHY_NOT_SELECTED_SCHEMA_V1,
        "report schema should match",
    )?;
    ensure(
        json["taskHash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "report should expose a blake3 task hash",
    )?;
    ensure(
        json["primaryReason"] == "omitted_by_token_budget",
        "token-budget fixture should explain budget omission",
    )?;
    ensure(
        !json.to_string().contains("why-not-contract-fixture"),
        "report JSON must not leak raw memory content",
    )
}
