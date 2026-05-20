//! bd-1zb7k.19.1: structural contract for the AFR1 flight-recorder
//! trace schema `docs/schemas/ee.agent_workload_trace.v1.json`.
//!
//! AFR1 is the wire shape that AFR2 replay (bd-1zb7k.19.2), AFR3 diet
//! report (bd-1zb7k.19.3), and AFR5 64-agent playback (bd-1zb7k.19.5)
//! will compose on. The implementation surface
//! (src/obs/flight_recorder.rs, config / env-registry / status / doctor
//! wiring) lands behind this contract; pinning the schema first means
//! those downstream consumers can compile against a stable shape.
//!
//! The contract enforces the redaction-safety acceptance items
//! literally: every raw-content boolean in `redactionPosture` is
//! `const: false` so a serializer that ever emits `true` for raw
//! task/query/memory/provenance/mail text, secrets, env dumps, or
//! full file listings cannot validate. The schema is structurally
//! incapable of carrying those payloads.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.agent_workload_trace.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.agent_workload_trace.v1.json";
const SCHEMA_NAME: &str = "ee.agent_workload_trace.v1";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "sideEffectFree",
    "redactionLevel",
    "traceId",
    "recordedAt",
    "command",
    "exitCode",
    "elapsedMs",
    "responseByteCount",
    "harnessIdentity",
    "memoryReferences",
    "degradedCodes",
];

const REDACTION_LEVEL_ALLOWED: &[&str] = &["strict", "audit"];

// The bead text lists exactly these raw-content classes that must never
// appear. Each becomes a `const: false` boolean in redactionPosture so
// the schema enforces the contract.
const REQUIRED_REDACTION_POSTURE_FALSE: &[&str] = &[
    "rawTaskStringPresent",
    "rawQueryTextPresent",
    "rawMemoryBodyPresent",
];

const OPTIONAL_REDACTION_POSTURE_FALSE: &[&str] = &[
    "rawProvenanceTextPresent",
    "rawMailBodyPresent",
    "secretsPresent",
    "environmentDumpPresent",
    "fullFileListingPresent",
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

#[test]
fn agent_workload_trace_v1_schema_has_expected_envelope() -> TestResult {
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
    let side_effect_const = &schema["properties"]["sideEffectFree"]["const"];
    ensure(
        side_effect_const == &Value::Bool(true),
        format!(
            "flight-recorder schema must declare sideEffectFree const true; got: {side_effect_const}"
        ),
    )?;
    let required = collect_strings(&schema["required"], "top-level required")?;
    for field in REQUIRED_TOP_LEVEL {
        ensure(
            required.iter().any(|r| r == field),
            format!("top-level `required` is missing `{field}`; got: {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn agent_workload_trace_v1_redaction_level_enum_is_pinned() -> TestResult {
    let schema = load_schema()?;
    let values = collect_strings(
        &schema["properties"]["redactionLevel"]["enum"],
        "redactionLevel.enum",
    )?;
    ensure(
        values.len() == REDACTION_LEVEL_ALLOWED.len()
            && REDACTION_LEVEL_ALLOWED
                .iter()
                .all(|a| values.iter().any(|v| v == a)),
        format!(
            "redactionLevel enum must be exactly {REDACTION_LEVEL_ALLOWED:?}; got: {values:?}. \
             Raw text is never present at any level; `strict` is the default."
        ),
    )?;
    Ok(())
}

#[test]
fn agent_workload_trace_v1_redaction_posture_forbids_raw_content_structurally() -> TestResult {
    let schema = load_schema()?;
    let posture = &schema["$defs"]["redactionPosture"];

    // Required fields are the three the bead text lists as the
    // top-priority never-leak classes (task string, query text,
    // memory body). The schema makes these REQUIRED and `const:
    // false` so a serializer cannot omit them and cannot ever set
    // them true.
    let required = collect_strings(&posture["required"], "redactionPosture.required")?;
    for field in REQUIRED_REDACTION_POSTURE_FALSE {
        ensure(
            required.iter().any(|r| r == field),
            format!(
                "redactionPosture.required must include `{field}` so trace rows \
                 cannot omit the redaction assertion; got: {required:?}"
            ),
        )?;
        let const_value = &posture["properties"][field]["const"];
        ensure(
            const_value == &Value::Bool(false),
            format!(
                "redactionPosture.properties.{field}.const must be `false` so the \
                 schema structurally rejects any trace that claims raw content is \
                 present; got: {const_value}"
            ),
        )?;
    }

    // The optional ones (provenance, mail body, secrets, env dump,
    // full file listing) are also const: false when present — so a
    // serializer that emits them MUST emit `false`. They are
    // optional only in the sense that a trace row may omit the
    // explicit assertion; if they appear they cannot be `true`.
    for field in OPTIONAL_REDACTION_POSTURE_FALSE {
        if posture["properties"][field].is_object() {
            let const_value = &posture["properties"][field]["const"];
            ensure(
                const_value == &Value::Bool(false),
                format!(
                    "redactionPosture.properties.{field}.const must be `false` (raw \
                     {field} content must never be present in a trace row); got: {const_value}"
                ),
            )?;
        }
    }
    Ok(())
}

#[test]
fn agent_workload_trace_v1_command_shape_omits_raw_argument_values() -> TestResult {
    let schema = load_schema()?;
    let command = &schema["$defs"]["commandShape"];
    let properties = command["properties"]
        .as_object()
        .ok_or_else(|| "commandShape.properties is not an object".to_string())?;
    // Raw values must not be representable in commandShape; check
    // there is no field that would accept a raw query string.
    for forbidden in ["query", "rawQuery", "rawArgs", "argv", "task", "rawTask"] {
        ensure(
            !properties.contains_key(forbidden),
            format!(
                "commandShape must not carry `{forbidden}` — raw argument values are \
                 forbidden in flight-recorder rows. Capture shape only (verbs + flagNames)."
            ),
        )?;
    }
    let required = collect_strings(&command["required"], "commandShape.required")?;
    for field in &["verbs", "flagNames"] {
        ensure(
            required.iter().any(|r| r == field),
            format!("commandShape.required must include `{field}`; got: {required:?}"),
        )?;
    }
    Ok(())
}

#[test]
fn agent_workload_trace_v1_memory_references_carry_hashes_not_raw_ids() -> TestResult {
    let schema = load_schema()?;
    let entry = &schema["$defs"]["memoryHashRef"];
    let required = collect_strings(&entry["required"], "memoryHashRef.required")?;
    ensure(
        required.iter().any(|r| r == "hash"),
        format!("memoryHashRef.required must include `hash`; got: {required:?}"),
    )?;
    let pattern = entry["properties"]["hash"]["pattern"]
        .as_str()
        .ok_or_else(|| format!("memoryHashRef.hash.pattern missing: {entry}"))?;
    ensure(
        pattern.contains("blake3:"),
        format!(
            "memoryHashRef.hash pattern must require a blake3 prefix so raw memory IDs \
             never appear in trace rows; got: {pattern}"
        ),
    )?;
    let properties = entry["properties"]
        .as_object()
        .ok_or_else(|| "memoryHashRef.properties not an object".to_string())?;
    ensure(
        !properties.contains_key("memoryId") && !properties.contains_key("rawId"),
        format!(
            "memoryHashRef must not carry raw `memoryId` or `rawId`; got properties: {:?}",
            properties.keys().collect::<Vec<_>>()
        ),
    )?;
    Ok(())
}
