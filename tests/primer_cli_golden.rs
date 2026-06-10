//! bd-39tzu.3 — golden + contract tests for the `ee primer` CLI surface.
//!
//! The fixture seeds a fresh workspace DB with FIXED memory ids through the
//! library API, then runs the real binary, so output is byte-deterministic
//! across machines: `ee.primer.v1` carries no wall-clock timestamps, no
//! workspace paths, and no binary version. Goldens cover both formats; the
//! contract test validates the JSON payload structurally against
//! `docs/schemas/ee.primer.v1.json` (required sets, enums, const fields).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .output()
        .map_err(|error| format!("failed to run ee {args:?}: {error}"))
}

const FIXTURE_WORKSPACE_ID: &str = "wsp_00000000000000000000000071";

fn seed_primer_workspace() -> Result<tempfile::TempDir, String> {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let db_dir = temp.path().join(".ee");
    std::fs::create_dir_all(&db_dir).map_err(|error| format!("mkdir .ee: {error}"))?;
    let connection = ee::db::DbConnection::open_file(&db_dir.join("ee.db"))
        .map_err(|error| format!("open db: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    connection
        .insert_workspace(
            FIXTURE_WORKSPACE_ID,
            &ee::db::CreateWorkspaceInput {
                path: temp.path().to_string_lossy().into_owned(),
                name: Some("primer-golden".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;

    let seeds = [
        (
            "mem_00000000000000000000000001",
            "procedural",
            "rule",
            "Always run the verify script before pushing changes to main.",
            0.9_f32,
        ),
        (
            "mem_00000000000000000000000002",
            "episodic",
            "failure",
            "Release broke when goldens were regenerated on the wrong host.",
            0.8,
        ),
        (
            "mem_00000000000000000000000003",
            "semantic",
            "decision",
            "Keep the async runtime on asupersync; tokio is forbidden.",
            0.85,
        ),
    ];
    for (id, level, kind, content, confidence) in seeds {
        connection
            .insert_memory(
                id,
                &ee::db::CreateMemoryInput {
                    workspace_id: FIXTURE_WORKSPACE_ID.to_owned(),
                    level: level.to_owned(),
                    kind: kind.to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://primer-golden".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["primer-golden".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("insert memory {id}: {error}"))?;
    }
    connection
        .close()
        .map_err(|error| format!("close db: {error}"))?;
    Ok(temp)
}

#[test]
fn primer_markdown_output_matches_golden() -> TestResult {
    let workspace = seed_primer_workspace()?;
    let output = run_ee(&[
        "primer",
        "--no-persist",
        "--workspace",
        workspace.path().to_str().unwrap(),
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "ee primer failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    assert_primer_golden("markdown.golden", &stdout)
}

#[test]
fn primer_json_output_matches_golden_and_schema() -> TestResult {
    let workspace = seed_primer_workspace()?;
    let output = run_ee(&[
        "primer",
        "--no-persist",
        "--workspace",
        workspace.path().to_str().unwrap(),
        "--json",
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "ee primer --json failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    assert_primer_golden("json.json.golden", &(stdout.trim().to_string() + "\n"))?;

    // Structural contract check against docs/schemas/ee.primer.v1.json.
    let response: Value =
        serde_json::from_str(stdout.trim()).map_err(|error| format!("parse response: {error}"))?;
    let payload = response
        .pointer("/data")
        .ok_or("response missing data payload")?;
    let schema = load_schema()?;
    validate_against_schema(&schema, payload)
}

#[test]
fn primer_cache_round_trip_is_byte_identical_modulo_cache_flag() -> TestResult {
    let workspace = seed_primer_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    // First run persists the cache; second run must hit it.
    let cold = run_ee(&["primer", "--workspace", &workspace_arg, "--json"])?;
    let warm = run_ee(&["primer", "--workspace", &workspace_arg, "--json"])?;
    let cold_json: Value =
        serde_json::from_slice(&cold.stdout).map_err(|error| format!("parse cold: {error}"))?;
    let warm_json: Value =
        serde_json::from_slice(&warm.stdout).map_err(|error| format!("parse warm: {error}"))?;
    if warm_json.pointer("/data/cache_hit") != Some(&Value::Bool(true)) {
        return Err("second run must be a cache hit".to_owned());
    }
    // Sections are byte-identical between cold and warm runs.
    if cold_json.pointer("/data/sections") != warm_json.pointer("/data/sections") {
        return Err("cache hit must reproduce identical sections".to_owned());
    }
    if cold_json.pointer("/data/rendered_markdown") != warm_json.pointer("/data/rendered_markdown")
    {
        return Err("cache hit must reproduce identical rendered markdown".to_owned());
    }
    Ok(())
}

/// Self-contained golden compare/update (same UPDATE_GOLDEN contract as
/// tests/golden.rs, without pulling that file's test module into this
/// binary).
fn assert_primer_golden(file_name: &str, actual: &str) -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("primer")
        .join(file_name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, actual)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        return Ok(());
    }
    let expected = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "read {}: {error} (run with UPDATE_GOLDEN=1)",
            path.display()
        )
    })?;
    if expected == actual {
        Ok(())
    } else {
        Err(format!(
            "golden mismatch for {}.\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ))
    }
}

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join("ee.primer.v1.json");
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse schema: {error}"))
}

fn string_set(value: &Value, pointer: &str) -> Result<Vec<String>, String> {
    Ok(value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("schema missing {pointer}"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// Minimal structural validator covering the constructs the primer schema
/// uses (required sets, const, enums); the repo has no jsonschema engine.
fn validate_against_schema(schema: &Value, payload: &Value) -> TestResult {
    for field in string_set(schema, "/required")? {
        if payload.get(&field).is_none() {
            return Err(format!("payload missing required field {field}"));
        }
    }
    if payload.pointer("/schema").and_then(Value::as_str) != Some("ee.primer.v1") {
        return Err("payload schema must be ee.primer.v1".to_owned());
    }
    let format_enum = string_set(schema, "/properties/format/enum")?;
    let format = payload
        .pointer("/format")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !format_enum.iter().any(|allowed| allowed == format) {
        return Err(format!("format {format:?} not in schema enum"));
    }
    let section_enum = string_set(schema, "/properties/sections/items/properties/name/enum")?;
    let code_enum = string_set(schema, "/properties/degraded/items/properties/code/enum")?;
    for section in payload
        .pointer("/sections")
        .and_then(Value::as_array)
        .ok_or("sections must be an array")?
    {
        let name = section
            .pointer("/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !section_enum.iter().any(|allowed| allowed == name) {
            return Err(format!("section name {name:?} not in schema enum"));
        }
        for item in section
            .pointer("/items")
            .and_then(Value::as_array)
            .ok_or("section items must be an array")?
        {
            for field in string_set(
                schema,
                "/properties/sections/items/properties/items/items/required",
            )? {
                if item.get(&field).is_none() {
                    return Err(format!("section item missing required field {field}"));
                }
            }
        }
    }
    for entry in payload
        .pointer("/degraded")
        .and_then(Value::as_array)
        .ok_or("degraded must be an array")?
    {
        let code = entry.pointer("/code").and_then(Value::as_str).unwrap_or("");
        if !code_enum.iter().any(|allowed| allowed == code) {
            return Err(format!("degraded code {code:?} not in schema enum"));
        }
    }
    let _unused: &Path = Path::new(".");
    Ok(())
}
