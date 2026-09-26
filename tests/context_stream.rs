//! bd-1prrl.1.5: streaming context frame order and snapshot stability.
//!
//! These tests use a deterministic in-memory `ContextResponse` fixture so the
//! stream envelope shape is golden-snapshot stable and the terminal-frame
//! validator rejects partial streams.
//!
//! `ee context --stream` is a soft-deprecated alias surface; this file keeps the
//! library-level streaming adapter pinned independently of binary-level alias
//! coverage.

#![allow(clippy::expect_used)]

use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::streaming::{ContextStreamFrameOptions, context_response_stream_frames};
use ee::pack::{
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackDraft, PackDraftItem,
    PackProvenance, PackSection, PackSelectedItem, PackSelectionAudit, PackSelectionObjective,
    PackSelectionPhase, PackTrustSignal, TokenBudget,
};
use insta::assert_snapshot;
use serde_json::{Map, Value, json};
use uuid::Uuid;

type TestResult = Result<(), String>;

const QUERY: &str = "stream release guardrail";

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("unit score in range")
}

fn provenance(uri: &str) -> PackProvenance {
    PackProvenance::new(
        ProvenanceUri::from_str(uri).expect("provenance URI parses"),
        "stream fixture",
    )
    .expect("pack provenance constructs")
}

fn fixture_item(
    rank: u32,
    seed: u128,
    content: &str,
    relevance: f32,
    section: PackSection,
) -> PackDraftItem {
    let candidate = PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section,
        content: content.to_owned(),
        estimated_tokens: 12,
        relevance: unit(relevance),
        utility: unit(0.7),
        provenance: vec![provenance("file://tests/context_stream.rs")],
        why: "stream fixture item selected for context emission".to_owned(),
    })
    .expect("candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("stream-fixture".to_owned()),
    ));
    PackDraftItem {
        rank,
        memory_id: candidate.memory_id,
        section: candidate.section,
        content: candidate.content,
        estimated_tokens: candidate.estimated_tokens,
        relevance: candidate.relevance,
        utility: candidate.utility,
        proximity_to_seed: candidate.proximity_to_seed,
        score_breakdown: candidate.score_breakdown,
        attempt_family_multiplicity: candidate.attempt_family_multiplicity,
        provenance: candidate.provenance,
        why: candidate.why,
        diversity_key: candidate.diversity_key,
        trust: candidate.trust,
        redactions: Vec::new(),
        tombstoned_at: candidate.tombstoned_at,
        lifecycle: candidate.lifecycle,
        freshness_facets: Vec::new(),
        selected_in: PackSelectionPhase::StrictMmr,
        evidence_freshness: None,
        origin: None,
    }
}

fn fixture_response() -> ContextResponse {
    let mut request = ContextRequest::from_query(QUERY).expect("request query accepts");
    let budget = TokenBudget::new(600).expect("budget accepts 600");
    request.budget = budget;
    // Pin adapter input independently of evolving packing policy, including the
    // reserved failure slice. The real renderer and stream adapter run below.
    let items = vec![
        fixture_item(
            1,
            0x11,
            "Use stream frames when an agent can consume context incrementally.",
            0.91,
            PackSection::ProceduralRules,
        ),
        fixture_item(
            2,
            0x12,
            "Trailer hash must match the non-streaming context pack hash.",
            0.83,
            PackSection::Decisions,
        ),
        fixture_item(
            3,
            0x13,
            "Partial streams are not complete packs until a terminal frame arrives.",
            0.72,
            PackSection::Failures,
        ),
    ];
    let used_tokens = items.iter().map(|item| item.estimated_tokens).sum();
    let selection_audit = PackSelectionAudit {
        profile: request.profile,
        objective: PackSelectionObjective::MmrRedundancy,
        algorithm_id: "stream_fixture_input_order",
        algorithm_description: "Fixed adapter fixture; no candidate selection is performed.",
        candidate_count: items.len(),
        selected_count: items.len(),
        omitted_count: 0,
        budget_limit: budget.max_tokens(),
        budget_used: used_tokens,
        total_objective_value: 0.0,
        monotone: false,
        submodular: false,
        selected_items: items
            .iter()
            .map(|item| PackSelectedItem {
                rank: item.rank,
                memory_id: item.memory_id,
                token_cost: item.estimated_tokens,
                feasible: true,
            })
            .collect(),
        steps: Vec::new(),
    };
    let draft = PackDraft {
        query: request.query.clone(),
        budget,
        used_tokens,
        items,
        evidence_items: Vec::new(),
        omitted: Vec::new(),
        selection_audit,
        hash: Some("blake3:context-stream-fixture-pack".to_owned()),
    };
    ContextResponse::new(request, draft, Vec::new()).expect("context response constructs")
}

fn stream_options() -> ContextStreamFrameOptions {
    ContextStreamFrameOptions::new(
        "pack_stream_fixture",
        "workspace_fixture",
        "request_fixture",
        "2026-05-16T00:00:00Z",
        "2026-05-16T00:00:01Z",
    )
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonical_json(value));
            }
            Value::Object(canonical)
        }
        scalar => scalar,
    }
}

fn snapshot_stream_frames(value: Value) -> TestResult {
    let value =
        serde_json::to_string_pretty(&canonical_json(value)).map_err(|error| error.to_string())?;
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        assert_snapshot!("context_stream", value, "canonical_json(value)");
    });
    Ok(())
}

fn snapshot_score(value: &Value) -> Result<Value, String> {
    let Value::Number(number) = value else {
        return Err(format!("stream score must be a JSON number, got {value}"));
    };
    let decimal = number.to_string();
    let (whole, fraction) = decimal.split_once('.').unwrap_or((&decimal, ""));
    // The batch renderer emits non-negative decimal unit scores. Validate that
    // representation exactly, then trim only insignificant fractional zeroes.
    if !matches!(whole, "0" | "1")
        || !fraction.bytes().all(|digit| digit.is_ascii_digit())
        || (whole == "1" && fraction.bytes().any(|digit| digit != b'0'))
    {
        return Err(format!(
            "stream score must be a decimal in 0..=1, got {decimal}"
        ));
    }
    let canonical = if decimal.contains('.') {
        decimal.trim_end_matches('0').trim_end_matches('.')
    } else {
        &decimal
    };
    serde_json::Number::from_str(canonical)
        .map(Value::Number)
        .map_err(|error| error.to_string())
}

fn snapshot_contract_frame(frame: &Value) -> Result<Value, String> {
    Ok(match frame.get("kind").and_then(Value::as_str) {
        Some("header") => json!({
            "schema": frame["schema"],
            "kind": frame["kind"],
            "packId": frame["packId"],
            "query": frame["query"],
            "canonicalKeyHash": frame["canonicalKeyHash"],
        }),
        Some("item") => json!({
            "schema": frame["schema"],
            "kind": frame["kind"],
            "packId": frame["packId"],
            "seq": frame["seq"],
            "rank": frame["rank"],
            "memoryId": frame["memoryId"],
            "section": frame["section"],
            "content": frame["content"],
            "estimatedTokens": frame["estimatedTokens"],
            "scores": {
                "relevance": snapshot_score(&frame["scores"]["relevance"])?,
                "utility": snapshot_score(&frame["scores"]["utility"])?,
            },
            "why": frame["why"],
        }),
        Some("trailer") => json!({
            "schema": frame["schema"],
            "kind": frame["kind"],
            "packId": frame["packId"],
            "packHash": frame["packHash"],
            "totalItems": frame["totalItems"],
            "usedTokens": frame["usedTokens"],
            "skippedTotal": frame["skippedTotal"],
            "degraded": frame["degraded"],
        }),
        _ => frame.clone(),
    })
}

#[test]
fn stream_adapter_frames_match_golden_shape() -> TestResult {
    let response = fixture_response();
    let frames = context_response_stream_frames(&response, stream_options())
        .map_err(|error| error.to_string())?;
    let mut validator = ee::output::streaming::StreamSequenceValidator::new();
    for frame in &frames {
        validator
            .observe(frame)
            .map_err(|error| error.to_string())?;
    }
    validator.finish().map_err(|error| error.to_string())?;
    let values = frames
        .iter()
        .map(|frame| serde_json::to_value(frame).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(values.len(), response.data.pack.items.len() + 2);
    for (index, item) in response.data.pack.items.iter().enumerate() {
        let frame = &values[index + 1];
        assert_eq!(frame["rank"].as_u64(), Some(u64::from(item.rank)));
        assert_eq!(
            frame["memoryId"].as_str(),
            Some(item.memory_id.to_string().as_str())
        );
        assert_eq!(frame["content"].as_str(), Some(item.content.as_str()));
        assert_eq!(frame["why"].as_str(), Some(item.why.as_str()));
    }

    snapshot_stream_frames(Value::Array(
        values
            .iter()
            .map(snapshot_contract_frame)
            .collect::<Result<Vec<_>, _>>()?,
    ))?;
    Ok(())
}

#[test]
fn snapshot_score_preserves_digits_and_rejects_malformed_values() -> TestResult {
    for (raw, expected) in [
        ("0.910000", "0.91"),
        ("0.910000000000000000001000", "0.910000000000000000001"),
        ("0.000000", "0"),
        ("1.000000", "1"),
    ] {
        let value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        assert_eq!(snapshot_score(&value)?.to_string(), expected);
    }
    for raw in [
        "null",
        "true",
        "[]",
        "{}",
        "\"0.91\"",
        "-0.01",
        "1.1",
        "1.000000000000000000001",
        "1e309",
    ] {
        let value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        assert!(
            snapshot_score(&value).is_err(),
            "invalid score accepted: {raw}"
        );
    }
    Ok(())
}

#[test]
fn partial_stream_without_terminal_is_rejected_by_validator() -> TestResult {
    let mut frames = context_response_stream_frames(&fixture_response(), stream_options())
        .map_err(|error| error.to_string())?;
    frames.pop();
    let mut validator = ee::output::streaming::StreamSequenceValidator::new();
    for frame in &frames {
        validator
            .observe(frame)
            .map_err(|error| format!("partial prefix should be valid until finish: {error}"))?;
    }
    let error = validator
        .finish()
        .expect_err("partial stream without trailer must not be complete");
    if !matches!(
        error,
        ee::output::streaming::StreamValidationError::MissingTerminal
    ) {
        return Err(format!(
            "partial stream failed with unexpected error: {error}"
        ));
    }
    Ok(())
}
