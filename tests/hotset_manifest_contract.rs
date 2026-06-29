//! Contract coverage for `ee.hotset_manifest.v1` fixtures.
//!
//! The cache executor consumes `ee.cache.hotset.v1`; this higher-level
//! manifest is the read-only swarm planning contract. These tests keep
//! the schema registry and the cold/warm/abstain fixtures pinned without
//! requiring a live workspace.

use serde_json::Value;

type TestResult = Result<(), String>;

struct Fixture {
    name: &'static str,
    text: &'static str,
    expected_status: &'static str,
    expected_blocked: bool,
    expected_reason: Option<&'static str>,
    expected_item_count: u64,
    expected_admitted_count: u64,
    expected_fail_closed_count: u64,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "cold_workspace",
        text: include_str!("fixtures/golden/cache/hotset_manifest_cold_workspace.json.golden"),
        expected_status: "clear",
        expected_blocked: false,
        expected_reason: None,
        expected_item_count: 0,
        expected_admitted_count: 0,
        expected_fail_closed_count: 0,
    },
    Fixture {
        name: "warm_workspace",
        text: include_str!("fixtures/golden/cache/hotset_manifest_warm_workspace.json.golden"),
        expected_status: "clear",
        expected_blocked: false,
        expected_reason: None,
        expected_item_count: 4,
        expected_admitted_count: 4,
        expected_fail_closed_count: 0,
    },
    Fixture {
        name: "stale_source_abstain",
        text: include_str!(
            "fixtures/golden/cache/hotset_manifest_stale_source_abstain.json.golden"
        ),
        expected_status: "fail_closed",
        expected_blocked: true,
        expected_reason: Some("source_snapshot_stale"),
        expected_item_count: 1,
        expected_admitted_count: 0,
        expected_fail_closed_count: 1,
    },
    Fixture {
        name: "dirty_checkout_abstain",
        text: include_str!(
            "fixtures/golden/cache/hotset_manifest_dirty_checkout_abstain.json.golden"
        ),
        expected_status: "fail_closed",
        expected_blocked: true,
        expected_reason: Some("dirty_checkout_overlap"),
        expected_item_count: 1,
        expected_admitted_count: 0,
        expected_fail_closed_count: 1,
    },
    Fixture {
        name: "resource_pressure_abstain",
        text: include_str!(
            "fixtures/golden/cache/hotset_manifest_resource_pressure_abstain.json.golden"
        ),
        expected_status: "fail_closed",
        expected_blocked: true,
        expected_reason: Some("resource_budget_exceeded"),
        expected_item_count: 1,
        expected_admitted_count: 0,
        expected_fail_closed_count: 1,
    },
];

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn parse_fixture(fixture: &Fixture) -> Result<Value, String> {
    serde_json::from_str(fixture.text)
        .map_err(|error| format!("{} fixture is not valid JSON: {error}", fixture.name))
}

fn as_array<'a>(value: &'a Value, pointer: &str, name: &str) -> Result<&'a Vec<Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{name} missing array at {pointer}"))
}

fn assert_no_host_private_paths(value: &Value, fixture_name: &str) -> TestResult {
    match value {
        Value::String(text) => {
            for forbidden in ["/Users/", "/private/", "C:\\", "\\Users\\"] {
                ensure(
                    !text.contains(forbidden),
                    format!("{fixture_name} leaked host-private path marker {forbidden}: {text}"),
                )?;
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_no_host_private_paths(value, fixture_name)?;
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                assert_no_host_private_paths(value, fixture_name)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

#[test]
fn hotset_manifest_schema_is_registered_and_exportable() -> TestResult {
    let schema_id = ee::models::HOTSET_MANIFEST_SCHEMA_V1;
    let registry_entry = ee::output::public_schemas()
        .iter()
        .find(|entry| entry.id == schema_id)
        .ok_or_else(|| format!("{schema_id} missing from public schema registry"))?;
    ensure(
        registry_entry.description.contains("hotset manifest"),
        "registry description should identify hotset manifest",
    )?;

    let exported = ee::output::render_schema_export_json(Some(schema_id));
    let parsed: Value = serde_json::from_str(&exported).map_err(|error| error.to_string())?;
    ensure(
        parsed["title"].as_str() == Some(schema_id),
        format!("exported schema title must be {schema_id}; got {parsed}"),
    )?;
    ensure(
        parsed
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(schema_id),
        "exported schema must pin the wire schema const",
    )?;
    ensure(
        parsed
            .pointer("/x-ee-status/tracking_bead")
            .and_then(Value::as_str)
            == Some("bd-ty3pl.1"),
        "schema status must retain its original tracking bead",
    )
}

#[test]
fn hotset_manifest_fixtures_pin_required_cases() -> TestResult {
    for fixture in FIXTURES {
        let parsed = parse_fixture(fixture)?;
        ensure(
            parsed["schema"].as_str() == Some(ee::models::HOTSET_MANIFEST_SCHEMA_V1),
            format!("{} schema id", fixture.name),
        )?;
        ensure(parsed["readOnly"] == Value::Bool(true), fixture.name)?;
        ensure(parsed["sideEffectFree"] == Value::Bool(true), fixture.name)?;
        ensure(
            parsed["mutationPolicy"].as_str() == Some("never_mutates_or_prewarms"),
            format!("{} mutation policy", fixture.name),
        )?;
        ensure(
            parsed["redactionStatus"].as_str() == Some("hashes_counts_bounded_labels_no_content"),
            format!("{} redaction status", fixture.name),
        )?;
        ensure(
            parsed
                .pointer("/workspace/workspaceLabel")
                .and_then(Value::as_str)
                == Some("."),
            format!("{} workspace label must be symbolic", fixture.name),
        )?;
        ensure(
            parsed
                .pointer("/sourceSnapshots")
                .and_then(Value::as_array)
                .is_some_and(|snapshots| !snapshots.is_empty()),
            format!("{} must include source snapshot evidence", fixture.name),
        )?;
        ensure(
            parsed.pointer("/summary/itemCount").and_then(Value::as_u64)
                == Some(fixture.expected_item_count),
            format!("{} item count", fixture.name),
        )?;
        ensure(
            parsed
                .pointer("/summary/admittedCount")
                .and_then(Value::as_u64)
                == Some(fixture.expected_admitted_count),
            format!("{} admitted count", fixture.name),
        )?;
        ensure(
            parsed
                .pointer("/summary/failClosedCount")
                .and_then(Value::as_u64)
                == Some(fixture.expected_fail_closed_count),
            format!("{} fail-closed count", fixture.name),
        )?;
        ensure(
            parsed.pointer("/failClosed/status").and_then(Value::as_str)
                == Some(fixture.expected_status),
            format!("{} fail-closed status", fixture.name),
        )?;
        ensure(
            parsed
                .pointer("/failClosed/blockedExecution")
                .and_then(Value::as_bool)
                == Some(fixture.expected_blocked),
            format!("{} blockedExecution", fixture.name),
        )?;
        if let Some(reason) = fixture.expected_reason {
            let reasons = as_array(&parsed, "/failClosed/reasons", fixture.name)?;
            ensure(
                reasons.iter().any(|value| value.as_str() == Some(reason)),
                format!("{} missing fail-closed reason {reason}", fixture.name),
            )?;
        }
        assert_no_host_private_paths(&parsed, fixture.name)?;
    }

    Ok(())
}

#[test]
fn hotset_manifest_abstain_fixtures_block_prewarm_apply() -> TestResult {
    for fixture in FIXTURES.iter().filter(|fixture| fixture.expected_blocked) {
        let parsed = parse_fixture(fixture)?;
        let items = as_array(&parsed, "/items", fixture.name)?;
        ensure(
            !items.is_empty(),
            format!("{} must name blocked items", fixture.name),
        )?;
        for item in items {
            ensure(
                item.pointer("/admission/decision").and_then(Value::as_str) == Some("fail_closed"),
                format!("{} item must fail closed: {item}", fixture.name),
            )?;
            ensure(
                item.pointer("/invalidationReason").and_then(Value::as_str)
                    == fixture.expected_reason,
                format!("{} item invalidation reason: {item}", fixture.name),
            )?;
            let evidence = item
                .pointer("/evidence")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{} item missing evidence", fixture.name))?;
            ensure(
                evidence.iter().all(|entry| {
                    entry.pointer("/redactionStatus").and_then(Value::as_str)
                        == Some("bounded_preview_no_content")
                }),
                format!("{} evidence must stay bounded-preview only", fixture.name),
            )?;
        }
    }
    Ok(())
}

#[test]
fn warm_hotset_manifest_fixture_covers_pack_search_and_read_pool_reuse() -> TestResult {
    let warm = FIXTURES
        .iter()
        .find(|fixture| fixture.name == "warm_workspace")
        .ok_or_else(|| "warm fixture missing".to_owned())?;
    let parsed = parse_fixture(warm)?;
    let item_classes = as_array(&parsed, "/items", warm.name)?
        .iter()
        .filter_map(|item| item.pointer("/itemClass").and_then(Value::as_str))
        .collect::<Vec<_>>();

    for expected in [
        "memory",
        "pack_l2_candidate",
        "read_pool_target",
        "search_index_shard",
    ] {
        ensure(
            item_classes.contains(&expected),
            format!("warm fixture missing {expected}; got {item_classes:?}"),
        )?;
    }
    ensure(
        parsed
            .pointer("/summary/classCounts/packL2Candidate")
            .and_then(Value::as_u64)
            == Some(1),
        "warm fixture must count pack L2 candidates",
    )?;
    ensure(
        parsed
            .pointer("/summary/classCounts/searchIndexShard")
            .and_then(Value::as_u64)
            == Some(1),
        "warm fixture must count search-index candidates",
    )?;
    ensure(
        parsed
            .pointer("/summary/classCounts/readPoolTarget")
            .and_then(Value::as_u64)
            == Some(1),
        "warm fixture must count read-pool candidates",
    )
}
