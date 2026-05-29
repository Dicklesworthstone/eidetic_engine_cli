//! bd-1zwi4: structural contract for the swarmx.perf-live snapshot
//! schema `docs/schemas/ee.perf.live.v1.json`.
//!
//! The implementation surface (src/core/perf_live.rs ~24KB; `ee perf
//! live --json` long-running stdout snapshots; `ee perf snapshot`
//! one-shot variant) is already on main. This contract pins the wire
//! shape so a future change to the snapshot row that drops or renames
//! a required surface trips the contract before reaching review.
//!
//! The 5 instrumented surfaces (context, search, remember, why,
//! pack_build → camelCased packBuild) are the bead acceptance's
//! exact named set. Adding more is allowed in a minor revision;
//! removing any breaks consumers that exhaustively switch on the
//! closed set.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.perf.live.v1.json";
const SCHEMA_NAME: &str = "ee.perf.live.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "ts",
    "intervalMs",
    "sideEffectFree",
    "redactionStatus",
    "beadId",
    "surfaces",
    "readPool",
    "auditLane",
    "l2Cache",
    "rch",
    "graphSnapshot",
    "hostPressure",
    "beadActivity",
    "degraded",
];

const REQUIRED_SURFACES: &[&str] = &["context", "packBuild", "remember", "search", "why"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn load_schema() -> Result<Value, String> {
    let path = repo_root().join(SCHEMA_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn collect_strings(node: &Value, ctx: &str) -> Result<Vec<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got: {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry: {value}"))
        })
        .collect()
}

#[test]
fn perf_live_v1_schema_has_expected_envelope() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["properties"]["schema"]["const"] == SCHEMA_NAME,
        "properties.schema.const must equal ee.perf.live.v1",
    )?;
    ensure(
        schema["properties"]["sideEffectFree"]["const"] == Value::Bool(true),
        "sideEffectFree must be const true (snapshot is read-only)",
    )?;
    let required = collect_strings(&schema["required"], "top-level required")?;
    for field in REQUIRED_TOP_LEVEL {
        ensure(
            required.iter().any(|r| r == field),
            format!("required missing `{field}`; got {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn perf_live_v1_surfaces_cover_five_instrumented_command_families() -> TestResult {
    let schema = load_schema()?;
    let surfaces = schema["properties"]["surfaces"]["properties"]
        .as_object()
        .ok_or_else(|| "surfaces.properties not an object".to_string())?;
    let surface_keys: Vec<String> = surfaces.keys().cloned().collect();
    for surface in REQUIRED_SURFACES {
        ensure(
            surface_keys.iter().any(|k| k == surface),
            format!(
                "surfaces.properties must include `{surface}` per the bead's named \
                 set (context / search / remember / why / packBuild); got: {surface_keys:?}"
            ),
        )?;
    }
    Ok(())
}

/// Resolve a JSON-schema `$ref` (e.g. `#/$defs/surface`) against the
/// root schema. The surface schema was refactored to share a single
/// `$defs/surface` definition via `$ref`, so per-surface required
/// fields and observability sub-surface required arrays no longer live
/// inline. Tests must dereference before inspecting `required`.
fn resolve_ref<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        // Strip the leading `#/` and split on `/` to walk the pointer.
        let mut current = root;
        if let Some(path) = reference.strip_prefix("#/") {
            for segment in path.split('/') {
                current = &current[segment];
            }
            return current;
        }
    }
    node
}

#[test]
fn perf_live_v1_each_surface_carries_latency_percentiles_and_qps() -> TestResult {
    let schema = load_schema()?;
    let surfaces = schema["properties"]["surfaces"]["properties"]
        .as_object()
        .ok_or_else(|| "surfaces.properties not an object".to_string())?;
    let required_fields = ["p50Ms", "p95Ms", "p99Ms", "p999Ms", "qps", "inflight"];
    for surface in REQUIRED_SURFACES {
        let surface_schema = surfaces
            .get(*surface)
            .ok_or_else(|| format!("surface `{surface}` not present"))?;
        let resolved = resolve_ref(&schema, surface_schema);
        let surface_required = collect_strings(
            &resolved["required"],
            &format!("surfaces.{surface}.required"),
        )?;
        for field in &required_fields {
            ensure(
                surface_required.iter().any(|r| r == field),
                format!(
                    "surface `{surface}` must require `{field}` per the bead's latency-\
                     percentile contract; got: {surface_required:?}"
                ),
            )?;
        }
    }
    Ok(())
}

#[test]
fn perf_live_v1_observability_subsurfaces_are_present() -> TestResult {
    let schema = load_schema()?;
    let observability = [
        (
            "readPool",
            &["activePins", "expiredPins", "releaseFailures", "queueDepth"][..],
        ),
        (
            "auditLane",
            &["batchCount", "backpressureEvents", "channelDepth"][..],
        ),
        (
            "l2Cache",
            // `hitRate` was renamed to `hitRateBasisPoints` (integer
            // representation of the ratio, 0..=10_000) to avoid
            // floating-point drift across runs.
            &[
                "hits",
                "misses",
                "hitRateBasisPoints",
                "byteSize",
                "evictions",
            ][..],
        ),
        (
            "rch",
            &["workersHealthy", "slotsAvailable", "queueDepth"][..],
        ),
        ("graphSnapshot", &["ageMs", "refreshedCount"][..]),
        ("hostPressure", &["cpuUserPct", "memoryRssMb"][..]),
        (
            "beadActivity",
            &["activeAgents", "readyBeads", "inProgressBeads"][..],
        ),
    ];
    for (block, required_fields) in observability {
        let block_schema = &schema["properties"][block];
        let resolved = resolve_ref(&schema, block_schema);
        let block_required = collect_strings(
            &resolved["required"],
            &format!("properties.{block}.required"),
        )?;
        for field in required_fields {
            ensure(
                block_required.iter().any(|r| r == field),
                format!(
                    "properties.{block}.required must include `{field}` per the bead's \
                     observability contract; got: {block_required:?}"
                ),
            )?;
        }
    }
    Ok(())
}
