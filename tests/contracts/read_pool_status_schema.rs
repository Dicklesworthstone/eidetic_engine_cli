//! Contract checks for `data.read_pool` in the `ee status --json` surface
//! (bd-2caru.4 / bd-2caru.7 acceptance). Locks the counters and acquire-wait
//! summary against the schema definition at
//! `docs/schemas/ee.status.v1.json` so the read-pool report cannot drift
//! between the Rust type, the JSON renderer, and the published schema.

use std::fs;
use std::path::PathBuf;

use ee::core::status::{ReadPoolStatusReport, StatusReport};
use ee::output::render_status_json;
use serde_json::Value;

type TestResult = Result<(), String>;

const STATUS_SCHEMA_PATH: &str = "docs/schemas/ee.status.v1.json";
const READ_POOL_FIELDS: &[&str] = &[
    "active",
    "idle",
    "max_seen",
    "drops",
    "ad_hoc_bypass_count",
    "acquire_wait",
];
const ACQUIRE_WAIT_FIELDS: &[&str] = &["samples", "p50_ns", "p99_ns"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure_string_array_contains(json: &Value, pointer: &str, needle: &str) -> TestResult {
    let array = json
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{pointer} missing array"))?;
    let found = array
        .iter()
        .filter_map(Value::as_str)
        .any(|item| item == needle);
    if found {
        Ok(())
    } else {
        Err(format!(
            "{pointer} does not contain {needle:?}; got {array:?}"
        ))
    }
}

fn ensure_object_keys_eq(json: &Value, pointer: &str, expected: &[&str]) -> TestResult {
    let map = json
        .pointer(pointer)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{pointer} missing object"))?;
    let mut actual: Vec<&str> = map.keys().map(String::as_str).collect();
    actual.sort_unstable();
    let mut expected_sorted = expected.to_vec();
    expected_sorted.sort_unstable();
    if actual == expected_sorted {
        Ok(())
    } else {
        Err(format!(
            "{pointer} keys: expected {expected_sorted:?}, got {actual:?}"
        ))
    }
}

fn ensure_str_eq(json: &Value, pointer: &str, expected: &str) -> TestResult {
    let actual = json
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{pointer} missing string"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_bool_eq(json: &Value, pointer: &str, expected: bool) -> TestResult {
    let actual = json
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{pointer} missing bool"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_u64_eq(json: &Value, pointer: &str, expected: u64) -> TestResult {
    let actual = json
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{pointer} missing u64"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{pointer}: expected {expected}, got {actual}"))
    }
}

#[test]
fn read_pool_status_schema_declares_counters_and_wait_summary() -> TestResult {
    let schema = read_json(STATUS_SCHEMA_PATH)?;

    // `read_pool` must appear in the envelope's `data.required` list so the
    // schema refuses status payloads that omit the field.
    ensure_string_array_contains(&schema, "/properties/data/required", "read_pool")?;

    // The `standard` field profile (`ee status --json` default) must
    // emit `read_pool`, so the per-profile registry stays consistent
    // with the schema's required-set.
    ensure_string_array_contains(&schema, "/field_presets/standard", "read_pool")?;

    // The `data.properties.read_pool` slot must point at the
    // canonical `$defs/readPoolStatus` definition.
    ensure_str_eq(
        &schema,
        "/properties/data/properties/read_pool/$ref",
        "#/$defs/readPoolStatus",
    )?;

    // The `$defs/readPoolStatus` definition must enforce the exact
    // counter shape: object, no additional properties, counters required,
    // and every scalar counter typed as a non-negative integer.
    ensure_str_eq(&schema, "/$defs/readPoolStatus/type", "object")?;
    ensure_bool_eq(&schema, "/$defs/readPoolStatus/additionalProperties", false)?;
    for counter in READ_POOL_FIELDS {
        ensure_string_array_contains(&schema, "/$defs/readPoolStatus/required", counter)?;
    }
    ensure_object_keys_eq(
        &schema,
        "/$defs/readPoolStatus/properties",
        READ_POOL_FIELDS,
    )?;
    for counter in ["active", "idle", "max_seen", "drops", "ad_hoc_bypass_count"] {
        let type_pointer = format!("/$defs/readPoolStatus/properties/{counter}/type");
        let minimum_pointer = format!("/$defs/readPoolStatus/properties/{counter}/minimum");
        ensure_str_eq(&schema, &type_pointer, "integer")?;
        ensure_u64_eq(&schema, &minimum_pointer, 0)?;
    }
    ensure_object_keys_eq(
        &schema,
        "/$defs/readPoolStatus/properties/acquire_wait/properties",
        ACQUIRE_WAIT_FIELDS,
    )?;
    for counter in ACQUIRE_WAIT_FIELDS {
        let type_pointer =
            format!("/$defs/readPoolStatus/properties/acquire_wait/properties/{counter}/type");
        let minimum_pointer =
            format!("/$defs/readPoolStatus/properties/acquire_wait/properties/{counter}/minimum");
        ensure_str_eq(&schema, &type_pointer, "integer")?;
        ensure_u64_eq(&schema, &minimum_pointer, 0)?;
    }

    Ok(())
}

#[test]
fn read_pool_status_report_default_emits_zero_counters() -> TestResult {
    let report = ReadPoolStatusReport::default();
    if report.active != 0
        || report.idle != 0
        || report.max_seen != 0
        || report.drops != 0
        || report.ad_hoc_bypass_count != 0
        || report.acquire_wait.samples != 0
        || report.acquire_wait.p50_ns != 0
        || report.acquire_wait.p99_ns != 0
    {
        return Err(format!(
            "ReadPoolStatusReport::default must zero every counter; got {report:?}"
        ));
    }

    // `gather()` is the const-context constructor used by `StatusReport::gather`
    // when no process-local pool has reported stats yet. It must agree with
    // `Default` so the stub state is honest at both call sites.
    let gathered = ReadPoolStatusReport::gather();
    if gathered != report {
        return Err(format!(
            "ReadPoolStatusReport::gather drifted from Default: {gathered:?} vs {report:?}"
        ));
    }

    Ok(())
}

#[test]
fn rendered_status_json_includes_read_pool_with_all_counters() -> TestResult {
    let status = StatusReport::gather();
    let rendered = render_status_json(&status);
    let parsed: Value = serde_json::from_str(&rendered)
        .map_err(|error| format!("status JSON did not parse: {error}"))?;

    // The renderer must place `read_pool` under `data` exactly as the schema
    // says, with counters and the acquire wait summary present.
    let read_pool = parsed
        .pointer("/data/read_pool")
        .ok_or_else(|| format!("data.read_pool missing from rendered status JSON: {parsed}"))?;
    ensure_object_keys_eq(&parsed, "/data/read_pool", READ_POOL_FIELDS)?;
    for counter in ["active", "idle", "max_seen", "drops", "ad_hoc_bypass_count"] {
        let pointer = format!("/data/read_pool/{counter}");
        let value = read_pool
            .pointer(&format!("/{counter}"))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{pointer} not a u64"))?;
        if value > usize::MAX as u64 {
            return Err(format!("{pointer} overflowed usize range: {value}"));
        }
    }
    ensure_object_keys_eq(&parsed, "/data/read_pool/acquire_wait", ACQUIRE_WAIT_FIELDS)?;
    for counter in ACQUIRE_WAIT_FIELDS {
        let pointer = format!("/data/read_pool/acquire_wait/{counter}");
        read_pool
            .pointer(&format!("/acquire_wait/{counter}"))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{pointer} not a u64"))?;
    }

    Ok(())
}
