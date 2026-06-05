//! bd-2s3e9: property tests for `parse_eql_query` (src/models/query.rs:905).
//!
//! `docs/query-schema.md` now records the query-file follow-up checklist as
//! implemented history. This file keeps the property-test contract for the
//! canonical EQL parser (`parse_eql_query`) explicit alongside the happy-path
//! conformance coverage in `tests/conformance/query_v1_matrix.rs` and the
//! validation-error coverage in `tests/e2e_query_file_validation.rs`.
//!
//! Properties pinned here:
//!
//! * **Never panics on arbitrary JSON.** Parser is total: any
//!   `serde_json::Value` either returns `Ok(EqlQuery)` or
//!   `Err(EqlQueryError)`. No unwrap/expect/panic should escape.
//! * **Invariants on rejected inputs.**
//!   - Empty / whitespace-only `q` is always rejected.
//!   - `limit = 0` is always rejected.
//!   - Any unknown top-level key is always rejected.
//!   - Wrong type for `q` (non-string) is always rejected.
//! * **`q` trim idempotency.** A `q` field surrounded by ASCII whitespace
//!   parses to the same `EqlQuery.q` value as the trimmed form.
//! * **Defaults are deterministic.** A minimal valid query (`{"q":
//!   "<text>"}`) always produces the same default `tags_mode`, `speed`,
//!   `limit`, `rerank`, `return_subgraph`, `explain` regardless of which
//!   non-rejecting input shape produced it.

use ee::models::query::{EqlSpeedMode, EqlTagsMode, parse_eql_query};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::{Map, Value, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// Generate arbitrary `serde_json::Value` trees up to a small depth so the
/// fuzzer can build pathological objects/arrays without exploding case
/// runtime.
fn arb_json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|i| json!(i)),
        any::<f64>()
            .prop_filter("finite", |n| n.is_finite())
            .prop_map(|n| json!(n)),
        ".{0,32}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 32, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((".{0,16}", inner), 0..6).prop_map(|entries| {
                let mut map = Map::new();
                for (key, value) in entries {
                    map.insert(key, value);
                }
                Value::Object(map)
            }),
        ]
    })
}

/// Strategy producing only the top-level keys that `parse_eql_query`
/// recognizes. Keeps fuzzed inputs more likely to reach deeper code paths
/// instead of getting rejected at the unknown-field gate.
fn arb_eql_known_key() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("q".to_string()),
        Just("query".to_string()),
        Just("workspace".to_string()),
        Just("levels".to_string()),
        Just("kinds".to_string()),
        Just("tags".to_string()),
        Just("tags_mode".to_string()),
        Just("scope".to_string()),
        Just("time".to_string()),
        Just("confidence".to_string()),
        Just("graph".to_string()),
        Just("limit".to_string()),
        Just("speed".to_string()),
        Just("rerank".to_string()),
        Just("return_subgraph".to_string()),
        Just("explain".to_string()),
    ]
}

/// Build a candidate EQL document from a list of known keys mapped to
/// arbitrary JSON. The shape is otherwise unconstrained — many will be
/// rejected, which is the point.
fn arb_eql_document_with_known_keys() -> impl Strategy<Value = Value> {
    prop::collection::vec((arb_eql_known_key(), arb_json_value()), 0..8).prop_map(|entries| {
        let mut map = Map::new();
        for (key, value) in entries {
            map.insert(key, value);
        }
        Value::Object(map)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Property: `parse_eql_query` is total — it always returns
    /// `Ok` or `Err`, never panics, no matter what JSON we feed it.
    #[test]
    fn parse_eql_query_never_panics_on_arbitrary_json(value in arb_json_value()) {
        let _ = parse_eql_query(&value);
    }

    /// Property: `parse_eql_query` is total when each top-level key
    /// is one the parser claims to understand, even if its value is
    /// garbage. Catches panics behind the per-field validators.
    #[test]
    fn parse_eql_query_never_panics_on_known_key_garbage(
        value in arb_eql_document_with_known_keys(),
    ) {
        let _ = parse_eql_query(&value);
    }

    /// Property: empty / whitespace-only `q` is always rejected.
    /// Documents the invariant from src/models/query.rs:920-922.
    #[test]
    fn parse_eql_query_rejects_blank_query_text(pad in r"[ \t\n\r]{0,16}") {
        let value = json!({"q": pad});
        prop_assert!(parse_eql_query(&value).is_err());
    }

    /// Property: `limit = 0` is always rejected. Documents the
    /// invariant from src/models/query.rs:939-944.
    #[test]
    fn parse_eql_query_rejects_zero_limit(text in r"[A-Za-z]{1,16}") {
        let value = json!({"q": text, "limit": 0});
        let error = parse_eql_query(&value).map_or_else(
            Ok,
            |query| Err(TestCaseError::fail(format!("limit=0 unexpectedly parsed: {query:?}"))),
        )?;
        prop_assert_eq!(error.field, "limit");
    }

    /// Property: any top-level field outside the ee.query.v1 section
    /// 13.5 shape is always rejected. Documents the unknown-field
    /// gate at src/models/query.rs:909-916.
    #[test]
    fn parse_eql_query_rejects_unknown_top_level_field(
        text in r"[A-Za-z]{1,16}",
        unknown in r"[A-Za-z_][A-Za-z0-9_]{0,8}",
    ) {
        prop_assume!(![
            "q", "query", "workspace", "levels", "kinds", "tags", "tags_mode",
            "scope", "time", "confidence", "graph", "limit", "speed", "rerank",
            "return_subgraph", "explain",
        ].contains(&unknown.as_str()));
        let value = json!({"q": text, unknown: 1});
        prop_assert!(parse_eql_query(&value).is_err());
    }

    /// Property: when `q` is wrapped in surrounding ASCII whitespace,
    /// the parsed `EqlQuery.q` is byte-identical to the trimmed form.
    /// Documents the trim contract in src/models/query.rs:952.
    #[test]
    fn parse_eql_query_trims_query_text(
        pad_left in r"[ \t]{0,8}",
        text in r"[A-Za-z][A-Za-z0-9 _-]{0,32}",
        pad_right in r"[ \t]{0,8}",
    ) {
        prop_assume!(!text.trim().is_empty());
        let padded = format!("{pad_left}{text}{pad_right}");
        let trimmed = text.trim().to_string();
        let padded_query = parse_eql_query(&json!({"q": padded.clone()}))
            .map_err(|error| TestCaseError::fail(format!("padded query should parse: {error}")))?;
        let trimmed_query = parse_eql_query(&json!({"q": trimmed.clone()}))
            .map_err(|error| TestCaseError::fail(format!("trimmed query should parse: {error}")))?;
        prop_assert_eq!(padded_query.q, trimmed_query.q);
    }

    /// Property: a minimal valid query always produces the documented
    /// defaults (src/models/query.rs:932, :938, :948, :964-966).
    #[test]
    fn parse_eql_query_defaults_are_stable(text in r"[A-Za-z][A-Za-z0-9 _-]{0,32}") {
        prop_assume!(!text.trim().is_empty());
        let parsed = parse_eql_query(&json!({"q": text}))
            .map_err(|error| TestCaseError::fail(format!("minimal q should parse: {error}")))?;
        prop_assert_eq!(parsed.tags_mode, EqlTagsMode::Any);
        prop_assert_eq!(parsed.speed, EqlSpeedMode::Default);
        prop_assert_eq!(parsed.limit, 10);
        prop_assert!(!parsed.rerank);
        prop_assert!(!parsed.return_subgraph);
        prop_assert!(!parsed.explain);
    }
}

/// Spot-check: non-string `q` is rejected with the documented field path.
/// Not generated through proptest because the value space is tiny.
#[test]
fn parse_eql_query_rejects_non_string_query_field() -> TestResult {
    for bad in [
        json!({"q": 42}),
        json!({"q": null}),
        json!({"q": true}),
        json!({"q": ["text"]}),
        json!({"q": {"text": "release"}}),
    ] {
        let error = parse_eql_query(&bad).map_or_else(
            |error| error,
            |query| panic!("non-string q unexpectedly parsed: {query:?}"),
        );
        ensure(
            error.field == "q",
            format!(
                "non-string q should report field=q; got field={} for input {bad}",
                error.field
            ),
        )?;
    }
    Ok(())
}

/// Spot-check: top-level non-object value is rejected with `$` field path.
#[test]
fn parse_eql_query_rejects_non_object_root() -> TestResult {
    for bad in [
        Value::Null,
        json!(true),
        json!(42),
        json!("hello"),
        json!([{"q": "hello"}]),
    ] {
        let error = parse_eql_query(&bad).map_or_else(
            |error| error,
            |query| panic!("non-object root unexpectedly parsed: {query:?}"),
        );
        ensure(
            error.field == "$",
            format!(
                "non-object root should report field=`$`; got field={} for input {bad}",
                error.field
            ),
        )?;
    }
    Ok(())
}
