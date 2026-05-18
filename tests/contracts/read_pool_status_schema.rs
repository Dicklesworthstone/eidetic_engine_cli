//! Contract checks for compact coordination posture in the `ee status --json`
//! surface. Locks the read-pool counters, acquire-wait summary, and QoS lane
//! summary against the schema definition at
//! `docs/schemas/ee.status.v1.json` so the read-pool report cannot drift
//! between the Rust type, the JSON renderer, and the published schema.

use std::fs;
use std::path::PathBuf;

use ee::core::doctor::DoctorReport;
use ee::core::status::{ReadPoolStatusReport, StatusReport, WalStatusReport};
use ee::db::WalStatus;
use ee::db::read_pool::{AcquireWaitStats, PoolStats};
use ee::output::{render_doctor_json, render_status_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const STATUS_SCHEMA_PATH: &str = "docs/schemas/ee.status.v1.json";
const DOCTOR_SCHEMA_PATH: &str = "docs/schemas/ee.doctor.v1.json";
const READ_POOL_FIELDS: &[&str] = &[
    "active",
    "idle",
    "active_pins",
    "expired_pins",
    "max_seen",
    "drops",
    "release_failures",
    "ad_hoc_bypass_count",
    "acquire_wait",
];
const ACQUIRE_WAIT_FIELDS: &[&str] = &["samples", "p50_ns", "p99_ns"];
const WAL_FIELDS: &[&str] = &["bytes", "frames", "page_size", "checkpoint_threshold_bytes"];
const QOS_STATUS_FIELDS: &[&str] = &[
    "schema",
    "workspaceHash",
    "foregroundActiveCount",
    "backgroundActiveCount",
    "verificationActiveCount",
    "maintenanceActiveCount",
    "staleIgnoredCount",
    "foregroundPressure",
    "backgroundWorkActive",
    "registryHealthy",
    "degraded",
];

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
    ensure_string_array_contains(&schema, "/properties/data/required", "wal")?;
    ensure_string_array_contains(&schema, "/properties/data/required", "qos")?;

    // The `standard` field profile (`ee status --json` default) must
    // emit `read_pool`, so the per-profile registry stays consistent
    // with the schema's required-set.
    ensure_string_array_contains(&schema, "/field_presets/standard", "read_pool")?;
    ensure_string_array_contains(&schema, "/field_presets/standard", "wal")?;
    ensure_string_array_contains(&schema, "/field_presets/summary", "qos")?;
    ensure_string_array_contains(&schema, "/field_presets/standard", "qos")?;

    // The `data.properties.read_pool` slot must point at the
    // canonical `$defs/readPoolStatus` definition.
    ensure_str_eq(
        &schema,
        "/properties/data/properties/read_pool/$ref",
        "#/$defs/readPoolStatus",
    )?;
    ensure_str_eq(
        &schema,
        "/properties/data/properties/wal/$ref",
        "#/$defs/walStatus",
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
    for counter in [
        "active",
        "idle",
        "active_pins",
        "expired_pins",
        "max_seen",
        "drops",
        "release_failures",
        "ad_hoc_bypass_count",
    ] {
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

    ensure_str_eq(&schema, "/$defs/walStatus/type", "object")?;
    ensure_bool_eq(&schema, "/$defs/walStatus/additionalProperties", false)?;
    ensure_object_keys_eq(&schema, "/$defs/walStatus/properties", WAL_FIELDS)?;
    for counter in WAL_FIELDS {
        ensure_string_array_contains(&schema, "/$defs/walStatus/required", counter)?;
        let type_pointer = format!("/$defs/walStatus/properties/{counter}/type");
        let minimum_pointer = format!("/$defs/walStatus/properties/{counter}/minimum");
        ensure_str_eq(&schema, &type_pointer, "integer")?;
        ensure_u64_eq(&schema, &minimum_pointer, 0)?;
    }

    Ok(())
}

#[test]
fn qos_status_schema_declares_compact_lane_summary() -> TestResult {
    let schema = read_json(STATUS_SCHEMA_PATH)?;

    ensure_str_eq(
        &schema,
        "/properties/data/properties/qos/$ref",
        "#/$defs/qosStatus",
    )?;
    ensure_str_eq(&schema, "/$defs/qosStatus/type", "object")?;
    ensure_bool_eq(&schema, "/$defs/qosStatus/additionalProperties", false)?;
    for field in QOS_STATUS_FIELDS {
        ensure_string_array_contains(&schema, "/$defs/qosStatus/required", field)?;
    }
    ensure_object_keys_eq(
        &schema,
        "/$defs/qosStatus/properties",
        &[
            "schema",
            "workspaceHash",
            "foregroundActiveCount",
            "backgroundActiveCount",
            "verificationActiveCount",
            "maintenanceActiveCount",
            "staleIgnoredCount",
            "foregroundPressure",
            "backgroundWorkActive",
            "registryHealthy",
            "activeRecords",
            "degraded",
        ],
    )?;
    ensure_str_eq(
        &schema,
        "/$defs/qosStatus/properties/schema/const",
        "ee.qos.active_lane_summary.v1",
    )?;
    for counter in [
        "foregroundActiveCount",
        "backgroundActiveCount",
        "verificationActiveCount",
        "maintenanceActiveCount",
        "staleIgnoredCount",
    ] {
        let type_pointer = format!("/$defs/qosStatus/properties/{counter}/type");
        let minimum_pointer = format!("/$defs/qosStatus/properties/{counter}/minimum");
        ensure_str_eq(&schema, &type_pointer, "integer")?;
        ensure_u64_eq(&schema, &minimum_pointer, 0)?;
    }

    Ok(())
}

#[test]
fn qos_doctor_schema_declares_compact_lane_summary() -> TestResult {
    let schema = read_json(DOCTOR_SCHEMA_PATH)?;

    ensure_string_array_contains(&schema, "/properties/data/required", "qos")?;
    ensure_string_array_contains(&schema, "/field_presets/summary", "qos")?;
    ensure_string_array_contains(&schema, "/field_presets/standard", "qos")?;
    ensure_str_eq(
        &schema,
        "/properties/data/properties/qos/$ref",
        "#/$defs/qosStatus",
    )?;
    ensure_str_eq(&schema, "/$defs/qosStatus/type", "object")?;
    ensure_bool_eq(&schema, "/$defs/qosStatus/additionalProperties", false)?;
    for field in QOS_STATUS_FIELDS {
        ensure_string_array_contains(&schema, "/$defs/qosStatus/required", field)?;
    }
    ensure_object_keys_eq(
        &schema,
        "/$defs/qosStatus/properties",
        &[
            "schema",
            "workspaceHash",
            "foregroundActiveCount",
            "backgroundActiveCount",
            "verificationActiveCount",
            "maintenanceActiveCount",
            "staleIgnoredCount",
            "foregroundPressure",
            "backgroundWorkActive",
            "registryHealthy",
            "activeRecords",
            "degraded",
        ],
    )?;
    ensure_str_eq(
        &schema,
        "/$defs/qosStatus/properties/schema/const",
        "ee.qos.active_lane_summary.v1",
    )
}

#[test]
fn read_pool_status_report_default_emits_zero_counters() -> TestResult {
    let report = ReadPoolStatusReport::default();
    if report.active != 0
        || report.idle != 0
        || report.active_pins != 0
        || report.expired_pins != 0
        || report.max_seen != 0
        || report.drops != 0
        || report.release_failures != 0
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
fn read_pool_status_report_preserves_lifecycle_pool_stats() -> TestResult {
    let report = ReadPoolStatusReport::from(PoolStats {
        active: 2,
        idle: 1,
        active_pins: 2,
        expired_pins: 1,
        max_size: 4,
        max_seen: 3,
        drops: 5,
        release_failures: 7,
        ad_hoc_bypass_count: 11,
        acquire_wait: AcquireWaitStats {
            samples: 13,
            p50_ns: 17,
            p99_ns: 19,
        },
        size_was_zero: false,
        checkpoint_blocked_by: None,
    });

    if report.active != 2
        || report.idle != 1
        || report.active_pins != 2
        || report.expired_pins != 1
        || report.max_seen != 3
        || report.drops != 5
        || report.release_failures != 7
        || report.ad_hoc_bypass_count != 11
        || report.acquire_wait.samples != 13
        || report.acquire_wait.p50_ns != 17
        || report.acquire_wait.p99_ns != 19
    {
        return Err(format!(
            "ReadPoolStatusReport must preserve pool lifecycle counters; got {report:?}"
        ));
    }

    Ok(())
}

#[test]
fn wal_status_report_preserves_sidecar_observability() -> TestResult {
    let report = WalStatusReport::from_wal_status(
        WalStatus {
            bytes: 4096,
            frames: 1,
            page_size: 1024,
        },
        2048,
    );

    if report.bytes != 4096
        || report.frames != 1
        || report.page_size != 1024
        || report.checkpoint_threshold_bytes != 2048
    {
        return Err(format!(
            "WalStatusReport must preserve sidecar observability counters; got {report:?}"
        ));
    }
    if !report.exceeds_threshold() {
        return Err("WAL report should flag bytes above checkpoint threshold".to_owned());
    }

    Ok(())
}

#[test]
fn rendered_status_json_includes_qos_posture_without_active_records_by_default() -> TestResult {
    let status = StatusReport::gather();
    let rendered = render_status_json(&status);
    let parsed: Value = serde_json::from_str(&rendered)
        .map_err(|error| format!("status JSON did not parse: {error}"))?;

    let qos = parsed
        .pointer("/data/qos")
        .ok_or_else(|| format!("data.qos missing from rendered status JSON: {parsed}"))?;
    ensure_object_keys_eq(&parsed, "/data/qos", QOS_STATUS_FIELDS)?;
    ensure_str_eq(qos, "/schema", "ee.qos.active_lane_summary.v1")?;
    for counter in [
        "foregroundActiveCount",
        "backgroundActiveCount",
        "verificationActiveCount",
        "maintenanceActiveCount",
        "staleIgnoredCount",
    ] {
        qos.pointer(&format!("/{counter}"))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("data.qos.{counter} not a u64"))?;
    }
    qos.pointer("/foregroundPressure")
        .and_then(Value::as_bool)
        .ok_or_else(|| "data.qos.foregroundPressure not a bool".to_owned())?;
    qos.pointer("/backgroundWorkActive")
        .and_then(Value::as_bool)
        .ok_or_else(|| "data.qos.backgroundWorkActive not a bool".to_owned())?;
    qos.pointer("/registryHealthy")
        .and_then(Value::as_bool)
        .ok_or_else(|| "data.qos.registryHealthy not a bool".to_owned())?;
    qos.pointer("/degraded")
        .and_then(Value::as_array)
        .ok_or_else(|| "data.qos.degraded not an array".to_owned())?;
    if qos.get("activeRecords").is_some() {
        return Err(
            "default status JSON should keep QoS compact and omit activeRecords".to_owned(),
        );
    }

    Ok(())
}

#[test]
fn rendered_doctor_json_includes_qos_posture_without_active_records_by_default() -> TestResult {
    let report = DoctorReport::gather();
    let rendered = render_doctor_json(&report);
    let parsed: Value = serde_json::from_str(&rendered)
        .map_err(|error| format!("doctor JSON did not parse: {error}"))?;

    let qos = parsed
        .pointer("/data/qos")
        .ok_or_else(|| format!("data.qos missing from rendered doctor JSON: {parsed}"))?;
    ensure_object_keys_eq(&parsed, "/data/qos", QOS_STATUS_FIELDS)?;
    ensure_str_eq(qos, "/schema", "ee.qos.active_lane_summary.v1")?;
    for counter in [
        "foregroundActiveCount",
        "backgroundActiveCount",
        "verificationActiveCount",
        "maintenanceActiveCount",
        "staleIgnoredCount",
    ] {
        qos.pointer(&format!("/{counter}"))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("data.qos.{counter} not a u64"))?;
    }
    qos.pointer("/foregroundPressure")
        .and_then(Value::as_bool)
        .ok_or_else(|| "data.qos.foregroundPressure not a bool".to_owned())?;
    qos.pointer("/backgroundWorkActive")
        .and_then(Value::as_bool)
        .ok_or_else(|| "data.qos.backgroundWorkActive not a bool".to_owned())?;
    qos.pointer("/registryHealthy")
        .and_then(Value::as_bool)
        .ok_or_else(|| "data.qos.registryHealthy not a bool".to_owned())?;
    qos.pointer("/degraded")
        .and_then(Value::as_array)
        .ok_or_else(|| "data.qos.degraded not an array".to_owned())?;
    if qos.get("activeRecords").is_some() {
        return Err(
            "default doctor JSON should keep QoS compact and omit activeRecords".to_owned(),
        );
    }

    Ok(())
}

#[test]
fn rendered_status_json_includes_read_pool_with_all_counters() -> TestResult {
    let mut status = StatusReport::gather();
    status.read_pool = ReadPoolStatusReport::from(PoolStats {
        active: 2,
        idle: 1,
        active_pins: 2,
        expired_pins: 1,
        max_size: 4,
        max_seen: 3,
        drops: 5,
        release_failures: 7,
        ad_hoc_bypass_count: 11,
        acquire_wait: AcquireWaitStats {
            samples: 13,
            p50_ns: 17,
            p99_ns: 19,
        },
        size_was_zero: false,
        checkpoint_blocked_by: None,
    });
    status.wal = WalStatusReport::from_wal_status(
        WalStatus {
            bytes: 4096,
            frames: 1,
            page_size: 1024,
        },
        2048,
    );
    let rendered = render_status_json(&status);
    let parsed: Value = serde_json::from_str(&rendered)
        .map_err(|error| format!("status JSON did not parse: {error}"))?;

    // The renderer must place `read_pool` under `data` exactly as the schema
    // says, with counters and the acquire wait summary present.
    let read_pool = parsed
        .pointer("/data/read_pool")
        .ok_or_else(|| format!("data.read_pool missing from rendered status JSON: {parsed}"))?;
    ensure_object_keys_eq(&parsed, "/data/read_pool", READ_POOL_FIELDS)?;
    for counter in [
        "active",
        "idle",
        "active_pins",
        "expired_pins",
        "max_seen",
        "drops",
        "release_failures",
        "ad_hoc_bypass_count",
    ] {
        let pointer = format!("/data/read_pool/{counter}");
        let value = read_pool
            .pointer(&format!("/{counter}"))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{pointer} not a u64"))?;
        if value > usize::MAX as u64 {
            return Err(format!("{pointer} overflowed usize range: {value}"));
        }
    }
    ensure_u64_eq(&parsed, "/data/read_pool/active", 2)?;
    ensure_u64_eq(&parsed, "/data/read_pool/idle", 1)?;
    ensure_u64_eq(&parsed, "/data/read_pool/active_pins", 2)?;
    ensure_u64_eq(&parsed, "/data/read_pool/expired_pins", 1)?;
    ensure_u64_eq(&parsed, "/data/read_pool/max_seen", 3)?;
    ensure_u64_eq(&parsed, "/data/read_pool/drops", 5)?;
    ensure_u64_eq(&parsed, "/data/read_pool/release_failures", 7)?;
    ensure_u64_eq(&parsed, "/data/read_pool/ad_hoc_bypass_count", 11)?;
    ensure_object_keys_eq(&parsed, "/data/read_pool/acquire_wait", ACQUIRE_WAIT_FIELDS)?;
    for counter in ACQUIRE_WAIT_FIELDS {
        let pointer = format!("/data/read_pool/acquire_wait/{counter}");
        read_pool
            .pointer(&format!("/acquire_wait/{counter}"))
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{pointer} not a u64"))?;
    }
    ensure_u64_eq(&parsed, "/data/read_pool/acquire_wait/samples", 13)?;
    ensure_u64_eq(&parsed, "/data/read_pool/acquire_wait/p50_ns", 17)?;
    ensure_u64_eq(&parsed, "/data/read_pool/acquire_wait/p99_ns", 19)?;
    ensure_object_keys_eq(&parsed, "/data/wal", WAL_FIELDS)?;
    ensure_u64_eq(&parsed, "/data/wal/bytes", 4096)?;
    ensure_u64_eq(&parsed, "/data/wal/frames", 1)?;
    ensure_u64_eq(&parsed, "/data/wal/page_size", 1024)?;
    ensure_u64_eq(&parsed, "/data/wal/checkpoint_threshold_bytes", 2048)?;

    Ok(())
}
