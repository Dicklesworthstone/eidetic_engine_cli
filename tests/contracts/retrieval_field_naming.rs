//! Contract test for retrieval JSON field naming consistency (EE-FIELD-NAMING-001).
//!
//! Verifies that context, search, and why commands use consistent camelCase field
//! naming in JSON output. This prevents field name drift between retrieval commands.
//!
//! Acceptance criteria from eidetic_engine_cli-fbmq:
//! - All retrieval JSON fields use camelCase
//! - No snake_case fields appear in context/search/why machine output
//! - Field naming is stable across commands

use ee::core::search::{RetrievalMetrics, ScoreSource, SearchHit, SearchReport, SearchStatus};
use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn contains_snake_case_key(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key.contains('_') {
                    return Some(format!("{path}.{key}"));
                }
                if let Some(found) = contains_snake_case_key(child, &format!("{path}.{key}")) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for (i, child) in arr.iter().enumerate() {
                if let Some(found) = contains_snake_case_key(child, &format!("{path}[{i}]")) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

#[test]
fn search_report_data_json_uses_camel_case_fields() -> TestResult {
    let report = SearchReport {
        status: SearchStatus::Success,
        query: "test query".to_string(),
        requested_limit: 10,
        results: vec![SearchHit {
            doc_id: "mem_001".to_string(),
            score: 0.9,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.8),
            quality_score: Some(0.95),
            lexical_score: Some(0.7),
            rerank_score: None,
            metadata: Some(serde_json::json!({"level": "procedural"})),
            explanation: None,
        }],
        elapsed_ms: 12.5,
        errors: Vec::new(),
    };

    let json = report.data_json();

    if let Some(bad_key) = contains_snake_case_key(&json, "search") {
        return Err(format!(
            "SearchReport::data_json contains snake_case key: {bad_key}"
        ));
    }

    // Verify expected camelCase keys exist
    ensure(json.get("resultCount").is_some(), "missing resultCount")?;
    ensure(json.get("elapsedMs").is_some(), "missing elapsedMs")?;
    ensure(
        json["results"][0].get("docId").is_some(),
        "missing results[].docId",
    )?;
    ensure(
        json["results"][0].get("fastScore").is_some(),
        "missing results[].fastScore",
    )?;
    ensure(
        json["results"][0].get("qualityScore").is_some(),
        "missing results[].qualityScore",
    )?;
    ensure(
        json["results"][0].get("lexicalScore").is_some(),
        "missing results[].lexicalScore",
    )?;

    Ok(())
}

#[test]
fn retrieval_metrics_data_json_uses_camel_case_fields() -> TestResult {
    let hits = vec![
        SearchHit {
            doc_id: "doc1".to_string(),
            score: 0.9,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.8),
            quality_score: Some(0.95),
            lexical_score: Some(0.7),
            rerank_score: None,
            metadata: None,
            explanation: None,
        },
        SearchHit {
            doc_id: "doc2".to_string(),
            score: 0.7,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.7),
            rerank_score: None,
            metadata: None,
            explanation: None,
        },
    ];

    let metrics = RetrievalMetrics::from_hits(10, 5.5, &hits, 0);
    let json = metrics.data_json();

    if let Some(bad_key) = contains_snake_case_key(&json, "metrics") {
        return Err(format!(
            "RetrievalMetrics::data_json contains snake_case key: {bad_key}"
        ));
    }

    // Verify expected camelCase keys exist
    ensure(json.get("requestedLimit").is_some(), "missing requestedLimit")?;
    ensure(json.get("returnedCount").is_some(), "missing returnedCount")?;
    ensure(json.get("errorCount").is_some(), "missing errorCount")?;
    ensure(json.get("elapsedMs").is_some(), "missing elapsedMs")?;
    ensure(json.get("sourceCounts").is_some(), "missing sourceCounts")?;
    ensure(
        json["sourceCounts"].get("semanticFast").is_some(),
        "missing sourceCounts.semanticFast",
    )?;
    ensure(
        json["sourceCounts"].get("semanticQuality").is_some(),
        "missing sourceCounts.semanticQuality",
    )?;
    ensure(
        json.get("scoreDistribution").is_some(),
        "missing scoreDistribution",
    )?;
    ensure(json.get("fieldCoverage").is_some(), "missing fieldCoverage")?;
    ensure(
        json["fieldCoverage"].get("fastScoreCount").is_some(),
        "missing fieldCoverage.fastScoreCount",
    )?;
    ensure(
        json["fieldCoverage"].get("qualityScoreCount").is_some(),
        "missing fieldCoverage.qualityScoreCount",
    )?;
    ensure(
        json["fieldCoverage"].get("lexicalScoreCount").is_some(),
        "missing fieldCoverage.lexicalScoreCount",
    )?;
    ensure(
        json["fieldCoverage"].get("rerankScoreCount").is_some(),
        "missing fieldCoverage.rerankScoreCount",
    )?;

    Ok(())
}

#[test]
fn search_hit_fields_are_camel_case_when_populated() -> TestResult {
    let report = SearchReport {
        status: SearchStatus::Success,
        query: "test".to_string(),
        requested_limit: 5,
        results: vec![SearchHit {
            doc_id: "mem_full".to_string(),
            score: 0.85,
            source: ScoreSource::Reranked,
            fast_score: Some(0.7),
            quality_score: Some(0.9),
            lexical_score: Some(0.6),
            rerank_score: Some(0.88),
            metadata: Some(serde_json::json!({"kind": "rule", "tags": ["release"]})),
            explanation: Some(ee::core::search::ScoreExplanation {
                summary: "hybrid score".to_string(),
                factors: vec![ee::core::search::ScoreFactor {
                    name: "bm25".to_string(),
                    value: 0.6,
                    contribution: "0.3".to_string(),
                    source_field: "content".to_string(),
                    formula: "tf * idf".to_string(),
                }],
            }),
        }],
        elapsed_ms: 3.0,
        errors: Vec::new(),
    };

    let json = report.data_json();
    let hit = &json["results"][0];

    // Ensure rerank score is camelCase
    ensure(hit.get("rerankScore").is_some(), "missing rerankScore")?;

    // Ensure explanation factors use camelCase
    let factor = &hit["explanation"]["factors"][0];
    ensure(
        factor.get("sourceField").is_some(),
        "missing explanation.factors[].sourceField",
    )?;

    if let Some(bad_key) = contains_snake_case_key(&json, "search") {
        return Err(format!("snake_case key found: {bad_key}"));
    }

    Ok(())
}

#[test]
fn field_naming_contract_is_stable() -> TestResult {
    // This test documents the expected camelCase field names for search output.
    // If these assertions fail, the field naming contract has drifted.
    let expected_search_fields = [
        "command",
        "status",
        "query",
        "results",
        "resultCount",
        "elapsedMs",
        "metrics",
        "errors",
    ];

    let expected_hit_fields = [
        "docId",
        "score",
        "source",
        "fastScore",
        "qualityScore",
        "lexicalScore",
        "rerankScore",
        "metadata",
        "explanation",
    ];

    let expected_metrics_fields = [
        "requestedLimit",
        "returnedCount",
        "errorCount",
        "elapsedMs",
        "sourceCounts",
        "scoreDistribution",
        "fieldCoverage",
    ];

    // Build a report that exercises all fields
    let report = SearchReport {
        status: SearchStatus::Success,
        query: "naming contract".to_string(),
        requested_limit: 5,
        results: vec![SearchHit {
            doc_id: "contract-doc".to_string(),
            score: 0.9,
            source: ScoreSource::Hybrid,
            fast_score: Some(0.8),
            quality_score: Some(0.9),
            lexical_score: Some(0.7),
            rerank_score: Some(0.85),
            metadata: Some(serde_json::json!({})),
            explanation: Some(ee::core::search::ScoreExplanation {
                summary: "test".to_string(),
                factors: vec![],
            }),
        }],
        elapsed_ms: 1.0,
        errors: Vec::new(),
    };

    let json = report.data_json();

    // Verify top-level search fields
    for field in expected_search_fields {
        ensure(
            json.get(field).is_some(),
            format!("search missing expected field: {field}"),
        )?;
    }

    // Verify required hit fields are present
    // Bug: eidetic_engine_cli-9nw7 - old code was tautological (is_some || is_none always true)
    let hit = &json["results"][0];

    // Required fields must always be present
    let required_hit_fields = ["docId", "score", "source"];
    for field in required_hit_fields {
        ensure(
            hit.get(field).is_some(),
            format!("hit missing required field: {field}"),
        )?;
    }

    // Optional score fields - present when fixture provides them
    // The fixture above sets fastScore, qualityScore, lexicalScore, rerankScore
    let fixture_provided_score_fields = ["fastScore", "qualityScore", "lexicalScore", "rerankScore"];
    for field in fixture_provided_score_fields {
        ensure(
            hit.get(field).is_some(),
            format!("hit missing fixture-provided field: {field}"),
        )?;
    }

    // Optional fields that are set in fixture - metadata and explanation
    ensure(
        hit.get("metadata").is_some(),
        "hit missing fixture-provided metadata",
    )?;
    ensure(
        hit.get("explanation").is_some(),
        "hit missing fixture-provided explanation",
    )?;

    // Verify metrics fields
    let metrics = &json["metrics"];
    for field in expected_metrics_fields {
        ensure(
            metrics.get(field).is_some(),
            format!("metrics missing expected field: {field}"),
        )?;
    }

    Ok(())
}

#[test]
fn optional_hit_fields_absent_when_none() -> TestResult {
    // Bug: eidetic_engine_cli-9nw7
    // Verify optional fields are NOT present in JSON when None in struct.
    // This confirms the contract: optional fields omitted unless populated.
    let report = SearchReport {
        status: SearchStatus::Success,
        query: "minimal hit".to_string(),
        requested_limit: 5,
        results: vec![SearchHit {
            doc_id: "minimal-doc".to_string(),
            score: 0.5,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: None,
            rerank_score: None,
            metadata: None,
            explanation: None,
        }],
        elapsed_ms: 1.0,
        errors: Vec::new(),
    };

    let json = report.data_json();
    let hit = &json["results"][0];

    // Required fields must still be present
    ensure(hit.get("docId").is_some(), "docId must be present")?;
    ensure(hit.get("score").is_some(), "score must be present")?;
    ensure(hit.get("source").is_some(), "source must be present")?;

    // Optional fields must be absent when None
    ensure(
        hit.get("fastScore").is_none(),
        "fastScore should be absent when None",
    )?;
    ensure(
        hit.get("qualityScore").is_none(),
        "qualityScore should be absent when None",
    )?;
    ensure(
        hit.get("lexicalScore").is_none(),
        "lexicalScore should be absent when None",
    )?;
    ensure(
        hit.get("rerankScore").is_none(),
        "rerankScore should be absent when None",
    )?;
    ensure(
        hit.get("metadata").is_none(),
        "metadata should be absent when None",
    )?;
    ensure(
        hit.get("explanation").is_none(),
        "explanation should be absent when None",
    )?;

    Ok(())
}
