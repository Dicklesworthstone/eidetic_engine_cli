//! GH #56: exercise the emitter, not a separately maintained daemon fixture.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Value, json};

use super::super::{DaemonSearchResult, DaemonSearchTiming};
use super::{OPTIONAL, REQUIRED, validate_canonical_search_result};
use crate::core::profile::{OperatingProfile, RuntimeProfileReport};
use crate::core::search::{
    ScoreExplanation, ScoreFactor, ScoreSource, SearchAdvisorySession, SearchHit,
    SearchPerformanceTrace, SearchReport, SearchSourceMode, SearchStatus,
};
use crate::models::{EmbedBackend, MemoryScope, MemoryScopeStats};
use crate::search::SpeedMode;

const CALIBRATION_ID: &str =
    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SOURCES: [ScoreSource; 6] = [
    ScoreSource::Lexical,
    ScoreSource::SemanticFast,
    ScoreSource::SemanticQuality,
    ScoreSource::HashControl,
    ScoreSource::Hybrid,
    ScoreSource::Reranked,
];

fn report(source: ScoreSource, rich: bool) -> SearchReport {
    let hit = SearchHit {
        doc_id: if rich { "mem_contract" } else { "doc_contract" }.to_owned(),
        score: 0.75,
        source,
        fast_score: rich.then_some(0.6),
        quality_score: rich.then_some(0.7),
        lexical_score: rich.then_some(3.0),
        rerank_score: rich.then_some(0.75),
        metadata: rich.then(|| {
            json!({
                "level": "procedural",
                "kind": "rule",
                "content": "Verify the release contract before publishing. ".repeat(12),
                "provenance_uri": "file://AGENTS.md#L42",
                "scoreInterval": [0.5, 1.0],
                "coverageGuarantee": 0.95,
                "calibrated": true,
                "scoreCalibration": {"calibrationId": CALIBRATION_ID},
                "driftHint": {"status": "current"},
                "valid_from": "2026-01-01T00:00:00Z",
                "valid_to": "2027-01-01T00:00:00Z",
                "validity_status": "current",
                "validity_window_kind": "bounded"
            })
        }),
        explanation: rich.then(|| ScoreExplanation {
            summary: "Selected by the canonical search pipeline.".to_owned(),
            factors: vec![ScoreFactor {
                name: "lexical".to_owned(),
                value: 3.0,
                contribution: "matched query terms".to_owned(),
                source_field: "lexicalScore".to_owned(),
                formula: "bm25".to_owned(),
            }],
        }),
    };
    SearchReport {
        status: SearchStatus::Success,
        embed_backend: EmbedBackend::HashFallback,
        query: "release contract".to_owned(),
        requested_limit: 3,
        results: vec![hit],
        elapsed_ms: 3.5,
        errors: Vec::new(),
        degraded: Vec::new(),
        runtime_profile: RuntimeProfileReport::for_profile(
            OperatingProfile::Workstation,
            "daemon_search_contract_test",
        ),
        rerank_configured_mode: crate::config::SearchRerankMode::Auto,
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

fn emitted_method_value(report: &SearchReport, explain: bool, delivery: bool) -> Value {
    let mut session = SearchAdvisorySession::default();
    let trace = SearchPerformanceTrace::default();
    let timing = DaemonSearchTiming::from_trace(Duration::from_millis(7), &trace);
    let performance = explain.then(|| {
        report.performance_explain_json_with_trace(SpeedMode::Instant, explain, &trace)
    });
    let method = if delivery {
        let mut reservation = session.reserve_delivery("contract-workspace");
        DaemonSearchResult::from_report_for_delivery(
            report,
            explain,
            "contract-workspace",
            &mut session,
            &mut reservation,
            timing,
            performance,
        )
    } else {
        DaemonSearchResult::from_report(
            report,
            explain,
            "contract-workspace",
            &mut session,
            timing,
            performance,
        )
    };
    // Deliberately cross a JSON byte boundary, as the served path does.
    serde_json::from_slice(&serde_json::to_vec(&method).expect("encode method response"))
        .expect("decode JSON wire value")
}

fn emitted_result() -> Value {
    report(ScoreSource::Lexical, false).data_json()["results"][0].clone()
}

fn assert_round_trip(report: &SearchReport, explain: bool, delivery: bool) {
    let wire = emitted_method_value(report, explain, delivery);
    let expected_response = wire["response"].clone();
    let expected_human = wire["human"].as_str().expect("human rendering").to_owned();
    let rendered = DaemonSearchResult::from_value(wire)
        .unwrap_or_else(|reason| panic!("canonical report rejected as daemon drift: {reason}"))
        .into_renderings()
        .expect("render validated response");
    assert_eq!(rendered.response, expected_response);
    assert_eq!(rendered.human, expected_human);
    assert_eq!(rendered.performance.is_some(), explain);
    assert_eq!(
        rendered.response["data"]["results"],
        report.data_json()["results"],
        "daemon rendering must preserve the canonical documents"
    );
    assert_eq!(
        rendered.response["data"].get("resultPath").is_some(),
        explain
    );

    // Also exercise the daemon envelope's custom decoder, not just a bare
    // serde_json::Value. Its numeric representation differs from direct JSON
    // decoding, so this reaches the same uint64 normalization as the client.
    let envelope = crate::daemon::protocol::DaemonResponse::ok(
        "contract-request",
        "contract-agent",
        Some("contract-workspace".to_owned()),
        emitted_method_value(report, explain, delivery),
    );
    let mut frame = Vec::new();
    crate::daemon::protocol::write_response(&mut frame, &envelope).expect("frame reply");
    let length = u32::from_be_bytes(frame[..4].try_into().expect("length prefix")) as usize;
    assert_eq!(length, frame.len() - 4);
    let decoded: crate::daemon::protocol::DaemonResponse =
        serde_json::from_slice(&frame[4..]).expect("decode daemon response envelope");
    let framed = DaemonSearchResult::from_value(decoded.result.expect("successful reply"))
        .expect("accept canonical documents after daemon envelope decoding")
        .into_renderings()
        .expect("render framed reply");
    let documents = framed.response["data"]["results"].as_array().unwrap();
    let canonical = report.data_json();
    let expected = canonical["results"].as_array().unwrap();
    assert_eq!(documents.len(), expected.len());
    for (document, original) in documents.iter().zip(expected) {
        assert_eq!(document.get("docId"), original.get("docId"));
        assert_eq!(document.get("calibrationId"), original.get("calibrationId"));
    }
}

#[test]
fn canonical_minimal_reports_round_trip_every_source_and_rendering() {
    for source in SOURCES {
        let report = report(source, false);
        assert_eq!(report.data_json()["results"][0].get("calibrationId"), Some(&Value::Null));
        for explain in [false, true] {
            for delivery in [false, true] {
                assert_round_trip(&report, explain, delivery);
            }
        }
    }
}

#[test]
fn canonical_rich_reports_round_trip_every_source_and_rendering() {
    for source in SOURCES {
        let report = report(source, true);
        let document = report.data_json()["results"][0].clone();
        assert_eq!(document["calibrationId"], CALIBRATION_ID);
        for field in [
            "memoryId", "metadata", "fastScore", "qualityScore", "lexicalScore",
            "rerankScore", "content", "content_truncated", "driftHint", "validFrom",
            "validTo", "validityStatus", "validityWindowKind", "explanation",
        ] {
            assert!(document.get(field).is_some(), "fixture must emit {field}");
        }
        for explain in [false, true] {
            for delivery in [false, true] {
                assert_round_trip(&report, explain, delivery);
            }
        }
    }
}

#[test]
fn empty_canonical_reports_round_trip_without_a_result_fixture() {
    let mut report = report(ScoreSource::Lexical, false);
    report.results.clear();
    report.status = SearchStatus::NoResults;
    for explain in [false, true] {
        for delivery in [false, true] {
            assert_round_trip(&report, explain, delivery);
        }
    }
}

#[test]
fn calibration_id_accepts_string_null_and_legacy_absence_without_rewriting() {
    for id in [None, Some(Value::Null), Some(json!(CALIBRATION_ID)), Some(json!(""))] {
        let mut wire = emitted_method_value(&report(ScoreSource::Lexical, false), false, true);
        let document = wire["response"]["data"]["results"][0].as_object_mut().unwrap();
        document.remove("calibrationId");
        if let Some(id) = id {
            document.insert("calibrationId".to_owned(), id);
        }
        let expected = wire["response"].clone();
        let rendered = DaemonSearchResult::from_value(wire)
            .expect("compatible calibration metadata")
            .into_renderings()
            .expect("render compatible response");
        assert_eq!(rendered.response, expected);
    }
}

#[test]
fn calibration_id_rejects_non_string_non_null_types_with_result_location() {
    for invalid in [json!(true), json!(false), json!(1), json!(0.5), json!([]), json!({})] {
        let mut wire = emitted_method_value(&report(ScoreSource::Lexical, false), false, true);
        let mut bad = wire["response"]["data"]["results"][0].clone();
        bad["calibrationId"] = invalid;
        wire["response"]["data"]["results"].as_array_mut().unwrap().push(bad);
        wire["response"]["data"]["resultCount"] = json!(2);
        let reason = DaemonSearchResult::from_value(wire).expect_err("invalid ID must fail closed");
        assert_eq!(reason, "canonical search result[1].calibrationId must be a string or null");
    }
}

#[test]
fn unexpected_document_fields_still_fail_closed() {
    for field in ["newCanonicalField", "doc_id", "contentPreview", "calibration_id"] {
        let mut document = emitted_result();
        document[field] = Value::Null;
        let reason = validate_canonical_search_result(&document, 7).unwrap_err();
        assert!(reason.contains("result[7]"), "{reason}");
        assert!(reason.contains(field), "{reason}");
    }
}

#[test]
fn every_required_document_field_remains_required() {
    for field in REQUIRED {
        let mut document = emitted_result();
        document.as_object_mut().unwrap().remove(*field);
        let reason = validate_canonical_search_result(&document, 0).unwrap_err();
        assert!(reason.contains(field), "missing {field}: {reason}");
    }
}

#[test]
fn existing_document_type_score_and_vocabulary_checks_remain_strict() {
    for (field, invalid) in [
        ("docId", Value::Null), ("why", json!(false)), ("provenance", json!({})),
        ("calibrated", json!("true")), ("score", Value::Null), ("score", json!("0.5")),
        ("relevanceScore", json!(-0.1)), ("relevanceScore", json!(1.1)),
        ("scoreKind", json!("unit_normalized")), ("scoreKind", json!("unknown")),
        ("source", json!("unknown")), ("scoreInterval", Value::Null),
        ("scoreInterval", json!([0.0])), ("scoreInterval", json!([0.0, 0.5, 1.0])),
        ("scoreInterval", json!([0.0, "1"])), ("scoreInterval", json!([0.0, null])),
        ("coverageGuarantee", json!(-0.1)), ("coverageGuarantee", json!(1.1)),
        ("coverageGuarantee", json!("0.95")),
    ] {
        let mut document = emitted_result();
        document[field] = invalid;
        assert!(validate_canonical_search_result(&document, 0).is_err(), "accepted invalid {field}");
    }
}

fn schemas() -> (Value, Value) {
    let canonical = serde_json::from_str(include_str!("../../docs/schemas/ee.search.document.v1.json"))
        .expect("canonical document schema");
    let daemon: Value = serde_json::from_str(include_str!("../../docs/schemas/ee.daemon.search.response.v3.json"))
        .expect("daemon response schema");
    (canonical, daemon["$defs"]["searchDocument"].clone())
}

fn string_set(value: &Value) -> BTreeSet<&str> {
    value.as_array().expect("schema array").iter()
        .map(|value| value.as_str().expect("schema string")).collect()
}

#[test]
fn canonical_and_daemon_schema_fields_exactly_match_the_rust_validator() {
    let (canonical, daemon) = schemas();
    let accepted: BTreeSet<_> = REQUIRED.iter().chain(OPTIONAL).copied().collect();
    assert_eq!(accepted.len(), REQUIRED.len() + OPTIONAL.len(), "duplicate validator fields");
    for schema in [&canonical, &daemon] {
        let published: BTreeSet<_> = schema["properties"].as_object().unwrap()
            .keys().map(String::as_str).collect();
        assert_eq!(accepted, published, "published and accepted result fields drifted");
        assert_eq!(schema["additionalProperties"], false);
    }
}

#[test]
fn schema_required_fields_pin_the_only_legacy_compatibility_exception() {
    let (canonical, daemon) = schemas();
    let required: BTreeSet<_> = REQUIRED.iter().copied().collect();
    assert_eq!(string_set(&daemon["required"]), required);
    let mut canonical_required = string_set(&canonical["required"]);
    assert!(canonical_required.remove("calibrationId"), "current emitters must carry calibrationId");
    assert_eq!(canonical_required, required, "new required fields need an explicit compatibility decision");
    for schema in [&canonical, &daemon] {
        let properties = schema["properties"].as_object().unwrap();
        for field in string_set(&schema["required"]) {
            assert!(properties.contains_key(field), "required but undeclared: {field}");
        }
        assert_eq!(schema["properties"]["calibrationId"]["type"], json!(["string", "null"]));
    }
}

#[test]
fn published_score_and_source_vocabularies_are_accepted_without_legacy_aliases() {
    let (canonical, daemon) = schemas();
    for field in ["scoreKind", "source"] {
        assert_eq!(canonical["properties"][field]["enum"], daemon["properties"][field]["enum"]);
        for value in canonical["properties"][field]["enum"].as_array().unwrap() {
            let mut document = emitted_result();
            document[field] = value.clone();
            validate_canonical_search_result(&document, 0)
                .unwrap_or_else(|reason| panic!("published {field}={value} rejected: {reason}"));
        }
    }
    assert!(!string_set(&canonical["properties"]["scoreKind"]["enum"]).contains("unit_normalized"));
}
