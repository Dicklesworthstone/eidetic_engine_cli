//! Strict validation of canonical search documents received from the daemon.
//!
//! Keep the field set explicit: accepting arbitrary additions would hide drift
//! (and legacy spellings). The tests compare it with both published schemas and
//! round-trip real `SearchReport` emissions through the served response path.

use super::validate_exact_object_fields;

const REQUIRED: &[&str] = &[
    "docId",
    "score",
    "relevanceScore",
    "scoreKind",
    "scoreInterval",
    "coverageGuarantee",
    "calibrated",
    "source",
    "why",
    "provenance",
];
const OPTIONAL: &[&str] = &[
    // Current emitters always include this field, but older v3 daemons may
    // omit it. Its absence means unavailable metadata, not response drift.
    "calibrationId",
    "memoryId",
    "fastScore",
    "qualityScore",
    "lexicalScore",
    "rerankScore",
    "metadata",
    "driftHint",
    "meshProvenance",
    "meshTrustAdjustment",
    "content",
    "content_truncated",
    "contentRedacted",
    "redactions",
    "tombstoned",
    "tombstonedAt",
    "validFrom",
    "validTo",
    "validityStatus",
    "validityWindowKind",
    "explanation",
];

pub(super) fn validate_canonical_search_result(
    result: &serde_json::Value,
    index: usize,
) -> Result<(), String> {
    let context = format!("canonical search result[{index}]");
    validate_exact_object_fields(result, &context, REQUIRED, OPTIONAL)?;
    if result
        .get("calibrationId")
        .is_some_and(|value| !value.is_string() && !value.is_null())
    {
        return Err(format!("{context}.calibrationId must be a string or null"));
    }
    if !["docId", "why"]
        .iter()
        .all(|field| result.get(*field).is_some_and(serde_json::Value::is_string))
        || !result
            .get("provenance")
            .is_some_and(serde_json::Value::is_array)
        || !result
            .get("calibrated")
            .is_some_and(serde_json::Value::is_boolean)
    {
        return Err(format!("{context} required field types drifted"));
    }
    for field in ["score", "relevanceScore"] {
        let value = result
            .get(field)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| format!("{context}.{field} must be a number"))?;
        if !value.is_finite() {
            return Err(format!("{context}.{field} must be finite"));
        }
    }
    let relevance = result["relevanceScore"].as_f64().unwrap_or_default();
    if !(0.0..=1.0).contains(&relevance) {
        return Err(format!("{context}.relevanceScore must be between 0 and 1"));
    }
    // FAIL-CLOSED, AND DELIBERATELY SINGLE-SPELLING. The lexical tag was
    // renamed `unit_normalized` -> `query_relative_pool_minmax`
    // (bd-reality-core-convergence-1azkt.11). The old spelling is NOT retained
    // beside the new one: accepting both would let a half-renamed emitter keep
    // shipping the tag the rename exists to remove, and this validator would
    // report healthy while doing it. One spelling means a partial rename
    // refuses loudly instead of degrading quietly.
    if !matches!(
        result.get("scoreKind").and_then(serde_json::Value::as_str),
        Some("query_relative_pool_minmax" | "cosine_similarity" | "rrf_fused" | "reranked")
    ) || !matches!(
        result.get("source").and_then(serde_json::Value::as_str),
        Some(
            "lexical"
                | "semantic_fast"
                | "semantic_quality"
                | "hash_control"
                | "hybrid"
                | "reranked"
        )
    ) {
        return Err(format!("{context} score/source vocabulary drifted"));
    }
    let interval = result
        .get("scoreInterval")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{context}.scoreInterval must be an array"))?;
    if interval.len() != 2
        || !interval
            .iter()
            .all(|value| value.as_f64().is_some_and(f64::is_finite))
    {
        return Err(format!(
            "{context}.scoreInterval must contain two finite numbers"
        ));
    }
    if !result.get("coverageGuarantee").is_some_and(|value| {
        value.is_null()
            || value
                .as_f64()
                .is_some_and(|number| number.is_finite() && (0.0..=1.0).contains(&number))
    }) {
        return Err(format!("{context}.coverageGuarantee drifted"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "search_result_contract_tests.rs"]
mod tests;
