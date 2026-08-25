//! Contract checks for the `ee.query_assist.v1` payload embedded by weak
//! search and ask responses.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_query_assist_schema() -> Result<Value, String> {
    let text = fs::read_to_string(repo_root().join("docs/schemas/ee.query_assist.v1.json"))
        .map_err(|error| format!("read query assist schema: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse query assist schema: {error}"))
}

fn sample_search_query_assist() -> Value {
    serde_json::json!({
        "schema": "ee.query_assist.v1",
        "mode": "explain",
        "weakResultReason": "no_relevant_results",
        "candidateCount": 1,
        "droppedBelowFloor": 1,
        "relevanceFloor": 0.05,
        "reformulations": [{
            "query": "installer validation smoke release",
            "strategy": "nearest_memory_terms",
            "rationale": "Adds salient terms from a semantically near memory that was below the relevance floor.",
            "matchedDocId": "mem_installer_smoke",
            "matchedMemoryId": "mem_installer_smoke"
        }],
        "didYouMean": [{
            "docId": "mem_installer_smoke",
            "memoryId": "mem_installer_smoke",
            "score": 0.02,
            "relevanceScore": 0.02,
            "scoreKind": "cosine_similarity",
            "source": "semantic_fast",
            "candidateStatus": "below_relevance_floor",
            "content": "Use installer live smoke validation before publishing release artifacts.",
            "why": "Selected by semantic_fast retrieval with score 0.0200.",
            "provenance": []
        }],
        "captureTemplate": {
            "level": "semantic",
            "kind": "note",
            "tags": ["query-gap", "search-miss"],
            "content": "TODO: capture memory needed for search query: installer validation",
            "command": "ee remember --level semantic --kind note --tags query-gap,search-miss --json 'TODO: capture memory needed for search query: installer validation'",
            "rationale": "Capture this missing demand explicitly so ee learn gaps can cluster repeated misses."
        }
    })
}

fn assert_required_fields(payload: &Value) -> TestResult {
    for field in [
        "schema",
        "mode",
        "weakResultReason",
        "reformulations",
        "didYouMean",
        "captureTemplate",
    ] {
        if payload.get(field).is_none() {
            return Err(format!(
                "query assist payload missing required field `{field}`"
            ));
        }
    }
    Ok(())
}

#[test]
fn query_assist_schema_declares_required_contract() -> TestResult {
    let schema = read_query_assist_schema()?;
    assert_eq!(schema["title"], "ee.query_assist.v1");
    assert_eq!(
        schema["properties"]["schema"]["const"],
        "ee.query_assist.v1"
    );
    let required = schema["required"]
        .as_array()
        .ok_or_else(|| "schema required must be an array".to_string())?;
    for field in [
        "schema",
        "mode",
        "weakResultReason",
        "reformulations",
        "didYouMean",
        "captureTemplate",
    ] {
        if !required.iter().any(|value| value == field) {
            return Err(format!("schema required[] missing `{field}`"));
        }
    }
    Ok(())
}

#[test]
fn representative_query_assist_payload_matches_contract_shape() -> TestResult {
    let payload = sample_search_query_assist();
    assert_required_fields(&payload)?;
    assert_eq!(payload["schema"], "ee.query_assist.v1");
    assert_eq!(payload["mode"], "explain");
    assert_eq!(
        payload["didYouMean"][0]["candidateStatus"],
        "below_relevance_floor"
    );
    assert!(
        payload["captureTemplate"]["command"]
            .as_str()
            .is_some_and(|command| command.starts_with("ee remember "))
    );
    Ok(())
}
