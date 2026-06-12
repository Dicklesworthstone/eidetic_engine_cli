//! bd-u875s.3 — golden + contract tests for the `ee recall` CLI surface.
//!
//! The fixture seeds a fresh workspace DB with FIXED memory ids and explicit
//! `anchor:path:…` / `anchor:symbol:…` tokens through the library API, then
//! runs the real binary, so output is byte-deterministic across machines:
//! `ee.recall.v1` carries no wall-clock timestamps, no workspace paths, and
//! no binary version. Goldens cover both formats; the contract test
//! validates the JSON payload structurally against
//! `docs/schemas/ee.recall.v1.json` (required sets, enums, const fields).
//! Budget paging is asserted to partition the ranked set exactly once
//! (no duplicates, no gaps) and rejected cursors must yield an EMPTY page
//! with the ADR 0063 cursor vocabulary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

const FIXTURE_WORKSPACE_ID: &str = "wsp_00000000000000000000000081";
const EMPTY_WORKSPACE_ID: &str = "wsp_00000000000000000000000082";

const MEM_RULE: &str = "mem_00000000000000000000000041";
const MEM_FAILURE: &str = "mem_00000000000000000000000042";
const MEM_DECISION: &str = "mem_00000000000000000000000043";

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_MAX_OUTPUT_TOKENS")
        .output()
        .map_err(|error| format!("failed to run ee {args:?}: {error}"))
}

fn seed_workspace(
    workspace_id: &str,
    seeds: &[(&str, &str, &str, &str, f32)],
) -> Result<tempfile::TempDir, String> {
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
            workspace_id,
            &ee::db::CreateWorkspaceInput {
                path: temp.path().to_string_lossy().into_owned(),
                name: Some("recall-golden".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;
    for (id, level, kind, content, confidence) in seeds {
        connection
            .insert_memory(
                id,
                &ee::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: (*level).to_owned(),
                    kind: (*kind).to_owned(),
                    content: (*content).to_owned(),
                    workflow_id: None,
                    confidence: *confidence,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://recall-golden".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["recall-golden".to_owned()],
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

/// Three memories with explicit anchors: a procedural rule and an episodic
/// failure both anchored on `src/db/mod.rs` (the failure also carries a
/// symbol anchor), plus a semantic decision anchored elsewhere so glob
/// scoping is observable.
fn seed_recall_workspace() -> Result<tempfile::TempDir, String> {
    seed_workspace(
        FIXTURE_WORKSPACE_ID,
        &[
            (
                MEM_RULE,
                "procedural",
                "rule",
                "Always route Cargo verification through RCH for storage-layer changes. \
                 anchor:path:src/db/mod.rs",
                0.9,
            ),
            (
                MEM_FAILURE,
                "episodic",
                "failure",
                "Linker OOM at default -j when rebuilding the DB layer. \
                 anchor:path:src/db/mod.rs anchor:symbol:DbConnection",
                0.8,
            ),
            (
                MEM_DECISION,
                "semantic",
                "decision",
                "Graph snapshots persist centrality scores for the primer. \
                 anchor:path:src/graph/mod.rs",
                0.85,
            ),
        ],
    )
}

/// Self-contained golden compare/update (same UPDATE_GOLDEN contract as
/// tests/golden.rs, without pulling that file's test module into this
/// binary).
fn assert_recall_golden(file_name: &str, actual: &str) -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("recall")
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

fn parse_response(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "{context} failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_str(stdout.trim()).map_err(|error| format!("{context}: parse: {error}"))
}

fn degraded_codes(response: &Value) -> Vec<String> {
    response
        .pointer("/degraded")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.pointer("/code").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn item_memory_ids(response: &Value) -> Vec<String> {
    response
        .pointer("/data/recall/items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.pointer("/memoryId").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn recall_requires_at_least_one_selector() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let output = run_ee(&[
        "recall",
        "--workspace",
        workspace.path().to_str().unwrap(),
        "--json",
    ])?;
    if output.status.success() {
        return Err("ee recall without selectors must fail with a usage error".to_owned());
    }
    if output.status.code() != Some(1) {
        return Err(format!(
            "expected usage exit code 1, got {:?}",
            output.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("requires at least one selector") {
        return Err(format!(
            "usage error must name the selector requirement; got: {stdout}"
        ));
    }
    Ok(())
}

#[test]
fn recall_markdown_output_matches_golden() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let output = run_ee(&[
        "recall",
        "--path",
        "src/db/*.rs",
        "--workspace",
        workspace.path().to_str().unwrap(),
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "ee recall failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    assert_recall_golden("markdown.golden", &stdout)
}

#[test]
fn recall_json_output_matches_golden_and_schema() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let output = run_ee(&[
        "recall",
        "--path",
        "src/db/*.rs",
        "--workspace",
        workspace.path().to_str().unwrap(),
        "--json",
    ])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!(
            "ee recall --json failed: {}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    assert_recall_golden("json.json.golden", &(stdout.trim().to_string() + "\n"))?;

    let response: Value =
        serde_json::from_str(stdout.trim()).map_err(|error| format!("parse response: {error}"))?;
    if response.pointer("/schema").and_then(Value::as_str) != Some("ee.response.v2") {
        return Err("envelope schema must be ee.response.v2".to_owned());
    }
    let payload = response
        .pointer("/data/recall")
        .ok_or("response missing data.recall payload")?;
    let schema = load_schema()?;
    validate_against_schema(&schema, payload)?;

    // Ranking: the procedural rule (0.9) outranks the episodic failure
    // (1.0 × 0.8 × 0.6 × 1.15 = 0.552); the decision anchored on
    // src/graph/mod.rs must not match the src/db glob.
    let ids = item_memory_ids(&response);
    if ids != vec![MEM_RULE.to_owned(), MEM_FAILURE.to_owned()] {
        return Err(format!("unexpected ranked ids: {ids:?}"));
    }
    Ok(())
}

#[test]
fn recall_kind_filter_narrows_before_ranking() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let narrowed = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--kind",
            "failure",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --kind failure",
    )?;
    let ids = item_memory_ids(&narrowed);
    if ids != vec![MEM_FAILURE.to_owned()] {
        return Err(format!(
            "--kind failure must keep only the failure: {ids:?}"
        ));
    }
    Ok(())
}

#[test]
fn recall_symbol_level_and_or_dedup_compose() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let symbol_only = parse_response(
        &run_ee(&[
            "recall",
            "--symbol",
            "DbConnection",
            "--level",
            "episodic",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --symbol DbConnection --level episodic",
    )?;
    let ids = item_memory_ids(&symbol_only);
    if ids != vec![MEM_FAILURE.to_owned()] {
        return Err(format!(
            "--symbol plus --level must keep only the anchored episodic failure: {ids:?}"
        ));
    }
    if symbol_only
        .pointer("/data/recall/items/0/anchor/kind")
        .and_then(Value::as_str)
        != Some("symbol")
    {
        return Err("symbol-only recall must report a symbol anchor".to_owned());
    }

    let path_and_symbol = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--symbol",
            "DbConnection",
            "--level",
            "episodic",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --path src/db/*.rs --symbol DbConnection --level episodic",
    )?;
    let ids = item_memory_ids(&path_and_symbol);
    if ids != vec![MEM_FAILURE.to_owned()] {
        return Err(format!(
            "path+symbol OR composition must dedup the failure memory once: {ids:?}"
        ));
    }
    if path_and_symbol
        .pointer("/data/recall/totalMatched")
        .and_then(Value::as_u64)
        != Some(1)
    {
        return Err("path+symbol dedup must report one matched memory".to_owned());
    }
    Ok(())
}

#[test]
fn recall_distinct_empty_codes_for_filtered_vs_empty_index() -> TestResult {
    // Filters removed everything: the index HAS rows for the surface, so the
    // distinct recall_filtered_empty (not anchor_index_empty) must fire.
    let workspace = seed_recall_workspace()?;
    let filtered = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--kind",
            "convention",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall --kind convention",
    )?;
    if !item_memory_ids(&filtered).is_empty() {
        return Err("filtered recall must return no items".to_owned());
    }
    let codes = degraded_codes(&filtered);
    if !codes.iter().any(|code| code == "recall_filtered_empty") {
        return Err(format!("expected recall_filtered_empty, got {codes:?}"));
    }
    if codes.iter().any(|code| code == "anchor_index_empty") {
        return Err("anchor_index_empty must not fire when the index has rows".to_owned());
    }

    // A workspace with no anchored memories at all reports the empty index.
    let empty = seed_workspace(EMPTY_WORKSPACE_ID, &[])?;
    let empty_response = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/**",
            "--workspace",
            empty.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall over empty index",
    )?;
    let codes = degraded_codes(&empty_response);
    if !codes.iter().any(|code| code == "anchor_index_empty") {
        return Err(format!("expected anchor_index_empty, got {codes:?}"));
    }
    if codes.iter().any(|code| code == "recall_filtered_empty") {
        return Err("recall_filtered_empty must not fire for an empty index".to_owned());
    }
    Ok(())
}

#[test]
fn recall_budget_pages_partition_the_ranked_set_exactly_once() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let workspace_arg = workspace.path().to_str().unwrap().to_owned();
    let first = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--budget-tokens",
            "20",
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --budget-tokens 20",
    )?;
    let first_ids = item_memory_ids(&first);
    if first_ids.is_empty() || first_ids.len() >= 2 {
        return Err(format!(
            "budget 20 must keep a strict non-empty prefix of the 2 matches: {first_ids:?}"
        ));
    }
    if first.pointer("/data/recall/truncated") != Some(&Value::Bool(true)) {
        return Err("budget page must report truncated=true".to_owned());
    }
    let truncation_entry = first
        .pointer("/degraded")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.pointer("/code").and_then(Value::as_str) == Some("output_truncated_budget")
            })
        })
        .ok_or("budget truncation must emit output_truncated_budget")?
        .clone();
    let cursor = truncation_entry
        .pointer("/details/continuationCursor")
        .and_then(Value::as_str)
        .ok_or("output_truncated_budget must carry details.continuationCursor")?
        .to_owned();
    let dropped = truncation_entry
        .pointer("/details/droppedCount")
        .and_then(Value::as_u64)
        .ok_or("output_truncated_budget must carry details.droppedCount")?;
    if dropped as usize + first_ids.len() != 2 {
        return Err(format!(
            "droppedCount {dropped} plus kept {} must equal total 2",
            first_ids.len()
        ));
    }

    let second = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--budget-tokens",
            "20",
            "--cursor",
            &cursor,
            "--workspace",
            &workspace_arg,
            "--json",
        ])?,
        "recall --cursor resume",
    )?;
    let second_ids = item_memory_ids(&second);
    let mut union: Vec<String> = first_ids
        .iter()
        .cloned()
        .chain(second_ids.clone())
        .collect();
    let union_len = union.len();
    union.sort();
    union.dedup();
    if union.len() != union_len {
        return Err(format!(
            "pages must not duplicate items: {first_ids:?} + {second_ids:?}"
        ));
    }
    let mut expected = vec![MEM_RULE.to_owned(), MEM_FAILURE.to_owned()];
    expected.sort();
    if union != expected {
        return Err(format!(
            "pages must cover the full ranked set with no gaps: {first_ids:?} + {second_ids:?}"
        ));
    }
    Ok(())
}

#[test]
fn recall_rejects_garbage_cursor_with_empty_page() -> TestResult {
    let workspace = seed_recall_workspace()?;
    let response = parse_response(
        &run_ee(&[
            "recall",
            "--path",
            "src/db/*.rs",
            "--cursor",
            "not-a-valid-cursor",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall garbage cursor",
    )?;
    let codes = degraded_codes(&response);
    if !codes.iter().any(|code| code == "cursor_invalid") {
        return Err(format!("expected cursor_invalid, got {codes:?}"));
    }
    if !item_memory_ids(&response).is_empty() {
        return Err("a rejected cursor must yield an empty page, never a restart".to_owned());
    }
    Ok(())
}

#[test]
fn recall_diff_outside_git_degrades_and_never_blocks() -> TestResult {
    let workspace = seed_recall_workspace()?;
    // tempdir workspaces are not git worktrees, so the read-only shell-out
    // fails and the diff selector degrades to an empty path set while the
    // command still succeeds (a recall failure must never block an edit).
    let response = parse_response(
        &run_ee(&[
            "recall",
            "--diff-staged",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])?,
        "recall --diff-staged outside git",
    )?;
    let codes = degraded_codes(&response);
    if !codes.iter().any(|code| code == "recall_git_unavailable") {
        return Err(format!("expected recall_git_unavailable, got {codes:?}"));
    }
    if !item_memory_ids(&response).is_empty() {
        return Err("a degraded diff selector must contribute no matches".to_owned());
    }
    Ok(())
}

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join("ee.recall.v1.json");
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

/// Minimal structural validator covering the constructs the recall schema
/// uses (required sets, const, enums); the repo has no jsonschema engine.
fn validate_against_schema(schema: &Value, payload: &Value) -> TestResult {
    for field in string_set(schema, "/required")? {
        if payload.get(&field).is_none() {
            return Err(format!("payload missing required field {field}"));
        }
    }
    if payload.pointer("/schema").and_then(Value::as_str) != Some("ee.recall.v1") {
        return Err("payload schema must be ee.recall.v1".to_owned());
    }
    for field in string_set(schema, "/properties/query/required")? {
        if payload.pointer(&format!("/query/{field}")).is_none() {
            return Err(format!("query missing required field {field}"));
        }
    }
    let freshness_enum = string_set(
        schema,
        "/properties/items/items/properties/freshnessState/enum",
    )?;
    let item_required = string_set(schema, "/properties/items/items/required")?;
    for item in payload
        .pointer("/items")
        .and_then(Value::as_array)
        .ok_or("items must be an array")?
    {
        for field in &item_required {
            if item.get(field).is_none() {
                return Err(format!("item missing required field {field}"));
            }
        }
        let freshness = item
            .pointer("/freshnessState")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !freshness_enum.iter().any(|allowed| allowed == freshness) {
            return Err(format!("freshnessState {freshness:?} not in schema enum"));
        }
        for reference in item
            .pointer("/provenance")
            .and_then(Value::as_array)
            .ok_or("item provenance must be an array")?
        {
            if reference.pointer("/uri").and_then(Value::as_str).is_none()
                || reference
                    .pointer("/sourceType")
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err("provenance entries need uri and sourceType".to_owned());
            }
        }
    }
    Ok(())
}
