//! bd-ssoco.3: scale-envelope collectors derived from live status posture.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use ee::core::status::{
    ReadPoolAcquireWaitReport, ReadPoolStatusReport, ScaleEnvelopeCollectorInput,
    ScaleEnvelopeIndexPosture, ScaleEnvelopeIndexSubsystem, ScaleEnvelopeReport,
    ScaleEnvelopeStoreProbe, WalStatusReport,
};
use ee::models::{
    SCALE_ENVELOPE_SCHEMA_V1, SCALE_POSTURE_THRASHING_CODE, SCALE_POSTURE_WARMING_CODE,
    SCALE_PROBE_BUDGET_EXCEEDED_CODE,
};
use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn base_input() -> ScaleEnvelopeCollectorInput {
    ScaleEnvelopeCollectorInput {
        generated_at: "2026-06-15T04:22:00Z".to_owned(),
        workspace_fingerprint: "abcdef1234567890".to_owned(),
        source: "live_probe",
        fixture_profile_id: None,
        memory_count: 128,
        link_count: 384,
        pack_count: 8,
        search_document_count: 128,
        storage_state: "healthy",
        store_probe: ScaleEnvelopeStoreProbe::from_parts(8192, 2, 4096, 0),
        read_pool: ReadPoolStatusReport {
            active: 0,
            idle: 4,
            active_pins: 0,
            expired_pins: 0,
            max_seen: 4,
            drops: 0,
            release_failures: 0,
            ad_hoc_bypass_count: 0,
            acquire_wait: ReadPoolAcquireWaitReport {
                samples: 0,
                p50_ns: 0,
                p99_ns: 0,
            },
            checkpoint_blocked_by: None,
        },
        wal: WalStatusReport {
            bytes: 0,
            frames: 0,
            page_size: 4096,
            checkpoint_threshold_bytes: 64 * 1024 * 1024,
        },
        index_posture: ScaleEnvelopeIndexPosture::new(
            ScaleEnvelopeIndexSubsystem::new(
                "fresh",
                Some(128),
                0,
                Some("2026-06-15T04:00:00Z".to_owned()),
            ),
            ScaleEnvelopeIndexSubsystem::new(
                "fresh",
                Some(128),
                0,
                Some("2026-06-15T04:00:00Z".to_owned()),
            ),
            ScaleEnvelopeIndexSubsystem::new(
                "fresh",
                Some(384),
                0,
                Some("2026-06-15T04:00:00Z".to_owned()),
            ),
        ),
        page_cache_bytes: 4096,
        page_faults_pre: 0,
        page_faults_post: 0,
    }
}

fn report(input: ScaleEnvelopeCollectorInput) -> Value {
    ScaleEnvelopeReport::from_collector_input(input).into_json()
}

fn degraded_codes(value: &Value) -> Result<BTreeSet<String>, String> {
    let array = value
        .pointer("/degradedCodes")
        .and_then(Value::as_array)
        .ok_or("missing degradedCodes array")?;
    Ok(array
        .iter()
        .filter_map(|entry| entry.get("code").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}

fn recovery_kinds(value: &Value) -> Result<BTreeSet<String>, String> {
    let array = value
        .pointer("/recoveryActions")
        .and_then(Value::as_array)
        .ok_or("missing recoveryActions array")?;
    Ok(array
        .iter()
        .filter_map(|entry| entry.get("kind").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}

#[test]
fn healthy_status_parts_emit_warm_scale_envelope_without_degraded_codes() -> TestResult {
    let value = report(base_input());
    ensure(
        value.pointer("/schema").and_then(Value::as_str) == Some(SCALE_ENVELOPE_SCHEMA_V1),
        "schema must be ee.scale_envelope.v1",
    )?;
    ensure(
        value.pointer("/redactionStatus").and_then(Value::as_str)
            == Some("counts_hashes_paths_no_content"),
        "redaction status drifted",
    )?;
    ensure(
        value
            .pointer("/pageCacheWalPosture/cacheState")
            .and_then(Value::as_str)
            == Some("warm"),
        "page cache should be warm when lexical bytes are retained",
    )?;
    ensure(
        value
            .pointer("/pageCacheWalPosture/walState")
            .and_then(Value::as_str)
            == Some("clean"),
        "empty WAL should be clean",
    )?;
    ensure(
        degraded_codes(&value)?.is_empty(),
        "healthy envelope should not emit degraded codes",
    )?;
    ensure(
        recovery_kinds(&value)?.is_empty(),
        "healthy envelope should not emit recovery actions",
    )
}

#[test]
fn stale_index_parts_emit_warming_code_and_rebuild_recovery() -> TestResult {
    let mut input = base_input();
    input.page_cache_bytes = 0;
    input.index_posture.lexical = ScaleEnvelopeIndexSubsystem::new(
        "stale",
        Some(110),
        18,
        Some("2026-06-15T03:59:00Z".to_owned()),
    );
    input.index_posture.semantic = input.index_posture.lexical.clone();

    let value = report(input);
    ensure(
        value
            .pointer("/pageCacheWalPosture/cacheState")
            .and_then(Value::as_str)
            == Some("warming"),
        "stale index should make cache posture warming",
    )?;
    ensure(
        degraded_codes(&value)?.contains(SCALE_POSTURE_WARMING_CODE),
        "warming posture should emit scale_posture_warming",
    )?;
    let recovery = recovery_kinds(&value)?;
    ensure(
        recovery.contains("rebuild_index"),
        "stale index should recommend rebuild_index",
    )?;
    ensure(
        recovery.contains("warm_cache"),
        "cold derived assets should recommend warm_cache",
    )
}

#[test]
fn wal_threshold_and_saturated_read_pool_emit_thrashing_posture() -> TestResult {
    let mut input = base_input();
    input.wal.bytes = input.wal.checkpoint_threshold_bytes + 1;
    input.wal.frames = 20_000;
    input.read_pool.active = 2;
    input.read_pool.active_pins = 2;
    input.read_pool.max_seen = 2;

    let value = report(input);
    ensure(
        value
            .pointer("/pageCacheWalPosture/cacheState")
            .and_then(Value::as_str)
            == Some("thrashing"),
        "WAL threshold pressure should make cache posture thrashing",
    )?;
    ensure(
        value
            .pointer("/pageCacheWalPosture/walState")
            .and_then(Value::as_str)
            == Some("checkpoint_recommended"),
        "oversized WAL should recommend checkpoint posture",
    )?;
    ensure(
        degraded_codes(&value)?.contains(SCALE_POSTURE_THRASHING_CODE),
        "thrashing posture should emit scale_posture_thrashing",
    )?;
    ensure(
        recovery_kinds(&value)?.contains("checkpoint_wal"),
        "oversized WAL should recommend checkpoint_wal",
    )
}

#[test]
fn unavailable_probe_parts_emit_partial_evidence_code_and_support_recovery() -> TestResult {
    let mut input = base_input();
    input.storage_state = "unknown";
    input.wal.page_size = 0;
    input.page_cache_bytes = 0;
    input.index_posture = ScaleEnvelopeIndexPosture::new(
        ScaleEnvelopeIndexSubsystem::new("unknown", None, 0, None),
        ScaleEnvelopeIndexSubsystem::new("unavailable", None, 0, None),
        ScaleEnvelopeIndexSubsystem::new("unknown", None, 0, None),
    );

    let value = report(input);
    ensure(
        value
            .pointer("/pageCacheWalPosture/cacheState")
            .and_then(Value::as_str)
            == Some("unknown"),
        "unavailable sources should keep cache posture unknown",
    )?;
    ensure(
        value
            .pointer("/indexPosture/semantic/state")
            .and_then(Value::as_str)
            == Some("unavailable"),
        "semantic index should preserve unavailable state",
    )?;
    ensure(
        degraded_codes(&value)?.contains(SCALE_PROBE_BUDGET_EXCEEDED_CODE),
        "partial probes should emit scale_probe_budget_exceeded",
    )?;
    ensure(
        recovery_kinds(&value)?.contains("inspect_support_bundle"),
        "unknown probe source should recommend inspect_support_bundle",
    )
}
