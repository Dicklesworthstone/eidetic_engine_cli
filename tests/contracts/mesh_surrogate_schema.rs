//! bd-2irom: structural contract for `docs/schemas/ee.mesh.surrogate.v1.json`.
//!
//! Mesh search-surrogate compatibility, privacy, and rebuild rules are a
//! Phase-6 SRR6 mesh feature. The Rust types and emission code are
//! sequenced behind `bd-29ulx` (peer trust + redaction policy); this
//! contract pins the *schema-side* shape now so that the policy and
//! emission slices that land later can compile against a stable
//! contract instead of redesigning the wire shape mid-stream.
//!
//! What the contract asserts:
//!
//! 1. The file parses as JSON and declares the expected `$id`, `title`,
//!    and `schema` const so consumers can identify it.
//! 2. The five surrogate types (`embedding`, `summary`, `minhash`,
//!    `lexical_metadata`, `query_fingerprint`) are all present in the
//!    `surrogateType` enum. Future additions are fine; deletions break
//!    the bd-2irom contract because peer policy and degraded-code paths
//!    depend on the closed set above.
//!
//! 3. The four documented degraded codes (`surrogate_denied`,
//!    `surrogate_incompatible`, `surrogate_recomputed`,
//!    `lexical_fallback_used`) are all listed in the `degradedCode`
//!    enum. The bead acceptance lists exactly these four; the contract
//!    locks them so the Rust constants that land later can grep for the
//!    same literals.
//!
//! 4. The default-deny posture is encoded structurally:
//!    `policy.exportAllowed` is required (so an absent value cannot
//!    silently default to true), and `policy.requiresLocalRecompute`
//!    plus `policy.requiresCompatibilityCheck` are required so a
//!    receiver cannot accept a surrogate without first making a
//!    recompute / compatibility-check decision.
//!
//! No Rust source touches; no peer-WIP collision risk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.mesh.surrogate.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.mesh.surrogate.v1.json";
const SCHEMA_NAME: &str = "ee.mesh.surrogate.v1";
const REQUIRED_SURROGATE_TYPES: &[&str] = &[
    "embedding",
    "summary",
    "minhash",
    "lexical_metadata",
    "query_fingerprint",
];
const REQUIRED_DEGRADED_CODES: &[&str] = &[
    "surrogate_denied",
    "surrogate_incompatible",
    "surrogate_recomputed",
    "lexical_fallback_used",
];
const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "surrogateId",
    "surrogateType",
    "modelFingerprint",
    "contentHash",
    "policy",
];
const REQUIRED_POLICY_FIELDS: &[&str] = &[
    "exportAllowed",
    "requiresLocalRecompute",
    "requiresCompatibilityCheck",
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

fn collect_enum_strings(node: &Value) -> Result<Vec<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("expected `enum` array, got: {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("non-string enum entry: {value}"))
        })
        .collect()
}

fn collect_required_strings(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected `required` array, got: {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string required entry: {value}"))
        })
        .collect()
}

fn expected_string_set(expected: &[&str]) -> BTreeSet<String> {
    expected.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn ee_mesh_surrogate_v1_schema_has_expected_envelope() -> TestResult {
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
    let actual = collect_required_strings(&schema["required"], "top-level")?;
    let expected = expected_string_set(REQUIRED_TOP_LEVEL);
    ensure(
        actual == expected,
        format!(
            "REQUIRED_TOP_LEVEL drifted from schema required array\nexpected={expected:?}\nactual={actual:?}"
        ),
    )
}

#[test]
fn ee_mesh_surrogate_v1_lists_all_documented_surrogate_types() -> TestResult {
    let schema = load_schema()?;
    let enum_node = &schema["$defs"]["surrogateType"]["enum"];
    let values = collect_enum_strings(enum_node)?;
    for required in REQUIRED_SURROGATE_TYPES {
        ensure(
            values.iter().any(|v| v == required),
            format!(
                "surrogateType enum is missing `{required}`; got: {values:?}. \
                 bd-2irom lane policy and lexical-fallback paths depend on \
                 these exact tokens; adding more is fine, removing is not."
            ),
        )?;
    }
    Ok(())
}

#[test]
fn ee_mesh_surrogate_v1_lists_all_documented_degraded_codes() -> TestResult {
    let schema = load_schema()?;
    let enum_node = &schema["$defs"]["degradedCode"]["enum"];
    let values = collect_enum_strings(enum_node)?;
    for required in REQUIRED_DEGRADED_CODES {
        ensure(
            values.iter().any(|v| v == required),
            format!(
                "degradedCode enum is missing `{required}`; got: {values:?}. \
                 The four codes pinned here are the bd-2irom acceptance \
                 contract for structured logs; Rust constants and \
                 failure_mode fixtures must grep against these literals."
            ),
        )?;
    }
    Ok(())
}

#[test]
fn ee_mesh_surrogate_v1_policy_enforces_explicit_export_decision() -> TestResult {
    let schema = load_schema()?;
    let policy_def = &schema["$defs"]["policy"];
    let actual = collect_required_strings(&policy_def["required"], "policy")?;
    let expected = expected_string_set(REQUIRED_POLICY_FIELDS);
    ensure(
        actual == expected,
        format!(
            "REQUIRED_POLICY_FIELDS drifted from schema policy.required array\n\
             expected={expected:?}\nactual={actual:?}. \
             bd-2irom default-deny posture requires `exportAllowed`, \
             `requiresLocalRecompute`, and `requiresCompatibilityCheck` \
             to be present explicitly so an absent value cannot \
             silently widen the export surface."
        ),
    )
}
