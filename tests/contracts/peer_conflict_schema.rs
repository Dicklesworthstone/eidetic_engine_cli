//! bd-2w02i: structural contract for the SRR6.37 peer duplicate /
//! near-duplicate / contradiction event schema.
//!
//! The detector implementation (content-hash dedupe, SimHash near-
//! duplicate scoring, contradiction-signal heuristics, `ee why` and
//! `ee curate apply` integration, two-peer e2e) lands in follow-up
//! slices once bd-37ptl and bd-wl4ja close. This contract pins the
//! wire shape now so the detection logic and the rendering surface
//! compose against a stable event row.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.peer_conflict.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.peer_conflict.v1.json";
const SCHEMA_NAME: &str = "ee.peer_conflict.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "sideEffectFree",
    "kind",
    "ts",
    "workspaceIdHash",
    "primaryMemoryHash",
    "peerMemoryHashes",
    "trustClasses",
    "detectorVerdict",
];

const REQUIRED_KINDS: &[&str] = &[
    "duplicate_detected",
    "near_duplicate_candidate",
    "contradiction_candidate",
    "selected_with_conflict",
    "promotion_blocked_by_conflict",
];

const REQUIRED_DETECTOR_VERDICTS: &[&str] = &[
    "exact_duplicate",
    "near_duplicate",
    "contradiction",
    "co_present_with_conflict",
    "promotion_blocked",
];

const REQUIRED_CONTRADICTION_SIGNALS: &[&str] = &[
    "claim_negation_overlap",
    "rule_predicate_inversion",
    "trust_class_disagreement_at_same_revision",
    "explicit_supersession_chain",
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
fn peer_conflict_v1_schema_has_expected_envelope() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["$id"] == SCHEMA_ID,
        format!("expected $id={SCHEMA_ID}; got: {}", schema["$id"]),
    )?;
    ensure(
        schema["title"] == SCHEMA_NAME,
        format!("expected title={SCHEMA_NAME}; got: {}", schema["title"]),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == SCHEMA_NAME,
        "properties.schema.const must equal ee.peer_conflict.v1",
    )?;
    ensure(
        schema["properties"]["sideEffectFree"]["const"] == Value::Bool(true),
        "sideEffectFree must be const true (detector is read-only)",
    )?;
    let required = collect_strings(&schema["required"], "top-level required")?;
    for field in REQUIRED_TOP_LEVEL {
        ensure(
            required.iter().any(|r| r == field),
            format!("required missing {field}; got {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn peer_conflict_v1_required_matrix_matches_schema_required_array() -> TestResult {
    let schema = load_schema()?;
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
fn peer_conflict_v1_kinds_cover_all_four_structured_log_codes_plus_promotion_block() -> TestResult {
    let schema = load_schema()?;
    let values = collect_strings(
        &schema["$defs"]["conflictKind"]["enum"],
        "conflictKind.enum",
    )?;
    for kind in REQUIRED_KINDS {
        ensure(
            values.iter().any(|v| v == kind),
            format!(
                "conflictKind enum must include `{kind}` per the bead's structured-log \
                 contract (duplicate_detected / near_duplicate_candidate / \
                 contradiction_candidate / selected_with_conflict / \
                 promotion_blocked_by_conflict); got: {values:?}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn peer_conflict_v1_detector_verdicts_are_exhaustive() -> TestResult {
    let schema = load_schema()?;
    let values = collect_strings(
        &schema["$defs"]["detectorVerdict"]["enum"],
        "detectorVerdict.enum",
    )?;
    for verdict in REQUIRED_DETECTOR_VERDICTS {
        ensure(
            values.iter().any(|v| v == verdict),
            format!("detectorVerdict missing {verdict}; got {values:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn peer_conflict_v1_contradiction_signals_cover_documented_heuristics() -> TestResult {
    let schema = load_schema()?;
    let signal_enum = &schema["properties"]["contradictionScore"]["properties"]["signal"]["enum"];
    let values = collect_strings(signal_enum, "contradictionScore.signal.enum")?;
    for signal in REQUIRED_CONTRADICTION_SIGNALS {
        ensure(
            values.iter().any(|v| v == signal),
            format!(
                "contradictionScore.signal must include `{signal}` so the deterministic \
                 detector heuristics are encoded literally; got: {values:?}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn peer_conflict_v1_memory_refs_are_hashed_not_raw() -> TestResult {
    let schema = load_schema()?;
    let primary = schema["properties"]["primaryMemoryHash"]["pattern"]
        .as_str()
        .ok_or_else(|| "primaryMemoryHash.pattern missing".to_string())?;
    ensure(
        primary.contains("blake3:"),
        format!("primaryMemoryHash pattern must require blake3 prefix; got: {primary}"),
    )?;
    let peer_items = schema["properties"]["peerMemoryHashes"]["items"]["pattern"]
        .as_str()
        .ok_or_else(|| "peerMemoryHashes.items.pattern missing".to_string())?;
    ensure(
        peer_items.contains("blake3:"),
        format!("peerMemoryHashes.items pattern must require blake3 prefix; got: {peer_items}"),
    )?;
    let properties = schema["properties"]
        .as_object()
        .ok_or_else(|| "top-level properties not an object".to_string())?;
    for forbidden in ["memoryId", "rawId", "memoryBody", "primaryMemoryId"] {
        ensure(
            !properties.contains_key(forbidden),
            format!("peer_conflict schema must not accept raw `{forbidden}` field"),
        )?;
    }
    Ok(())
}
