//! bd-1clqr.1: structural contract for `ee.session_budget.v1`.
//!
//! This pins the opt-in session budget ledger row before the recording path
//! lands in bd-1clqr.2. The schema and fixtures are the implementation
//! contract for later low-overhead recording and planner work.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::models::{KNOWN_SCHEMAS, SESSION_BUDGET_SCHEMA_V1};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.session_budget.v1.json";
const SCHEMA_NAME: &str = "ee.session_budget.v1";
const FIXTURES_REL: &str = "tests/fixtures/session_budget";
const FIXTURE_NAMES: [&str; 4] = [
    "cheap_recall",
    "large_pack",
    "rch_blocked_proof",
    "agent_mail_degraded_coordination",
];
const REDACTION_CONST: &str = "paths_counts_hashes_no_content";

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
fn session_budget_schema_identity_status_and_registry_are_pinned() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        SESSION_BUDGET_SCHEMA_V1 == SCHEMA_NAME,
        "model constant must match schema name",
    )?;
    ensure(
        KNOWN_SCHEMAS.contains(&SESSION_BUDGET_SCHEMA_V1),
        "KNOWN_SCHEMAS must include the session budget schema",
    )?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_NAME),
        "schema title must be the schema name",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with(&format!("{SCHEMA_NAME}.json"))),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_NAME),
        "schema.const must pin the schema name",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/shipped")
            .and_then(Value::as_bool)
            == Some(false),
        "schema must stay unshipped until bd-1clqr.2 lands the recorder",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/tracking_bead")
            .and_then(Value::as_str)
            == Some("bd-1clqr.2"),
        "schema status must point at the recording-path bead",
    )
}

#[test]
fn required_field_sets_cover_cost_correlation_privacy_and_retention() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let top_level = string_set(&schema, "/required")?;
    let expected_top_level: BTreeSet<String> = [
        "schema",
        "eventId",
        "recordedAt",
        "workspaceFingerprint",
        "optIn",
        "correlation",
        "command",
        "cost",
        "degradedGroups",
        "privacy",
        "retention",
        "evidence",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        top_level == expected_top_level,
        format!("top-level required set drifted: {top_level:?}"),
    )?;

    let cost = string_set(&schema, "/$defs/cost/required")?;
    for field in [
        "wallClockMs",
        "outputTokensEstimated",
        "outputTokensReturned",
        "outputBytes",
        "packTokensRequested",
        "packTokensUsed",
        "rch",
        "db",
        "derivedAssets",
    ] {
        ensure(cost.contains(field), format!("cost missing {field}"))?;
    }

    let correlation = string_set(&schema, "/$defs/correlation/required")?;
    for field in [
        "sessionId",
        "commandId",
        "parentCommandId",
        "taskHash",
        "packId",
        "rchJobId",
        "agentMailThreadId",
        "beadId",
    ] {
        ensure(
            correlation.contains(field),
            format!("correlation missing {field}"),
        )?;
    }
    Ok(())
}

#[test]
fn privacy_contract_forbids_raw_content_and_paths() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        schema
            .pointer("/$defs/privacy/properties/redactionStatus/const")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        "redactionStatus const drifted",
    )?;
    for field in ["rawCommandStored", "rawOutputStored", "contentStored"] {
        ensure(
            schema
                .pointer(&format!("/$defs/privacy/properties/{field}/const"))
                .and_then(Value::as_bool)
                == Some(false),
            format!("{field} must be const false"),
        )?;
    }
    for forbidden in [
        "rawCommand",
        "rawOutput",
        "memoryBody",
        "mailBody",
        "absolutePath",
    ] {
        ensure(
            schema
                .pointer(&format!("/properties/{forbidden}"))
                .is_none(),
            format!("schema must not expose {forbidden}"),
        )?;
    }
    Ok(())
}

fn validate_fixture(schema: &Value, row: &Value, label: &str) -> TestResult {
    let object = row
        .as_object()
        .ok_or_else(|| format!("{label}: row must be an object"))?;
    for field in string_set(schema, "/required")? {
        ensure(
            object.contains_key(&field),
            format!("{label}: row missing required field {field}"),
        )?;
    }
    let allowed = schema
        .pointer("/properties")
        .and_then(Value::as_object)
        .ok_or("schema /properties missing")?;
    for key in object.keys() {
        ensure(
            allowed.contains_key(key),
            format!("{label}: row has unknown field {key}"),
        )?;
    }
    ensure(
        row.pointer("/schema").and_then(Value::as_str) == Some(SCHEMA_NAME),
        format!("{label}: schema field must be {SCHEMA_NAME}"),
    )?;
    ensure(
        row.pointer("/optIn/enabled").and_then(Value::as_bool) == Some(true),
        format!("{label}: optIn.enabled must be true"),
    )?;
    ensure(
        row.pointer("/privacy/redactionStatus")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        format!("{label}: redactionStatus must be pinned"),
    )?;
    for field in [
        "/privacy/rawCommandStored",
        "/privacy/rawOutputStored",
        "/privacy/contentStored",
    ] {
        ensure(
            row.pointer(field).and_then(Value::as_bool) == Some(false),
            format!("{label}: {field} must be false"),
        )?;
    }

    let surfaces = string_set(schema, "/$defs/command/properties/surface/enum")?;
    let command_classes = string_set(schema, "/$defs/command/properties/commandClass/enum")?;
    let degraded_sources = string_set(schema, "/$defs/degradedGroup/properties/source/enum")?;
    let severities = string_set(schema, "/$defs/degradedGroup/properties/severity/enum")?;
    let surface = row
        .pointer("/command/surface")
        .and_then(Value::as_str)
        .unwrap_or("");
    ensure(
        surfaces.contains(surface),
        format!("{label}: surface {surface:?} not in enum"),
    )?;
    let command_class = row
        .pointer("/command/commandClass")
        .and_then(Value::as_str)
        .unwrap_or("");
    ensure(
        command_classes.contains(command_class),
        format!("{label}: commandClass {command_class:?} not in enum"),
    )?;

    for group in row
        .pointer("/degradedGroups")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label}: degradedGroups must be an array"))?
    {
        let source = group
            .pointer("/source")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            degraded_sources.contains(source),
            format!("{label}: degraded source {source:?} not in enum"),
        )?;
        let severity = group
            .pointer("/severity")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            severities.contains(severity),
            format!("{label}: degraded severity {severity:?} not in enum"),
        )?;
        ensure(
            group.pointer("/count").and_then(Value::as_u64).unwrap_or(0) >= 1,
            format!("{label}: degraded count must be positive"),
        )?;
    }

    let serialized = serde_json::to_string(row).map_err(|error| error.to_string())?;
    for forbidden in [
        "rawCommand",
        "rawOutput",
        "memoryBody",
        "mailBody",
        "/Users/",
    ] {
        ensure(
            !serialized.contains(forbidden),
            format!("{label}: fixture leaked forbidden marker {forbidden}"),
        )?;
    }
    let reparsed: Value = serde_json::from_str(&serialized).map_err(|error| error.to_string())?;
    ensure(
        reparsed == *row,
        format!("{label}: serialize/parse round trip changed fixture"),
    )
}

#[test]
fn examples_cover_recall_pack_rch_block_and_agent_mail_degradation() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let mut surfaces = BTreeSet::new();
    let mut degraded_sources = BTreeSet::new();
    for fixture in FIXTURE_NAMES {
        let row = load_json(&format!("{FIXTURES_REL}/{fixture}.json"))?;
        validate_fixture(&schema, &row, fixture)?;
        surfaces.insert(
            row.pointer("/command/surface")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        );
        for group in row
            .pointer("/degradedGroups")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            degraded_sources.insert(
                group
                    .pointer("/source")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            );
        }
    }
    for required in [
        "recall",
        "pack",
        "verification_proof",
        "agent_mail_coordination",
    ] {
        ensure(
            surfaces.contains(required),
            format!("fixtures missing required surface {required}: {surfaces:?}"),
        )?;
    }
    for required in ["rch", "agent_mail"] {
        ensure(
            degraded_sources.contains(required),
            format!("fixtures missing degraded source {required}: {degraded_sources:?}"),
        )?;
    }
    Ok(())
}
