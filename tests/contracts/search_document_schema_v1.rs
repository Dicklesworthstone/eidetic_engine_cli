//! bd-34scj: validate real `ee search` result document emissions against
//! `docs/schemas/ee.search.document.v1.json`.
//!
//! The existing `ee.search.v1` coverage validates the command envelope. This
//! contract pins the per-result `data.results[]` object produced by
//! `SearchReport::data_json`, where search documents are actually emitted.

use std::fs;
use std::path::PathBuf;

use ee::core::profile::{OperatingProfile, RuntimeProfileReport};
use ee::core::search::{
    ScoreExplanation, ScoreFactor, ScoreSource, SearchHit, SearchReport, SearchSourceMode,
    SearchStatus,
};
use ee::models::{EmbedBackend, MemoryScope, MemoryScopeStats};
use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_schema() -> Result<Value, String> {
    let text = fs::read_to_string(repo_root().join("docs/schemas/ee.search.document.v1.json"))
        .map_err(|error| format!("read schema: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse schema: {error}"))
}

fn test_runtime_profile() -> RuntimeProfileReport {
    RuntimeProfileReport::for_profile(OperatingProfile::Workstation, "search_document_schema")
}

fn report_with_results(results: Vec<SearchHit>) -> SearchReport {
    SearchReport {
        status: SearchStatus::Success,
        embed_backend: EmbedBackend::HashFallback,
        query: "schema contract".to_string(),
        requested_limit: results.len() as u32,
        results,
        elapsed_ms: 3.5,
        errors: Vec::new(),
        degraded: Vec::new(),
        runtime_profile: test_runtime_profile(),
        rerank_configured_mode: ee::config::SearchRerankMode::Auto,
        rerank_configured_top_k: 50,
        rerank_runtime_available: false,
        relevance_floor_applied: Some(0.0),
        candidates_below_floor: 0,
        query_assist: None,
        source_mode_requested: SearchSourceMode::Hybrid,
        source_mode_applied: SearchSourceMode::Hybrid,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
        index_freshness: None,
    }
}

fn full_emitted_search_document() -> Result<Value, String> {
    let report = report_with_results(vec![SearchHit {
        doc_id: "mem_search_document_schema".to_string(),
        score: 0.91,
        source: ScoreSource::Hybrid,
        fast_score: Some(0.81),
        quality_score: Some(0.93),
        lexical_score: Some(0.72),
        rerank_score: Some(0.88),
        metadata: Some(serde_json::json!({
            "level": "procedural",
            "kind": "rule",
            "content": "Run the canonical release verification sequence before publishing. ".repeat(6),
            "provenance_uri": "file://AGENTS.md#L42",
            "scoreInterval": [0.72, 0.97],
            "coverageGuarantee": 0.95,
            "calibrated": true,
            "driftHint": {
                "status": "current"
            },
            "valid_from": "2026-05-22T00:00:00Z",
            "validity_status": "current",
            "validity_window_kind": "open"
        })),
        explanation: Some(ScoreExplanation {
            summary: "Selected by hybrid retrieval with score 0.9100.".to_string(),
            factors: vec![ScoreFactor {
                name: "lexical".to_string(),
                value: 0.72,
                contribution: "matched query terms".to_string(),
                source_field: "lexicalScore".to_string(),
                formula: "bm25".to_string(),
            }],
        }),
    }]);
    report
        .data_json()
        .pointer("/results/0")
        .cloned()
        .ok_or_else(|| "missing emitted search result".to_string())
}

fn reranked_emitted_search_document() -> Result<Value, String> {
    let report = report_with_results(vec![SearchHit {
        doc_id: "mem_search_document_reranked".to_string(),
        score: 0.88,
        source: ScoreSource::Reranked,
        fast_score: Some(0.61),
        quality_score: Some(0.73),
        lexical_score: Some(0.44),
        rerank_score: Some(0.88),
        metadata: Some(serde_json::json!({
            "level": "semantic",
            "kind": "fact",
            "provenance_uri": "file://README.md#L92",
            "scoreInterval": [0.80, 0.93],
            "coverageGuarantee": 0.95,
            "calibrated": true,
            "valid_from": "2026-06-18T00:00:00Z",
            "validity_status": "current",
            "validity_window_kind": "open"
        })),
        explanation: Some(ScoreExplanation {
            summary: "Reranked by local cross-encoder with score 0.8800.".to_string(),
            factors: vec![ScoreFactor {
                name: "rerank".to_string(),
                value: 0.88,
                contribution: "cross-encoder score".to_string(),
                source_field: "rerankScore".to_string(),
                formula: "score = rerank_score".to_string(),
            }],
        }),
    }]);
    report
        .data_json()
        .pointer("/results/0")
        .cloned()
        .ok_or_else(|| "missing reranked search result".to_string())
}

fn minimal_emitted_search_document() -> Result<Value, String> {
    let report = report_with_results(vec![SearchHit {
        doc_id: "doc_minimal".to_string(),
        score: 0.5,
        source: ScoreSource::Lexical,
        fast_score: None,
        quality_score: None,
        lexical_score: None,
        rerank_score: None,
        metadata: None,
        explanation: None,
    }]);
    report
        .data_json()
        .pointer("/results/0")
        .cloned()
        .ok_or_else(|| "missing minimal emitted search result".to_string())
}

#[test]
fn search_document_v1_validates_full_real_emission() -> TestResult {
    let schema = read_schema()?;
    let document = full_emitted_search_document()?;
    validate_json_schema(&document, &schema, "$")?;
    if !document.get("content").is_some_and(Value::is_string)
        || document.get("content_truncated").and_then(Value::as_bool) != Some(true)
        || document.get("contentPreview").is_some()
    {
        return Err(format!(
            "canonical bounded content fields drifted: {document}"
        ));
    }
    Ok(())
}

#[test]
fn search_document_v1_validates_reranked_real_emission() -> TestResult {
    let schema = read_schema()?;
    let document = reranked_emitted_search_document()?;

    validate_json_schema(&document, &schema, "$")?;

    if document.pointer("/source").and_then(Value::as_str) != Some("reranked") {
        return Err(format!(
            "reranked search document should expose source=reranked, got {:?}",
            document.pointer("/source")
        ));
    }
    if document.pointer("/scoreKind").and_then(Value::as_str) != Some("reranked") {
        return Err(format!(
            "reranked search document should expose scoreKind=reranked, got {:?}",
            document.pointer("/scoreKind")
        ));
    }
    let rerank_score = document
        .pointer("/rerankScore")
        .and_then(Value::as_f64)
        .ok_or_else(|| "reranked search document must include rerankScore".to_string())?;
    let relevance_score = document
        .pointer("/relevanceScore")
        .and_then(Value::as_f64)
        .ok_or_else(|| "reranked search document must include relevanceScore".to_string())?;
    if (rerank_score - 0.88).abs() > 0.000_001 {
        return Err(format!("unexpected rerankScore {rerank_score}"));
    }
    if (relevance_score - 0.88).abs() > 0.000_001 {
        return Err(format!(
            "reranked relevanceScore should preserve unit rerank score, got {relevance_score}"
        ));
    }

    Ok(())
}

#[test]
fn search_document_v1_validates_minimal_real_emission() -> TestResult {
    let schema = read_schema()?;
    let document = minimal_emitted_search_document()?;
    validate_json_schema(&document, &schema, "$")
}

#[test]
fn search_document_v1_validator_rejects_unknown_result_fields() -> TestResult {
    let schema = read_schema()?;
    let mut document = full_emitted_search_document()?;
    document
        .as_object_mut()
        .ok_or_else(|| "document is not an object".to_string())?
        .insert("doc_id".to_string(), Value::String("legacy".to_string()));

    match validate_json_schema(&document, &schema, "$") {
        Ok(()) => Err("validator accepted legacy doc_id field".to_string()),
        Err(_) => Ok(()),
    }
}

#[test]
fn search_document_v1_validator_rejects_missing_calibration_fields() -> TestResult {
    let schema = read_schema()?;

    for field in ["scoreInterval", "coverageGuarantee", "calibrated"] {
        let mut document = full_emitted_search_document()?;
        document
            .as_object_mut()
            .ok_or_else(|| "document is not an object".to_string())?
            .remove(field);

        match validate_json_schema(&document, &schema, "$") {
            Ok(()) => return Err(format!("validator accepted document missing {field}")),
            Err(_) => {}
        }
    }

    Ok(())
}

#[test]
fn search_document_v1_requires_interpretable_score_fields() -> TestResult {
    let schema = read_schema()?;
    let document = full_emitted_search_document()?;

    if document
        .pointer("/relevanceScore")
        .and_then(Value::as_f64)
        .is_none()
    {
        return Err("emitted search document must include relevanceScore".to_string());
    }
    if document.pointer("/scoreKind").and_then(Value::as_str) != Some("rrf_fused") {
        return Err(format!(
            "hybrid search document should expose scoreKind=rrf_fused, got {:?}",
            document.pointer("/scoreKind")
        ));
    }

    for field in ["relevanceScore", "scoreKind"] {
        let mut missing = document.clone();
        missing
            .as_object_mut()
            .ok_or_else(|| "document is not an object".to_string())?
            .remove(field);

        match validate_json_schema(&missing, &schema, "$") {
            Ok(()) => return Err(format!("validator accepted document missing {field}")),
            Err(_) => {}
        }
    }

    Ok(())
}

fn validate_json_schema(value: &Value, schema: &Value, path: &str) -> TestResult {
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        if options
            .iter()
            .any(|candidate| validate_json_schema(value, candidate, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path} did not match any oneOf branch"));
    }

    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(format!("{path} expected const {expected}, got {value}"));
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(format!(
            "{path} value {value} is not in enum {enum_values:?}"
        ));
    }

    if let Some(expected_types) = schema_types(schema)
        && !expected_types
            .iter()
            .any(|expected_type| json_type_matches(value, expected_type))
    {
        return Err(format!(
            "{path} expected type {:?}, got {}",
            expected_types,
            json_type_name(value)
        ));
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                let field = field
                    .as_str()
                    .ok_or_else(|| format!("{path} schema required entry is not a string"))?;
                if !object.contains_key(field) {
                    return Err(format!("{path} missing required field {field}"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object {
            let child_path = format!("{path}.{key}");
            if let Some(property_schema) = properties.and_then(|props| props.get(key)) {
                validate_json_schema(child, property_schema, &child_path)?;
                continue;
            }
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    return Err(format!("{path} contains unexpected field {key}"));
                }
                Some(Value::Bool(true)) | None => {}
                Some(Value::Object(property_schema)) => {
                    validate_json_schema(
                        child,
                        &Value::Object(property_schema.clone()),
                        &child_path,
                    )?;
                }
                Some(other) => {
                    return Err(format!(
                        "{path} has unsupported additionalProperties schema {other}"
                    ));
                }
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && array.len() < min_items as usize
        {
            return Err(format!("{path} expected at least {min_items} items"));
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
            && array.len() > max_items as usize
        {
            return Err(format!("{path} expected at most {max_items} items"));
        }
        if let Some(prefix_items) = schema.get("prefixItems").and_then(Value::as_array) {
            for (index, item_schema) in prefix_items.iter().enumerate() {
                let item = array
                    .get(index)
                    .ok_or_else(|| format!("{path} missing prefix item {index}"))?;
                validate_json_schema(item, item_schema, &format!("{path}[{index}]"))?;
            }
        } else if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_json_schema(item, item_schema, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn schema_types(schema: &Value) -> Option<Vec<&str>> {
    match schema.get("type") {
        Some(Value::String(value)) => Some(vec![value.as_str()]),
        Some(Value::Array(values)) => Some(values.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
}

fn json_type_matches(value: &Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
