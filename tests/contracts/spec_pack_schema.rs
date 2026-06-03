//! bd-20bdb: structural contract for the swarmx.spec-pack telemetry
//! schema `docs/schemas/ee.spec_pack.v1.json`.
//!
//! The driver (src/steward/spec_pack.rs + src/core/recent_queries.rs +
//! ee steward prewarm-packs subcommand) lands in follow-up slices; this
//! contract pins the wire shape so the QoS-gated admission gate, the
//! L2 cache integration, and the proptest property test can all
//! compose against a stable telemetry row.
//!
//! Five phases pinned exactly: admission | prepare | run | abort |
//! store. The admissionVerdict enum encodes the bead's four hard QoS
//! gates literally so a regression that fires speculative work while
//! foreground pressure is active cannot validate. The abortReason enum
//! covers the cooperative-cancellation cases including the primary
//! `foreground_request_arrived` (a foreground ee context Cx kills
//! in-flight speculative work via the Cx tree).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.spec_pack.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.spec_pack.v1.json";
const SCHEMA_NAME: &str = "ee.spec_pack.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "sideEffectFree",
    "phase",
    "ts",
    "workspaceIdHash",
    "queryShapeHash",
    "qosLaneSnapshotHash",
    "admissionVerdict",
];

const REQUIRED_PHASES: &[&str] = &["admission", "prepare", "run", "abort", "store"];

const REQUIRED_ADMISSION_VERDICTS: &[&str] = &[
    "admitted",
    "denied_foreground_request_id_active",
    "denied_read_pool_foreground_pin_held",
    "denied_qos_foreground_pressure",
    "denied_per_workspace_concurrency_cap",
];

const REQUIRED_ABORT_REASONS: &[&str] = &[
    "foreground_request_arrived",
    "shutdown",
    "memory_pressure",
    "ttl_expired_before_store",
    "selection_no_longer_top_k",
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

#[test]
fn spec_pack_v1_schema_has_expected_envelope() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["$id"] == SCHEMA_ID,
        format!("expected `$id` = {SCHEMA_ID}; got: {}", schema["$id"]),
    )?;
    ensure(
        schema["title"] == SCHEMA_NAME,
        format!("expected `title` = {SCHEMA_NAME}; got: {}", schema["title"]),
    )?;
    let schema_const = &schema["properties"]["schema"]["const"];
    ensure(
        schema_const == SCHEMA_NAME,
        format!("expected properties.schema.const = {SCHEMA_NAME}; got: {schema_const}"),
    )?;
    let side_effect = &schema["properties"]["sideEffectFree"]["const"];
    ensure(
        side_effect == &Value::Bool(true),
        format!(
            "spec_pack telemetry schema must declare sideEffectFree const true; got: {side_effect}"
        ),
    )?;
    let actual = collect_string_set(&schema["required"], "top-level required")?;
    let expected = REQUIRED_TOP_LEVEL
        .iter()
        .map(|field| (*field).to_owned())
        .collect::<BTreeSet<_>>();
    ensure(
        actual == expected,
        format!(
            "REQUIRED_TOP_LEVEL drifted from schema required array\nexpected={expected:?}\nactual={actual:?}"
        ),
    )
}

#[test]
fn spec_pack_v1_phases_are_pinned_exactly() -> TestResult {
    let schema = load_schema()?;
    let values = collect_strings(&schema["$defs"]["phase"]["enum"], "phase.enum")?;
    ensure(
        values.len() == REQUIRED_PHASES.len()
            && REQUIRED_PHASES
                .iter()
                .all(|p| values.iter().any(|v| v == p)),
        format!(
            "phase enum must be exactly {REQUIRED_PHASES:?}; got: {values:?}. \
             Adding more phases is allowed in a minor revision; removing any \
             breaks consumers that switch on the closed set."
        ),
    )?;
    Ok(())
}

#[test]
fn spec_pack_v1_admission_verdict_covers_all_four_hard_qos_gates() -> TestResult {
    let schema = load_schema()?;
    let values = collect_strings(
        &schema["$defs"]["admissionVerdict"]["enum"],
        "admissionVerdict.enum",
    )?;
    for verdict in REQUIRED_ADMISSION_VERDICTS {
        ensure(
            values.iter().any(|v| v == verdict),
            format!(
                "admissionVerdict enum must include `{verdict}` so the bead's \
                 hard QoS gates are encoded literally; got: {values:?}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn spec_pack_v1_abort_reason_includes_foreground_request_arrived() -> TestResult {
    let schema = load_schema()?;
    let values = collect_strings(&schema["$defs"]["abortReason"]["enum"], "abortReason.enum")?;
    for reason in REQUIRED_ABORT_REASONS {
        ensure(
            values.iter().any(|v| v == reason),
            format!(
                "abortReason enum must include `{reason}` per the bead's cooperative-\
                 cancellation contract; got: {values:?}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn spec_pack_v1_workspace_and_query_are_hashed_not_raw() -> TestResult {
    let schema = load_schema()?;
    for field in ["workspaceIdHash", "queryShapeHash", "qosLaneSnapshotHash"] {
        let pattern = schema["properties"][field]["pattern"]
            .as_str()
            .ok_or_else(|| format!("missing pattern on properties.{field}"))?;
        ensure(
            pattern.contains("blake3:"),
            format!(
                "properties.{field}.pattern must require blake3 prefix so raw \
                 workspace paths / query text never appear in telemetry; got: {pattern}"
            ),
        )?;
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or_else(|| "top-level properties not an object".to_string())?;
    for forbidden in [
        "workspaceId",
        "workspacePath",
        "query",
        "rawQuery",
        "queryText",
    ] {
        ensure(
            !properties.contains_key(forbidden),
            format!(
                "spec_pack telemetry must not carry raw `{forbidden}` — only \
                 hashed identifiers are allowed."
            ),
        )?;
    }
    Ok(())
}
