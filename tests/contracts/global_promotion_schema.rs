//! bd-1bfwa.4: structural contract for the global-lane promotion/demotion
//! wire schemas (`ee.global_promotion.plan.v1`, `ee.global_promotion.report.v1`,
//! `ee.global_demotion.report.v1`).
//!
//! Pins schema identity, the `public_schemas()` registry wiring landed in
//! bd-1bfwa.3, and each schema's required field set, so surface drift in the
//! promote-global/demote-global contracts fails loudly instead of silently
//! changing what agents parse. Follows the `contention_schema.rs` pattern.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

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

fn required_set(schema: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = schema
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let array = node
        .as_array()
        .ok_or_else(|| format!("{pointer} must be a JSON array"))?;
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

struct SchemaCase {
    id: &'static str,
    file: &'static str,
    category: &'static str,
    required: &'static [&'static str],
}

const CASES: &[SchemaCase] = &[
    SchemaCase {
        id: "ee.global_promotion.plan.v1",
        file: "docs/schemas/ee.global_promotion.plan.v1.json",
        category: "memory",
        required: &[
            "schema",
            "memoryId",
            "originWorkspaceId",
            "verdict",
            "detail",
            "auditAction",
        ],
    },
    SchemaCase {
        id: "ee.global_promotion.report.v1",
        file: "docs/schemas/ee.global_promotion.report.v1.json",
        category: "memory",
        required: &[
            "schema",
            "plan",
            "executed",
            "globalMemoryId",
            "alreadyPromoted",
        ],
    },
    SchemaCase {
        id: "ee.global_demotion.report.v1",
        file: "docs/schemas/ee.global_demotion.report.v1.json",
        category: "memory",
        required: &[
            "schema",
            "globalMemoryId",
            "executed",
            "tombstoned",
            "originWorkspaceId",
            "originMemoryId",
        ],
    },
];

#[test]
fn global_lane_schema_identity_and_registry_are_pinned() -> TestResult {
    let registry = public_schemas();
    for case in CASES {
        let schema = load_json(case.file)?;
        ensure(
            schema.pointer("/title").and_then(Value::as_str) == Some(case.id),
            format!("{}: schema title must equal its id", case.id),
        )?;
        ensure(
            schema
                .pointer("/properties/schema/const")
                .and_then(Value::as_str)
                == Some(case.id),
            format!("{}: properties.schema.const must pin the id", case.id),
        )?;

        let entry = registry
            .iter()
            .find(|entry| entry.id == case.id)
            .ok_or_else(|| format!("public schema registry missing {}", case.id))?;
        ensure(
            entry.version == "1",
            format!("{}: registry version must be 1", case.id),
        )?;
        ensure(
            entry.category == case.category,
            format!("{}: registry category must be {}", case.id, case.category),
        )?;
        let exported: Value = serde_json::from_str(&render_schema_export_json(Some(case.id)))
            .map_err(|error| format!("{}: registry export did not parse: {error}", case.id))?;
        ensure(
            exported.pointer("/title").and_then(Value::as_str) == Some(case.id),
            format!("{}: registry definition must embed the schema", case.id),
        )?;
    }
    Ok(())
}

#[test]
fn global_lane_schema_required_fields_are_pinned() -> TestResult {
    for case in CASES {
        let schema = load_json(case.file)?;
        let required = required_set(&schema, "/required")?;
        let expected: BTreeSet<String> = case.required.iter().map(|s| (*s).to_owned()).collect();
        ensure(
            required == expected,
            format!(
                "{}: required set drifted: got {required:?}, pinned {expected:?}",
                case.id
            ),
        )?;
    }
    Ok(())
}
