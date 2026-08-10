//! bd-3a1op.6: structural contract for the conflict-resolve wire schema
//! (`ee.conflict.resolve.v1`, ADR 0066 / bd-3a1op.4).
//!
//! Pins schema identity, `public_schemas()` registry wiring, the report's
//! required field set, the verb vocabulary, the plan/action shapes, and the
//! per-atom execution-evidence contract. Follows
//! `graph_suggest_links_schema.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_ID: &str = "ee.conflict.resolve.v1";
const SCHEMA_REL: &str = "docs/schemas/ee.conflict.resolve.v1.json";

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_REL);
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
    let array = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("schema is missing array at {pointer}"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        out.insert(
            entry
                .as_str()
                .ok_or_else(|| format!("{pointer} contains non-string entry: {entry}"))?
                .to_owned(),
        );
    }
    Ok(out)
}

#[test]
fn conflict_resolve_schema_identity_and_registry_are_pinned() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "schema title must equal its id",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_ID),
        "properties.schema.const must pin the id",
    )?;

    let registry = public_schemas();
    let entry = registry
        .iter()
        .find(|entry| entry.id == SCHEMA_ID)
        .ok_or("public schema registry missing ee.conflict.resolve.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(entry.category == "graph", "registry category must be graph")?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(SCHEMA_ID)))
        .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "registry definition must embed the schema",
    )
}

#[test]
fn conflict_resolve_verbs_plan_and_evidence_are_pinned() -> TestResult {
    let schema = load_schema()?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "status",
        "dryRun",
        "persisted",
        "verb",
        "reason",
        "plan",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("report required set drifted: {required:?}"),
    )?;

    // The ADR 0066 verb table, exactly.
    let verbs = string_set(&schema, "/properties/verb/enum")?;
    let expected_verbs: BTreeSet<String> = ["supersede", "reject-one", "scope-split", "both-valid"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        verbs == expected_verbs,
        format!("verb vocabulary drifted: {verbs:?}"),
    )?;

    // Dry-run default is a contract: status is planned|applied, nothing else.
    let statuses = string_set(&schema, "/properties/status/enum")?;
    let expected_statuses: BTreeSet<String> = ["planned", "applied"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        statuses == expected_statuses,
        format!("status vocabulary drifted: {statuses:?}"),
    )?;

    let plan = string_set(&schema, "/properties/plan/required")?;
    let expected_plan: BTreeSet<String> = [
        "conflictId",
        "verb",
        "memoryA",
        "memoryB",
        "keep",
        "lose",
        "actions",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        plan == expected_plan,
        format!("plan shape drifted: {plan:?}"),
    )?;

    // Every planned atom names one of the four EXISTING audited operations.
    let actions = string_set(
        &schema,
        "/properties/plan/properties/actions/items/properties/action/enum",
    )?;
    let expected_actions: BTreeSet<String> =
        ["recordDecision", "expireMemory", "createLink", "addTags"]
            .into_iter()
            .map(str::to_owned)
            .collect();
    ensure(
        actions == expected_actions,
        format!("audited-atom vocabulary drifted: {actions:?}"),
    )?;

    // Applied runs report per-atom audit evidence.
    let result = string_set(&schema, "/properties/results/items/required")?;
    let expected_result: BTreeSet<String> = ["action", "auditIds", "createdMemoryId"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        result == expected_result,
        format!("execution-evidence shape drifted: {result:?}"),
    )
}
