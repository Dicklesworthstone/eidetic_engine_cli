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

use std::collections::BTreeSet;
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

const CLOSED_OBJECT_DEFS: &[&str] = &[
    "commandShape",
    "harnessIdentity",
    "memoryHashRef",
    "redactionPosture",
    "retentionPosture",
];

const COMMAND_OUTPUT_FORMAT_ALLOWED: &[&str] = &[
    "json", "human", "markdown", "toon", "jsonl", "compact", "hook",
];

const TOKEN_ESTIMATOR_ALLOWED: &[&str] = &["bytes_div_4", "tiktoken_cl100k_base", "approximate"];

const HARNESS_PROGRAM_ALLOWED: &[&str] = &[
    "claude-code",
    "codex-cli",
    "gemini-cli",
    "cursor",
    "windsurf",
    "ee-cli-direct",
    "unknown",
];

const HARNESS_MODEL_FAMILY_ALLOWED: &[&str] = &[
    "claude-opus",
    "claude-sonnet",
    "claude-haiku",
    "gpt-5",
    "gpt-4",
    "gemini-pro",
    "other",
    "unknown",
];

const MEMORY_KIND_ALLOWED: &[&str] = &[
    "fact",
    "decision",
    "rule",
    "anti_pattern",
    "workflow_hint",
    "session_evidence",
    "other",
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

fn ensure_required_fields_have_properties(schema: &Value, ctx: &str) -> TestResult {
    let required = collect_strings(&schema["required"], &format!("{ctx}.required"))?;
    let properties = schema["properties"]
        .as_object()
        .ok_or_else(|| format!("{ctx}.properties must be an object"))?;
    for field in &required {
        ensure(
            properties.contains_key(field),
            format!(
                "{ctx}.required includes `{field}` but properties are {:?}",
                properties.keys().collect::<Vec<_>>()
            ),
        )?;
    }
    Ok(())
}

fn ensure_exact_string_enum(node: &Value, expected: &[&str], ctx: &str) -> TestResult {
    let values = collect_strings(node, ctx)?;
    ensure(
        values.len() == expected.len() && expected.iter().all(|a| values.iter().any(|v| v == a)),
        format!("{ctx} must be exactly {expected:?}; got: {values:?}"),
    )
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
    )?;
    ensure_required_fields_have_properties(&schema, "top-level trace schema")?;
    Ok(())
}

#[test]
fn agent_workload_trace_v1_closes_object_shapes_against_extra_fields() -> TestResult {
    let schema = load_schema()?;
    let additional_properties = &schema["additionalProperties"];
    ensure(
        additional_properties == &Value::Bool(false),
        format!(
            "top-level trace row must be closed with additionalProperties=false; got: {}",
            additional_properties
        ),
    )?;

    for def_name in CLOSED_OBJECT_DEFS {
        let def = &schema["$defs"][def_name];
        ensure(
            def["type"] == "object",
            format!("$defs.{def_name}.type must be object; got: {}", def["type"]),
        )?;
        let additional_properties = &def["additionalProperties"];
        ensure(
            additional_properties == &Value::Bool(false),
            format!(
                "$defs.{def_name} must be closed with additionalProperties=false; got: {}",
                additional_properties
            ),
        )?;
        ensure_required_fields_have_properties(def, &format!("$defs.{def_name}"))?;
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
fn agent_workload_trace_v1_machine_facing_enums_are_pinned() -> TestResult {
    let schema = load_schema()?;
    ensure_exact_string_enum(
        &schema["$defs"]["commandShape"]["properties"]["outputFormat"]["enum"],
        COMMAND_OUTPUT_FORMAT_ALLOWED,
        "commandShape.outputFormat.enum",
    )?;
    ensure_exact_string_enum(
        &schema["properties"]["tokenEstimatorId"]["enum"],
        TOKEN_ESTIMATOR_ALLOWED,
        "tokenEstimatorId.enum",
    )?;
    ensure_exact_string_enum(
        &schema["$defs"]["harnessIdentity"]["properties"]["program"]["enum"],
        HARNESS_PROGRAM_ALLOWED,
        "harnessIdentity.program.enum",
    )?;
    ensure_exact_string_enum(
        &schema["$defs"]["harnessIdentity"]["properties"]["modelFamily"]["enum"],
        HARNESS_MODEL_FAMILY_ALLOWED,
        "harnessIdentity.modelFamily.enum",
    )?;
    ensure_exact_string_enum(
        &schema["$defs"]["memoryHashRef"]["properties"]["kind"]["enum"],
        MEMORY_KIND_ALLOWED,
        "memoryHashRef.kind.enum",
    )
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
