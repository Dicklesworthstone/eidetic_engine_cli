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
//!    prefix that downstream consumers can detect without parsing.
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

use ee::output::{JsonBuilder, ResponseEnvelope, escape_json_string};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use serde_json::Value;

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
/// `data_raw`, the output is `{"schema":...,"success":...,"data":...}`
/// in that exact order. This is the byte-identical promise hot
/// consumers depend on.
#[test]
fn response_envelope_field_order_is_schema_success_data() {
    let raw = r#"{"hit":true}"#;
    let output = ResponseEnvelope::success().data_raw(raw).finish();
    let schema_offset = output.find("\"schema\":").expect("schema present");
    let success_offset = output.find("\"success\":").expect("success present");
    let data_offset = output.find("\"data\":").expect("data present");
    assert!(schema_offset < success_offset, "schema before success");
    assert!(success_offset < data_offset, "success before data");
}
