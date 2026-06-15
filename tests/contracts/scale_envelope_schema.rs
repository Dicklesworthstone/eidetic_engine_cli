//! bd-ssoco.1: structural contract for `ee.scale_envelope.v1`.
//!
//! The first scale-envelope slice is a public contract, not a live collector:
//! it pins the redaction-safe posture shape, registry wiring, SLO vocabulary,
//! recovery action vocabulary, and degraded-code fixture coverage that later
//! fixture/probe/steward work must emit or consume.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::models::{
    KNOWN_SCHEMAS, SCALE_ENVELOPE_SCHEMA_V1, SCALE_FIXTURE_UNAVAILABLE_CODE,
    SCALE_POSTURE_THRASHING_CODE, SCALE_POSTURE_WARMING_CODE, SCALE_PROBE_BUDGET_EXCEEDED_CODE,
};
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.scale_envelope.v1.json";
const REDACTION_CONST: &str = "counts_hashes_paths_no_content";
const SCALE_CODES: [&str; 4] = [
    SCALE_POSTURE_WARMING_CODE,
    SCALE_POSTURE_THRASHING_CODE,
    SCALE_FIXTURE_UNAVAILABLE_CODE,
    SCALE_PROBE_BUDGET_EXCEEDED_CODE,
];

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
fn scale_envelope_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        KNOWN_SCHEMAS.contains(&SCALE_ENVELOPE_SCHEMA_V1),
        "KNOWN_SCHEMAS must include ee.scale_envelope.v1",
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCALE_ENVELOPE_SCHEMA_V1),
        "schema title must be ee.scale_envelope.v1",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCALE_ENVELOPE_SCHEMA_V1),
        "schema.const must pin ee.scale_envelope.v1",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with("ee.scale_envelope.v1.json")),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/tracking_bead")
            .and_then(Value::as_str)
            == Some("bd-ssoco.1"),
        "schema status must cite bd-ssoco.1",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/daemon_required")
            .and_then(Value::as_bool)
            == Some(false),
        "scale envelope must not require daemon mode",
    )?;
    ensure(
        schema
            .pointer("/properties/redactionStatus/const")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        "redactionStatus const drifted",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == SCALE_ENVELOPE_SCHEMA_V1)
        .ok_or("public schema registry missing ee.scale_envelope.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(entry.category == "ops", "registry category must be ops")?;
    let exported: Value =
        serde_json::from_str(&render_schema_export_json(Some(SCALE_ENVELOPE_SCHEMA_V1)))
            .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(SCALE_ENVELOPE_SCHEMA_V1),
        "registry definition must embed the scale-envelope schema",
    )
}

#[test]
fn required_blocks_cover_corpus_store_wal_index_slo_recovery_and_provenance() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "generatedAt",
        "workspaceFingerprint",
        "source",
        "redactionStatus",
        "corpusProfile",
        "storePosture",
        "pageCacheWalPosture",
        "indexPosture",
        "commandSlos",
        "degradedCodes",
        "recoveryActions",
        "provenance",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("top-level required set drifted: {required:?}"),
    )?;
    for (pointer, field) in [
        ("/$defs/corpusProfile/required", "memoryCount"),
        ("/$defs/storePosture/required", "readPoolState"),
        ("/$defs/pageCacheWalPosture/required", "walState"),
        ("/$defs/indexPosture/required", "graph"),
        ("/$defs/commandSlo/required", "budgetMs"),
        ("/$defs/recoveryAction/required", "command"),
        ("/$defs/provenanceRef/required", "hash"),
    ] {
        let fields = string_set(&schema, pointer)?;
        ensure(fields.contains(field), format!("{pointer} missing {field}"))?;
    }
    Ok(())
}

#[test]
fn degraded_codes_are_closed_and_fixtured() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let code_enum = string_set(&schema, "/$defs/degradedCode/properties/code/enum")?;
    let expected: BTreeSet<String> = SCALE_CODES.into_iter().map(str::to_owned).collect();
    ensure(
        code_enum == expected,
        format!("scale degraded code enum drifted: {code_enum:?}"),
    )?;
    for code in SCALE_CODES {
        let fixture = load_json(&format!("tests/fixtures/failure_modes/{code}.json"))?;
        ensure(
            fixture.pointer("/schema").and_then(Value::as_str)
                == Some("ee.failure_mode_fixture.v1"),
            format!("{code}: fixture schema drifted"),
        )?;
        ensure(
            fixture.pointer("/code").and_then(Value::as_str) == Some(code),
            format!("{code}: fixture code drifted"),
        )?;
        ensure(
            fixture
                .pointer("/surfaces")
                .and_then(Value::as_array)
                .is_some_and(|surfaces| {
                    surfaces
                        .iter()
                        .any(|surface| surface.as_str() == Some("scale envelope"))
                }),
            format!("{code}: fixture must cover the scale envelope surface"),
        )?;
    }
    Ok(())
}

#[test]
fn slo_and_recovery_vocabularies_are_intentionally_small() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let command_surfaces = string_set(&schema, "/$defs/commandSlo/properties/surface/enum")?;
    for required in [
        "ee remember",
        "ee search",
        "ee pack",
        "ee swarm work-packet",
    ] {
        ensure(
            command_surfaces.contains(required),
            format!("command SLO surfaces missing {required}"),
        )?;
    }
    let recovery_kinds = string_set(&schema, "/$defs/recoveryAction/properties/kind/enum")?;
    for required in [
        "recapture_fixture",
        "warm_cache",
        "checkpoint_wal",
        "rebuild_index",
        "reduce_probe_scope",
    ] {
        ensure(
            recovery_kinds.contains(required),
            format!("recovery kinds missing {required}"),
        )?;
    }
    Ok(())
}
