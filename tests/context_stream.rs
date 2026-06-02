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
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
    PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use insta::assert_json_snapshot;
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

fn candidate(seed: u128, content: &str, relevance: f32, section: PackSection) -> PackCandidate {
    PackCandidate::new(PackCandidateInput {
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
    ))
}

fn fixture_response() -> ContextResponse {
    let request = ContextRequest::from_query(QUERY).expect("request query accepts");
    let budget = TokenBudget::new(600).expect("budget accepts 600");
    let mut draft = assemble_draft(
        &request.query,
        budget,
        vec![
            candidate(
                0x11,
                "Use stream frames when an agent can consume context incrementally.",
                0.91,
                PackSection::ProceduralRules,
            ),
            candidate(
                0x12,
                "Trailer hash must match the non-streaming context pack hash.",
                0.83,
                PackSection::Decisions,
            ),
            candidate(
                0x13,
                "Partial streams are not complete packs until a terminal frame arrives.",
                0.72,
                PackSection::Failures,
            ),
        ],
    )
    .expect("draft assembles");
    draft.hash = Some("blake3:context-stream-fixture-pack".to_owned());
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

fn snapshot_stream_frames(value: Value) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        assert_json_snapshot!("context_stream", canonical_json(value));
    });
}

fn snapshot_contract_frame(frame: &Value) -> Value {
    match frame.get("kind").and_then(Value::as_str) {
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
                "relevance": frame["scores"]["relevance"],
                "utility": frame["scores"]["utility"],
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
    }
}

#[test]
fn stream_adapter_frames_match_golden_shape() -> TestResult {
    let frames = context_response_stream_frames(&fixture_response(), stream_options())
        .map_err(|error| error.to_string())?;
    let values = frames
        .iter()
        .map(|frame| serde_json::to_value(frame).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;

    snapshot_stream_frames(Value::Array(
        values.iter().map(snapshot_contract_frame).collect(),
    ));
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
