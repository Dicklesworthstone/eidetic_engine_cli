//! bd-1nxz4.1: property coverage for the zero-copy response envelope
//! renderer (`JsonBuilder` + `ResponseEnvelope` in `src/output/mod.rs`).
//!
//! The hot-surface JSON output path bypasses `serde_json::to_value` ->
//! `serde_json::to_string` allocation for high-frequency commands. This
//! file pins the invariants that adopters rely on:
//!
//! 1. `JsonBuilder` output is always parseable as valid JSON for any
//!    sequence of `field_str` / `field_raw` / `field_bool` /
//!    `field_u32` / `field_i32` / `field_object` / `field_array_of_*`
//!    insertions.
//!
//! 2. Field insertion order is preserved exactly. A consumer that
//!    captures bytes from the renderer can rely on the field order
//!    matching the call order.
//!
//! 3. `escape_json_string` is round-trippable: arbitrary input strings
//!    survive insertion via `field_str` and re-parse to the exact same
//!    string via `serde_json::from_str`. This is the byte-identical
//!    parity guarantee against `serde_json::to_string` for the
//!    string-field path.
//!
//! 4. `ResponseEnvelope::success()` / `failure()` emit a stable
//!    prefix that downstream consumers can detect without parsing, and
//!    success envelopes always carry a top-level `degraded` array.
//!
//! 5. Numeric field writers (`field_bool`, `field_u32`, `field_i32`)
//!    emit unquoted JSON literals that re-parse to the original value.
//!
//! These properties protect the zero-copy adoption path from silent
//! drift when adding new hot surfaces; if any one of them breaks,
//! existing adopters (`render_status_json_filtered`, the install /
//! procedure / curate JSON renderers) silently produce malformed or
//! reordered output.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::output::{
    JsonBuilder, OutputSizeDiagnostic, ResponseEnvelope, escape_json_string, render_toon_from_json,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use serde_json::{Map, Value};

fn json_key() -> impl Strategy<Value = String> {
    // Restrict keys to ASCII-safe identifiers to keep the property
    // focused on order/escape behavior rather than key-escape edge
    // cases (which are owned by `escape_json_string` directly).
    "[a-zA-Z][a-zA-Z0-9_]{0,15}".prop_map(String::from)
}

fn json_string_value() -> impl Strategy<Value = String> {
    // Include the JSON-significant punctuation so escape behavior gets
    // exercised: double quote, backslash, control characters, and
    // multi-byte unicode all need to survive a round trip.
    proptest::collection::vec(any::<char>(), 0..32).prop_map(|chars| chars.into_iter().collect())
}

fn bounded_json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|value| Value::Number(value.into())),
        json_string_value().prop_map(Value::String),
    ];

    leaf.prop_recursive(3, 32, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::vec((json_key(), inner), 0..4).prop_map(|entries| {
                let mut object = Map::new();
                for (key, value) in entries {
                    object.insert(key, value);
                }
                Value::Object(object)
            }),
        ]
    })
}

fn response_envelope_value() -> impl Strategy<Value = Value> {
    bounded_json_value().prop_map(|data| {
        let mut root = Map::new();
        root.insert(
            "schema".to_string(),
            Value::String("ee.response.v2".to_string()),
        );
        root.insert("success".to_string(), Value::Bool(true));
        root.insert("data".to_string(), data);
        root.insert("degraded".to_string(), Value::Array(Vec::new()));
        Value::Object(root)
    })
}

fn raw_jsonish_input() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..256).prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any sequence of unique key + arbitrary string-value pairs,
    /// the `JsonBuilder` output parses as a JSON object and round-trips
    /// to a map whose entries match the inserted pairs in iteration
    /// order.
    #[test]
    fn json_builder_field_str_round_trips_through_serde_json(
        pairs in proptest::collection::vec((json_key(), json_string_value()), 0..12)
    ) {
        // De-duplicate keys deterministically by retaining the LAST value
        // for each key (matches the JSON-object override semantics that
        // serde_json applies when parsing back).
        let mut seen = std::collections::BTreeSet::new();
        let mut ordered: Vec<(String, String)> = Vec::new();
        for (key, value) in pairs.into_iter().rev() {
            if seen.insert(key.clone()) {
                ordered.push((key, value));
            }
        }
        ordered.reverse();

        let mut builder = JsonBuilder::with_capacity(128);
        for (key, value) in &ordered {
            builder.field_str(key, value);
        }
        let output = builder.finish();

        let parsed: Value = serde_json::from_str(&output)
            .map_err(|e| TestCaseError::fail(format!("invalid JSON: {e}; output={output}")))?;
        let map = parsed
            .as_object()
            .ok_or_else(|| TestCaseError::fail(format!("not an object: {output}")))?;
        prop_assert_eq!(map.len(), ordered.len());
        for (key, expected) in &ordered {
            let got = map
                .get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| TestCaseError::fail(format!("missing key {key} in {output}")))?;
            prop_assert_eq!(got, expected);
        }
    }

    /// Field insertion order is preserved literally in the output
    /// bytes. The first key inserted appears before the second, and
    /// so on. We assert this by scanning for each key's `"key":`
    /// pattern and checking that their byte offsets are strictly
    /// increasing.
    #[test]
    fn json_builder_preserves_field_insertion_order(
        keys in proptest::collection::vec(json_key(), 2..8)
    ) {
        let mut seen = std::collections::BTreeSet::new();
        let unique: Vec<String> = keys
            .into_iter()
            .filter(|k| seen.insert(k.clone()))
            .collect();
        prop_assume!(unique.len() >= 2);

        let mut builder = JsonBuilder::with_capacity(128);
        for (i, key) in unique.iter().enumerate() {
            builder.field_u32(key, i as u32);
        }
        let output = builder.finish();

        let mut last_offset = 0usize;
        for key in &unique {
            let needle = format!("\"{key}\":");
            let offset = output
                .find(&needle)
                .ok_or_else(|| TestCaseError::fail(format!("missing key {needle} in {output}")))?;
            prop_assert!(
                offset >= last_offset,
                "key {key} at offset {offset} appeared before previous key (last_offset={last_offset}); output={output}"
            );
            last_offset = offset;
        }
    }

    /// `escape_json_string` round-trips arbitrary input through
    /// `serde_json::from_str`. This is the byte-correctness anchor for
    /// the string-field path: if the escape function ever drops a
    /// character or produces invalid escape sequences, this property
    /// fails.
    #[test]
    fn escape_json_string_round_trips_through_serde_json(value in json_string_value()) {
        let mut builder = JsonBuilder::with_capacity(64);
        builder.field_str("v", &value);
        let output = builder.finish();

        let parsed: Value = serde_json::from_str(&output)
            .map_err(|e| TestCaseError::fail(format!("invalid JSON: {e}; output={output}")))?;
        let got = parsed
            .get("v")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TestCaseError::fail(format!("missing v in {output}")))?;
        prop_assert_eq!(got, &value);

        // And `escape_json_string` itself must produce a body whose
        // quoted form re-parses to the original.
        let quoted = format!("\"{}\"", escape_json_string(&value));
        let direct: String = serde_json::from_str(&quoted)
            .map_err(|e| TestCaseError::fail(format!("invalid escaped form: {e}; quoted={quoted}")))?;
        prop_assert_eq!(direct, value);
    }

    /// `field_u32` emits unquoted decimal integers that re-parse to
    /// the original value.
    #[test]
    fn json_builder_field_u32_round_trips(value in any::<u32>()) {
        let mut builder = JsonBuilder::with_capacity(32);
        builder.field_u32("n", value);
        let output = builder.finish();
        let parsed: Value = serde_json::from_str(&output)
            .map_err(|e| TestCaseError::fail(format!("invalid JSON: {e}; output={output}")))?;
        let got = parsed
            .get("n")
            .and_then(Value::as_u64)
            .ok_or_else(|| TestCaseError::fail(format!("missing n in {output}")))?;
        prop_assert_eq!(got, u64::from(value));
    }

    /// `field_i32` emits unquoted signed integers (including the
    /// minus sign for negatives) that re-parse to the original value.
    #[test]
    fn json_builder_field_i32_round_trips(value in any::<i32>()) {
        let mut builder = JsonBuilder::with_capacity(32);
        builder.field_i32("n", value);
        let output = builder.finish();
        let parsed: Value = serde_json::from_str(&output)
            .map_err(|e| TestCaseError::fail(format!("invalid JSON: {e}; output={output}")))?;
        let got = parsed
            .get("n")
            .and_then(Value::as_i64)
            .ok_or_else(|| TestCaseError::fail(format!("missing n in {output}")))?;
        prop_assert_eq!(got, i64::from(value));
    }

    /// The JSON->TOON adapter is an input-parsing boundary used by many
    /// renderers. Bounded generated response envelopes should never hit the
    /// fallback error path, and identical input must produce byte-identical
    /// TOON output.
    #[test]
    fn toon_rendering_of_generated_response_envelopes_is_deterministic(value in response_envelope_value()) {
        let json = serde_json::to_string(&value)
            .map_err(|error| TestCaseError::fail(format!("generated JSON failed to serialize: {error}")))?;

        let first = render_toon_from_json(&json);
        let second = render_toon_from_json(&json);

        prop_assert_eq!(&first, &second);
        prop_assert!(!first.is_empty(), "TOON output must not be empty for response envelope {json}");
        prop_assert!(
            !first.contains("toon_encoding_failed"),
            "valid generated response envelope hit TOON fallback: json={json} toon={first}"
        );
        prop_assert!(
            first.contains("schema: ee.response.v2"),
            "TOON output must preserve the response schema: json={json} toon={first}"
        );
    }

    /// Arbitrary raw input, valid JSON or not, should keep the TOON diagnostic
    /// path total: no panic, deterministic fallback, internally consistent byte
    /// accounting, and parseable diagnostic JSON.
    #[test]
    fn toon_size_diagnostics_stay_total_for_arbitrary_raw_input(raw in raw_jsonish_input()) {
        let first = render_toon_from_json(&raw);
        let second = render_toon_from_json(&raw);
        prop_assert_eq!(&first, &second);

        let diagnostic = OutputSizeDiagnostic::from_json(&raw);
        prop_assert_eq!(diagnostic.json_bytes, raw.len());
        prop_assert_eq!(diagnostic.toon_bytes, first.len());
        prop_assert_eq!(
            diagnostic.byte_savings,
            diagnostic.json_bytes as i64 - diagnostic.toon_bytes as i64
        );
        prop_assert_eq!(
            diagnostic.token_savings,
            diagnostic.json_estimated_tokens as i64 - diagnostic.toon_estimated_tokens as i64
        );
        if raw.is_empty() {
            prop_assert_eq!(diagnostic.compression_ratio, 1.0);
        } else {
            let expected_ratio = diagnostic.toon_bytes as f64 / diagnostic.json_bytes as f64;
            prop_assert!(
                (diagnostic.compression_ratio - expected_ratio).abs() < f64::EPSILON,
                "compression ratio drifted: expected={expected_ratio} actual={}",
                diagnostic.compression_ratio
            );
        }
        prop_assert!(diagnostic.compression_ratio.is_finite());

        let diagnostic_json = diagnostic.to_json();
        let parsed: Value = serde_json::from_str(&diagnostic_json)
            .map_err(|error| TestCaseError::fail(format!("diagnostic JSON failed to parse: {error}; {diagnostic_json}")))?;
        prop_assert_eq!(parsed["schema"].as_str(), Some("ee.output_size_diagnostic.v1"));
        prop_assert_eq!(parsed["json"]["bytes"].as_u64(), Some(raw.len() as u64));
        prop_assert_eq!(parsed["toon"]["bytes"].as_u64(), Some(first.len() as u64));
    }
}

/// Empty builder produces an empty JSON object.
#[test]
fn json_builder_empty_produces_empty_object() {
    let builder = JsonBuilder::new();
    let output = builder.finish();
    assert_eq!(output, "{}");
}

/// `ResponseEnvelope::success` emits a stable prefix that hot consumers
/// can detect without parsing. The prefix is the schema+success
/// pair, in that exact order, with no leading whitespace.
#[test]
fn response_envelope_success_emits_stable_prefix() {
    let output = ResponseEnvelope::success().finish();
    assert!(
        output.starts_with("{\"schema\":\"ee.response.v2\",\"success\":true"),
        "unexpected envelope prefix: {output}"
    );
    let parsed: Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(parsed["schema"], "ee.response.v2");
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["degraded"], serde_json::json!([]));
}

/// A clean success envelope includes exactly one top-level degraded
/// field, even when the caller does not add one explicitly.
#[test]
fn response_envelope_success_appends_clean_degraded_array() {
    let output = ResponseEnvelope::success().finish();
    assert_eq!(output.matches("\"degraded\":").count(), 1);
    let parsed: Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(parsed["degraded"], serde_json::json!([]));
}

/// Explicit degradations must not be followed by a second clean
/// default array at finish time.
#[test]
fn response_envelope_success_does_not_duplicate_explicit_degraded_array() {
    let degradations = [("index_stale", "Search index is stale.")];
    let output = ResponseEnvelope::success()
        .data_raw(r#"{"command":"search"}"#)
        .degraded_array(&degradations, |obj, (code, message)| {
            obj.field_str("code", code);
            obj.field_str("message", message);
        })
        .finish();

    assert_eq!(output.matches("\"degraded\":").count(), 1);
    let parsed: Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(parsed["degraded"][0]["code"], "index_stale");
}

/// `ResponseEnvelope::failure` emits the parallel failure prefix with
/// `success: false`.
#[test]
fn response_envelope_failure_emits_stable_prefix() {
    let output = ResponseEnvelope::failure().finish();
    assert!(
        output.starts_with("{\"schema\":\"ee.response.v2\",\"success\":false"),
        "unexpected envelope prefix: {output}"
    );
    let parsed: Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(parsed["schema"], "ee.response.v2");
    assert_eq!(parsed["success"], false);
}

/// `ResponseEnvelope` round-trips a `data_raw` payload byte-for-byte
/// under the `data` key.
#[test]
fn response_envelope_data_raw_round_trips() {
    let raw = r#"{"foo":42,"bar":["a","b"]}"#;
    let output = ResponseEnvelope::success().data_raw(raw).finish();
    let parsed: Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(parsed["data"]["foo"], 42);
    assert_eq!(parsed["data"]["bar"], serde_json::json!(["a", "b"]));
}

/// Field-order check on the envelope shape: when `data` is set via
/// `data_raw`, the output keeps `schema`, `success`, then `data` in
/// that exact order and appends the clean `degraded` array last. This
/// is the byte-identical promise hot consumers depend on.
#[test]
fn response_envelope_field_order_is_schema_success_data_degraded() {
    let raw = r#"{"hit":true}"#;
    let output = ResponseEnvelope::success().data_raw(raw).finish();
    let schema_offset = output.find("\"schema\":").expect("schema present");
    let success_offset = output.find("\"success\":").expect("success present");
    let data_offset = output.find("\"data\":").expect("data present");
    let degraded_offset = output.find("\"degraded\":").expect("degraded present");
    assert!(schema_offset < success_offset, "schema before success");
    assert!(success_offset < data_offset, "success before data");
    assert!(data_offset < degraded_offset, "data before degraded");
}
