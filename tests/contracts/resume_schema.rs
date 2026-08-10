//! bd-resume-verb-v0f57: structural contract for the resume wire schema
//! (`ee.resume.v1`).
//!
//! Pins schema identity, `public_schemas()` registry wiring, the report's
//! required field set, the open-loops shape, and the per-item staleness
//! contract, so surface drift fails loudly. Follows
//! `graph_suggest_links_schema.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use ee::core::workspace::stable_workspace_id;
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::output::{public_schemas, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_ID: &str = "ee.resume.v1";
const SCHEMA_REL: &str = "docs/schemas/ee.resume.v1.json";

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
fn resume_schema_identity_and_registry_are_pinned() -> TestResult {
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
        .ok_or("public schema registry missing ee.resume.v1")?;
    ensure(entry.version == "1", "registry version must be 1")?;
    ensure(
        entry.category == "memory",
        "registry category must be memory",
    )?;
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some(SCHEMA_ID)))
        .map_err(|error| format!("registry export did not parse: {error}"))?;
    ensure(
        exported.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_ID),
        "registry definition must embed the schema",
    )
}

#[test]
fn resume_required_fields_and_staleness_contract_are_pinned() -> TestResult {
    let schema = load_schema()?;

    let required = string_set(&schema, "/required")?;
    let expected: BTreeSet<String> = [
        "schema",
        "workspaceId",
        "episodicTotal",
        "sessions",
        "openLoops",
        "staleCount",
        "nearbyStores",
        "nextCommands",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    ensure(
        required == expected,
        format!("report required set drifted: {required:?}"),
    )?;

    let open_loops = string_set(&schema, "/properties/openLoops/required")?;
    let expected_loops: BTreeSet<String> = ["revisitDecisions", "taggedItems"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        open_loops == expected_loops,
        format!("openLoops required set drifted: {open_loops:?}"),
    )?;

    // Every surfaced item must carry the stale field (nullable), and the
    // flag itself must name what superseded the item and why.
    let item_required = string_set(&schema, "/$defs/item/required")?;
    ensure(
        item_required.contains("stale"),
        "item.stale must be a required (nullable) field",
    )?;
    let stale_required = string_set(&schema, "/$defs/item/properties/stale/required")?;
    let expected_stale: BTreeSet<String> = ["supersededBy", "supersededByCreatedAt", "sharedTags"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    ensure(
        stale_required == expected_stale,
        format!("staleness contract drifted: {stale_required:?}"),
    )
}

#[test]
#[ignore = "10k real-store acceptance scale; run as a focused pinned RCH proof"]
fn resume_real_binary_completes_under_two_seconds_on_10k_store() -> TestResult {
    const CORPUS_SIZE: usize = 10_000;

    let temp = tempfile::tempdir().map_err(|error| format!("create temp workspace: {error}"))?;
    let workspace = temp.path().join("workspace");
    let store_dir = workspace.join(".ee");
    std::fs::create_dir_all(&store_dir)
        .map_err(|error| format!("create {}: {error}", store_dir.display()))?;
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", workspace.display()))?;
    let workspace_id = stable_workspace_id(&canonical_workspace);
    let database = store_dir.join("ee.db");
    let connection = DbConnection::open_file(&database)
        .map_err(|error| format!("open {}: {error}", database.display()))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate 10k acceptance store: {error}"))?;
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: canonical_workspace.display().to_string(),
                name: Some("resume-10k-acceptance".to_owned()),
            },
        )
        .map_err(|error| format!("insert acceptance workspace: {error}"))?;

    for index in 0..CORPUS_SIZE {
        let session = index < 3;
        let input = CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            level: if session { "episodic" } else { "semantic" }.to_owned(),
            kind: "note".to_owned(),
            content: format!("Resume 10k acceptance memory {index:05}"),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some("test://resume/10k-acceptance".to_owned()),
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            tags: session
                .then(|| format!("session-202608{index:02}"))
                .into_iter()
                .collect(),
            valid_from: None,
            valid_to: None,
        };
        connection
            .insert_memory(&format!("mem_{index:026}"), &input)
            .map_err(|error| format!("insert acceptance memory {index}: {error}"))?;
    }
    drop(connection);

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args([
            "resume",
            "--workspace",
            canonical_workspace
                .to_str()
                .ok_or("acceptance workspace path is not UTF-8")?,
            "--database",
            database
                .to_str()
                .ok_or("acceptance database path is not UTF-8")?,
            "--sessions",
            "3",
            "--json",
        ])
        .output()
        .map_err(|error| format!("launch real ee resume: {error}"))?;
    let elapsed = started.elapsed();

    ensure(
        output.status.success(),
        format!(
            "real ee resume failed with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let response: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse real ee resume response: {error}"))?;
    ensure(
        response.pointer("/schema").and_then(Value::as_str) == Some("ee.response.v2")
            && response.pointer("/success").and_then(Value::as_bool) == Some(true)
            && response
                .pointer("/data/report/schema")
                .and_then(Value::as_str)
                == Some(SCHEMA_ID)
            && response
                .pointer("/data/report/episodicTotal")
                .and_then(Value::as_u64)
                == Some(3),
        format!("real ee resume response contract drifted: {response}"),
    )?;
    ensure(
        elapsed < Duration::from_secs(2),
        format!(
            "ee resume took {:.3}s on a {CORPUS_SIZE}-document real store; acceptance requires <2s",
            elapsed.as_secs_f64()
        ),
    )
}
