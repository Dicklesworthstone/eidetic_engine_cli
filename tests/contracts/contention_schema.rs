//! bd-d67os.11: structural contract for `ee.diag.contention.v1`.
//!
//! Pins the read-only contention-diagnostic wire shape, the schema/struct
//! agreement, and the `public_schemas()` registry wiring that the Track D CLI
//! leaf (bd-d67os.12) will emit. Follows the `ee.diag.plan_cache.v1` pattern
//! (co-located schema tag, registered in `public_schemas()`, not in
//! `KNOWN_SCHEMAS`). See ADR 0079.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::core::contention::{
    ContentionInputs, FlockGateInput, GroupCommitInput, IndexIntakeInput, L2CacheInput,
    build_contention_report,
};
use ee::core::write_owner::WriteOwnerStatus;
use ee::db::read_pool::{AcquireWaitStats, PoolStats};
use ee::models::contention::CONTENTION_DIAG_SCHEMA_V1;
use ee::models::singleflight::SingleFlightPostureReport;
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.diag.contention.v1.json";

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn load_json(relative: &str) -> Result<Value, String> {
    let path = repo_path(relative);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn string_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let array = node
        .as_array()
        .ok_or_else(|| format!("{pointer} must be a JSON array"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        let value = entry
            .as_str()
            .ok_or_else(|| format!("{pointer} contains non-string entry: {entry}"))?;
        out.insert(value.to_owned());
    }
    Ok(out)
}

#[test]
fn contention_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(CONTENTION_DIAG_SCHEMA_V1),
        "schema title must be ee.diag.contention.v1",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some("ee.response.v2"),
        "envelope schema.const must be ee.response.v2",
    )?;
    ensure(
        schema
            .pointer("/properties/data/properties/command/const")
            .and_then(Value::as_str)
            == Some("diag contention"),
        "data.command const must be 'diag contention'",
    )?;
    ensure(
        schema
            .pointer("/$defs/contentionReport/properties/schemaTag/const")
            .and_then(Value::as_str)
            == Some(CONTENTION_DIAG_SCHEMA_V1),
        "report schemaTag const must pin ee.diag.contention.v1",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with("ee.diag.contention.v1.json")),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/additionalProperties")
            .and_then(Value::as_bool)
            == Some(false),
        "envelope must forbid additional properties",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == CONTENTION_DIAG_SCHEMA_V1)
        .ok_or("public schema registry missing ee.diag.contention.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(entry.category == "ops", "registry category must be ops")?;
    let exported: Value =
        serde_json::from_str(&render_schema_export_json(Some(CONTENTION_DIAG_SCHEMA_V1)))
            .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(CONTENTION_DIAG_SCHEMA_V1),
        "registry definition must embed the contention schema",
    )
}

#[test]
fn contention_report_required_fields_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let required = string_set(&schema, "/$defs/contentionReport/required")?;
    let expected: BTreeSet<String> = [
        "schemaTag",
        "overallPosture",
        "writeLock",
        "readPool",
        "singleflight",
        "topContention",
        "unavailableSources",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("contentionReport required set drifted: {required:?}"),
    )
}

#[test]
fn builder_output_conforms_to_schema_shape() -> TestResult {
    // A report with all three core sources present and no future-feature
    // sub-reports: serialized keys must exactly equal the schema's required set.
    let report = build_contention_report(&ContentionInputs::default());
    let value = serde_json::to_value(&report).map_err(|e| format!("serialize report: {e}"))?;
    let object = value
        .as_object()
        .ok_or("serialized report must be an object")?;

    let keys: BTreeSet<String> = object.keys().cloned().collect();
    let required = string_set(&load_json(SCHEMA_REL)?, "/$defs/contentionReport/required")?;
    ensure(
        keys == required,
        format!("serialized report keys {keys:?} do not match schema required {required:?}"),
    )?;

    ensure(
        value.pointer("/schemaTag").and_then(Value::as_str) == Some(CONTENTION_DIAG_SCHEMA_V1),
        "schemaTag must be emitted",
    )?;
    ensure(
        value.pointer("/overallPosture").and_then(Value::as_str) == Some("ok"),
        "default inputs yield overall posture ok",
    )?;
    // Omit-safe future-feature sub-reports must be absent, not null.
    ensure(
        value.get("groupCommit").is_none()
            && value.get("indexIntake").is_none()
            && value.get("l2Cache").is_none()
            && value.get("flockGate").is_none(),
        "future-feature sub-reports must be omitted when absent",
    )?;
    // With no inputs, all three core sources are reported as unavailable gaps.
    let gaps = value
        .pointer("/unavailableSources")
        .and_then(Value::as_array)
        .ok_or("unavailableSources must be an array")?;
    ensure(
        gaps.len() == 3,
        format!(
            "expected 3 source gaps from empty inputs, got {}",
            gaps.len()
        ),
    )
}

// --- bd-d67os.13: collector determinism goldens, ranking stability, drift gate ---

/// A maximally-loaded scenario: every core source contended/hot, all three
/// future-feature sub-reports present and active. Constructed directly (not from
/// a live system) so the report is a pure, deterministic function of the inputs.
/// Float-valued outputs are chosen to land on exact binary fractions
/// (`0.75`, `0.5`, `2.0`, `3.5`) so the serialized golden round-trips cleanly.
fn fully_contended_inputs() -> ContentionInputs {
    ContentionInputs {
        write_owner: Some(WriteOwnerStatus {
            running: true,
            queue_depth: 40,
            total_processed: 1000,
            avg_wait_ms: 12.0,
            max_wait_ms: 5000,
            ..WriteOwnerStatus::default()
        }),
        lock_wait_ms_p50: Some(50),
        lock_wait_ms_p99: Some(1500),
        read_pool: Some(PoolStats {
            active: 8,
            idle: 0,
            active_pins: 2,
            expired_pins: 1,
            max_size: 8,
            max_seen: 8,
            drops: 3,
            release_failures: 1,
            ad_hoc_bypass_count: 5,
            acquire_wait: AcquireWaitStats {
                samples: 100,
                p50_ns: 2_000_000,
                p99_ns: 1_200_000_000,
            },
            size_was_zero: false,
            ..PoolStats::default()
        }),
        singleflight: Some(SingleFlightPostureReport {
            schema: "ee.singleflight.posture.v1".to_owned(),
            status: "observed_failures".to_owned(),
            configured_surface_count: 3,
            active_leader_count: 2,
            leader_start_count: 2,
            follower_wait_count: 8,
            follower_timeout_count: 2,
            leader_failure_count: 1,
            reused_result_count: 6,
            surfaces: Vec::new(),
        }),
        group_commit: Some(GroupCommitInput {
            enabled: true,
            batches: 4,
            writes_coalesced: 8,
            fsync_saved: 4,
        }),
        index_intake: Some(IndexIntakeInput {
            intake_mode: "full_rebuild".to_owned(),
            rebuilds: 5,
            swap_stalls: 2,
            avg_swap_ms: 3.5,
        }),
        l2_cache: Some(L2CacheInput {
            hits: 6,
            misses: 2,
            evictions: 4,
            inserts: 8,
        }),
        flock_gate: Some(FlockGateInput {
            acquires: 16,
            contended_acquires: 8,
            // 4 s across 16 acquires: avg lands on exactly 250.0 ms.
            wait_ns_total: 4_000_000_000,
            max_wait_ns: 2_500_000_000,
            timeouts: 2,
        }),
    }
}

/// Compare `actual` against a committed golden, or (re)write it when
/// `UPDATE_GOLDEN` is set or the file is missing. The comparison is over parsed
/// `serde_json::Value`s, so golden formatting/whitespace is irrelevant — only
/// the semantic content is pinned. Regenerate on a Linux/RCH worker only (these
/// goldens are path- and version-free, so they are portable, but keep the
/// canonical workflow consistent with the rest of the suite).
fn check_golden(relative: &str, actual: &Value) -> TestResult {
    let path = repo_path(relative);
    if std::env::var_os("UPDATE_GOLDEN").is_some() || !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        let pretty = serde_json::to_string_pretty(actual)
            .map_err(|error| format!("serialize golden: {error}"))?;
        std::fs::write(&path, format!("{pretty}\n"))
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        return Ok(());
    }
    let expected = load_json(relative)?;
    ensure(
        actual == &expected,
        format!("golden mismatch for {relative}:\n  expected={expected}\n  actual={actual}"),
    )
}

/// Recursively collect every posture-like string in the report (the values of
/// `overallPosture`, per-source `posture`, and per-finding `severity` keys).
fn collect_posture_strings(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if matches!(key.as_str(), "posture" | "overallPosture" | "severity")
                    && let Some(text) = child.as_str()
                {
                    out.insert(text.to_owned());
                }
                collect_posture_strings(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_posture_strings(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn fully_contended_report_matches_golden() -> TestResult {
    let report = build_contention_report(&fully_contended_inputs());
    let actual = serde_json::to_value(&report).map_err(|error| format!("serialize: {error}"))?;
    check_golden(
        "tests/fixtures/golden/contention/fully_contended.json",
        &actual,
    )
}

#[test]
fn all_sources_unavailable_report_matches_golden() -> TestResult {
    let report = build_contention_report(&ContentionInputs::default());
    let actual = serde_json::to_value(&report).map_err(|error| format!("serialize: {error}"))?;
    check_golden(
        "tests/fixtures/golden/contention/all_sources_unavailable.json",
        &actual,
    )
}

#[test]
fn top_contention_ranking_is_stable_and_deterministic() -> TestResult {
    // The ranked topContention list is the agent/robot triage surface: it must be
    // ordered severity-desc then source-asc, deterministically, regardless of the
    // order the underlying sources were folded in.
    let report = build_contention_report(&fully_contended_inputs());
    let ranked: Vec<(String, String, String)> = report
        .top_contention
        .iter()
        .map(|finding| {
            (
                finding.severity.as_str().to_owned(),
                finding.source.clone(),
                finding.reason_code.clone(),
            )
        })
        .collect();
    let expected = vec![
        (
            "contended".to_owned(),
            "read_pool".to_owned(),
            "read_pool_ad_hoc_bypass".to_owned(),
        ),
        (
            "contended".to_owned(),
            "write_lock".to_owned(),
            "write_lock_queue_backlog".to_owned(),
        ),
        (
            "hot".to_owned(),
            "index_intake".to_owned(),
            "index_swap_stalls".to_owned(),
        ),
        (
            "hot".to_owned(),
            "singleflight".to_owned(),
            "singleflight_follower_timeouts".to_owned(),
        ),
        (
            "warm".to_owned(),
            "l2_cache".to_owned(),
            "l2_cache_thrash".to_owned(),
        ),
    ];
    ensure(
        ranked == expected,
        format!("top_contention ranking drifted: {ranked:?}"),
    )?;
    // Severity is monotonically non-increasing down the ranked list.
    for window in report.top_contention.windows(2) {
        ensure(
            window[0].severity >= window[1].severity,
            "top_contention is not severity-sorted",
        )?;
    }
    // Rebuilding the same inputs is byte-identical (no map/iteration nondeterminism).
    let a = serde_json::to_string(&build_contention_report(&fully_contended_inputs()))
        .map_err(|error| error.to_string())?;
    let b = serde_json::to_string(&build_contention_report(&fully_contended_inputs()))
        .map_err(|error| error.to_string())?;
    ensure(a == b, "report serialization is non-deterministic")
}

#[test]
fn unavailable_source_codes_match_schema_degraded_enum() -> TestResult {
    // Schema-drift gate: the builder's gap codes for the three core sources must
    // stay in lockstep with the schema's degradedEntry.code enum.
    let schema = load_json(SCHEMA_REL)?;
    let schema_codes = string_set(&schema, "/$defs/degradedEntry/properties/code/enum")?;
    let report = build_contention_report(&ContentionInputs::default());
    let builder_codes: BTreeSet<String> = report
        .unavailable_sources
        .iter()
        .map(|gap| gap.code.clone())
        .collect();
    ensure(
        builder_codes == schema_codes,
        format!("gap codes {builder_codes:?} drifted from schema enum {schema_codes:?}"),
    )
}

#[test]
fn emitted_postures_are_in_schema_posture_enum() -> TestResult {
    // Every posture/severity string the collector can emit must be a member of
    // the schema's $defs/posture enum — a drift gate over the severity vocabulary.
    let schema = load_json(SCHEMA_REL)?;
    let allowed = string_set(&schema, "/$defs/posture/enum")?;
    let value = serde_json::to_value(build_contention_report(&fully_contended_inputs()))
        .map_err(|error| format!("serialize: {error}"))?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    collect_posture_strings(&value, &mut seen);
    ensure(!seen.is_empty(), "expected at least one posture string")?;
    ensure(
        seen.iter().all(|posture| allowed.contains(posture)),
        format!("emitted postures {seen:?} are not all in schema enum {allowed:?}"),
    )
}
