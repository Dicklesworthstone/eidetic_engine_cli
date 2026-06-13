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

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ee::core::perf_live::{PerfLiveOptions, collect_perf_live_snapshot};
use ee::core::swarm_brief::{
    SwarmBriefCommandError, SwarmBriefCommandOutput, SwarmBriefCommandRunner,
};
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
const REQUIRED_SURFACE_FIELDS: &[&str] = &[
    "surface",
    "p50Ms",
    "p95Ms",
    "p99Ms",
    "p999Ms",
    "qps",
    "inflight",
    "qosClassCounts",
];
const NULLABLE_SURFACE_METRIC_FIELDS: &[(&str, &str)] = &[
    ("p50Ms", "integer"),
    ("p95Ms", "integer"),
    ("p99Ms", "integer"),
    ("p999Ms", "integer"),
    ("qps", "number"),
    ("inflight", "integer"),
];
const READ_POOL_REQUIRED_FIELDS: &[&str] =
    &["activePins", "expiredPins", "releaseFailures", "queueDepth"];
const AUDIT_LANE_REQUIRED_FIELDS: &[&str] = &[
    "batchCount",
    "batchSizeP50",
    "batchSizeP99",
    "backpressureEvents",
    "channelDepth",
];
const NULLABLE_AUDIT_LANE_COUNTER_FIELDS: &[&str] = &[
    "batchCount",
    "batchSizeP50",
    "batchSizeP99",
    "backpressureEvents",
    "channelDepth",
];
const L2_CACHE_REQUIRED_FIELDS: &[&str] = &[
    "status",
    "hits",
    "misses",
    "hitRateBasisPoints",
    "byteSize",
    "evictions",
];
const NULLABLE_L2_CACHE_METRIC_FIELDS: &[&str] = &[
    "hits",
    "misses",
    "hitRateBasisPoints",
    "byteSize",
    "evictions",
];
const RCH_REQUIRED_FIELDS: &[&str] = &[
    "workersHealthy",
    "slotsAvailable",
    "queueDepth",
    "headOfLineAgeMs",
];
const GRAPH_SNAPSHOT_REQUIRED_FIELDS: &[&str] =
    &["ageMs", "refreshedCount", "refreshLockWaitMsP99"];
const NULLABLE_GRAPH_SNAPSHOT_METRIC_FIELDS: &[&str] = &["refreshLockWaitMsP99"];
const HOST_PRESSURE_REQUIRED_FIELDS: &[&str] = &[
    "cpuUserPct",
    "cpuIowaitPct",
    "memoryRssMb",
    "pageCacheMb",
    "fsyncLatencyP99Ms",
];
const BEAD_ACTIVITY_REQUIRED_FIELDS: &[&str] = &[
    "activeAgents",
    "readyBeads",
    "inProgressBeads",
    "blockedBeads",
];
const OBSERVABILITY_BLOCKS: &[(&str, &[&str])] = &[
    ("readPool", READ_POOL_REQUIRED_FIELDS),
    ("auditLane", AUDIT_LANE_REQUIRED_FIELDS),
    ("l2Cache", L2_CACHE_REQUIRED_FIELDS),
    ("rch", RCH_REQUIRED_FIELDS),
    ("graphSnapshot", GRAPH_SNAPSHOT_REQUIRED_FIELDS),
    ("hostPressure", HOST_PRESSURE_REQUIRED_FIELDS),
    ("beadActivity", BEAD_ACTIVITY_REQUIRED_FIELDS),
];

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

fn collect_string_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    Ok(collect_strings(node, ctx)?.into_iter().collect())
}

fn expected_string_set(expected: &[&str]) -> BTreeSet<String> {
    expected.iter().map(|field| (*field).to_owned()).collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CrossReviewPerfLiveRunner;

impl SwarmBriefCommandRunner for CrossReviewPerfLiveRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        _cwd: &Path,
        _timeout_ms: u64,
    ) -> Result<SwarmBriefCommandOutput, SwarmBriefCommandError> {
        let stdout = match (program, args) {
            ("rch", ["status", "--workers", "--jobs", "--json"]) => {
                r#"{"workersHealthy":1,"slotsAvailable":1,"queueDepth":0,"headOfLineAgeMs":0}"#
            }
            ("br", _) => "[]",
            _ => {
                return Err(SwarmBriefCommandError::Unavailable(format!(
                    "unexpected perf-live contract command: {program} {}",
                    args.join(" ")
                )));
            }
        };

        Ok(SwarmBriefCommandOutput {
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }
}

fn ensure_exact_required_fields(schema: &Value, expected: &[&str], ctx: &str) -> TestResult {
    let required_ctx = format!("{ctx}.required");
    let actual = collect_string_set(&schema["required"], &required_ctx)?;
    let expected = expected_string_set(expected);
    ensure(
        actual == expected,
        format!(
            "{ctx}.required drifted from expected fields\nexpected={expected:?}\nactual={actual:?}"
        ),
    )
}

fn collect_schema_type_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    if node.is_array() {
        return collect_string_set(node, ctx);
    }
    let text = node
        .as_str()
        .ok_or_else(|| format!("{ctx}: expected string or array, got: {node}"))?;
    Ok(BTreeSet::from([text.to_owned()]))
}

fn ensure_schema_type_set(node: &Value, expected: &[&str], ctx: impl AsRef<str>) -> TestResult {
    let ctx = ctx.as_ref();
    let actual = collect_schema_type_set(&node["type"], &format!("{ctx}.type"))?;
    let expected = expected_string_set(expected);
    ensure(
        actual == expected,
        format!("{ctx}.type drifted\nexpected={expected:?}\nactual={actual:?}"),
    )
}

#[test]
fn cross_review_perf_live_snapshot_reports_real_fsync_latency() -> TestResult {
    let mut options = PerfLiveOptions::for_workspace(repo_root());
    options.command_timeout_ms = 1;
    options.timestamp_override = Some("2026-06-13T00:00:00.000Z".to_owned());

    let snapshot = collect_perf_live_snapshot(&options, &CrossReviewPerfLiveRunner);
    let measured = snapshot.host_pressure.fsync_latency_p99_ms.ok_or_else(|| {
        "perf-live hostPressure must include a real fsync latency sample".to_string()
    })?;
    ensure(
        measured < 5_000,
        format!("single-sample fsync probe should be bounded; got {measured}ms"),
    )
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
    ensure_exact_required_fields(&schema, REQUIRED_TOP_LEVEL, "top-level perf-live schema")
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
    ensure_exact_required_fields(
        &schema["properties"]["surfaces"],
        REQUIRED_SURFACES,
        "surfaces",
    )?;
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
    for surface in REQUIRED_SURFACES {
        let surface_schema = surfaces
            .get(*surface)
            .ok_or_else(|| format!("surface `{surface}` not present"))?;
        let resolved = resolve_ref(&schema, surface_schema);
        ensure_exact_required_fields(
            resolved,
            REQUIRED_SURFACE_FIELDS,
            &format!("surfaces.{surface}"),
        )?;
    }
    Ok(())
}

#[test]
fn surface_metrics_allow_explicit_null_when_unmeasured() -> TestResult {
    let schema = load_schema()?;
    let surface_schema = resolve_ref(
        &schema,
        &schema["properties"]["surfaces"]["properties"]["context"],
    );
    for &(field, numeric_type) in NULLABLE_SURFACE_METRIC_FIELDS {
        ensure_schema_type_set(
            &surface_schema["properties"][field],
            &[numeric_type, "null"],
            format!("$defs.surface.properties.{field}"),
        )?;
    }
    Ok(())
}

#[test]
fn audit_lane_counters_allow_explicit_null_when_unmeasured() -> TestResult {
    let schema = load_schema()?;
    let audit_lane_schema = resolve_ref(&schema, &schema["properties"]["auditLane"]);
    for field in NULLABLE_AUDIT_LANE_COUNTER_FIELDS {
        ensure_schema_type_set(
            &audit_lane_schema["properties"][field],
            &["integer", "null"],
            format!("$defs.auditLane.properties.{field}"),
        )?;
    }
    Ok(())
}

#[test]
fn l2_cache_metrics_allow_explicit_null_when_unmeasured() -> TestResult {
    let schema = load_schema()?;
    let l2_cache_schema = resolve_ref(&schema, &schema["properties"]["l2Cache"]);
    for field in NULLABLE_L2_CACHE_METRIC_FIELDS {
        ensure_schema_type_set(
            &l2_cache_schema["properties"][field],
            &["integer", "null"],
            format!("$defs.l2Cache.properties.{field}"),
        )?;
    }
    Ok(())
}

#[test]
fn graph_snapshot_metrics_allow_explicit_null_when_unmeasured() -> TestResult {
    let schema = load_schema()?;
    let graph_snapshot_schema = resolve_ref(&schema, &schema["properties"]["graphSnapshot"]);
    for field in NULLABLE_GRAPH_SNAPSHOT_METRIC_FIELDS {
        ensure_schema_type_set(
            &graph_snapshot_schema["properties"][field],
            &["integer", "null"],
            format!("$defs.graphSnapshot.properties.{field}"),
        )?;
    }
    Ok(())
}

#[test]
fn perf_live_v1_observability_subsurfaces_are_present() -> TestResult {
    let schema = load_schema()?;
    for (block, required_fields) in OBSERVABILITY_BLOCKS {
        let block_schema = &schema["properties"][block];
        let resolved = resolve_ref(&schema, block_schema);
        ensure_exact_required_fields(resolved, required_fields, &format!("properties.{block}"))?;
    }
    Ok(())
}
