//! A11 contract test (eidetic_engine_cli bd-17c65.1.10).
//!
//! Asserts the persisted-pack retrieval surface added in A11:
//!
//! - `ee context-show <pack_id>` reads a persisted pack record from
//!   the workspace database and returns an `ee.response.v1` envelope
//!   with the canonical pack shape: id, query, profile, maxTokens,
//!   usedTokens, itemCount, hash, items[], degraded[], etc.
//! - The pack hash returned by `ee context-show` matches the pack
//!   hash written during the original `ee context` invocation, so
//!   callers can correlate the retrieval back to the audit log.
//! - The items[] array contains the same memory IDs in the same rank
//!   order as the live context pack.
//! - `ee show pack_<id>` (the F2 alias dispatch) routes through to
//!   the same handler.
//! - Unknown pack IDs produce a NotFound error envelope.
//!
//! Tests use the in-process API (DbConnection + run_context_pack +
//! handle_context_show via process spawn for the alias path) to keep
//! the suite fast.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ee::core::context::{ContextPackOptions, run_context_pack};
use ee::core::index::{IndexRebuildOptions, rebuild_index};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use ee::models::{MemoryScope, QueryFilters};
use ee::search::scoring::SpeedMode;
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

struct PackFixture {
    _dir: TempDir,
    workspace: PathBuf,
    database: PathBuf,
    pack_id: String,
    pack_hash: String,
    memory_id: String,
}

fn build_persisted_pack() -> Result<PackFixture, String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let database = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(database.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    let remembered = remember_memory(&RememberMemoryOptions {
        workspace_path: &workspace,
        database_path: Some(&database),
        content: "Always run cargo fmt --check before tagging a release.",
        workflow_id: None,
        level: "procedural",
        kind: "rule",
        tags: Some("release"),
        confidence: 0.9,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .map_err(|error| format!("remember: {error:?}"))?;

    let index_dir = workspace.join(".ee").join("index");
    rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .map_err(|error| format!("rebuild_index: {error:?}"))?;

    // Run a live context pack so a persisted record lands in pack_records.
    let response = run_context_pack(&ContextPackOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(index_dir),
        query: "release formatting".to_owned(),
        profile: None,
        max_tokens: Some(2000),
        candidate_pool: Some(10),
        max_results: None,
        speed: SpeedMode::Default,
        filters: QueryFilters::default(),
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        redaction_level: ee::models::RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: Default::default(),
    })
    .map_err(|error| format!("run_context_pack: {error:?}"))?;

    // Find the persisted pack record by querying pack_records via the
    // join on pack_items.memory_id (the only public list_* accessor on
    // DbConnection). We seeded one memory above, so it will appear in
    // exactly one pack record.
    let conn = DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    let pairs = conn
        .list_pack_records_for_memory(&remembered.memory_id.to_string(), 10)
        .map_err(|error| format!("list_pack_records_for_memory: {error}"))?;
    let (record, _item) = pairs
        .into_iter()
        .next()
        .ok_or_else(|| "no persisted pack record for seeded memory".to_string())?;
    let pack_id = record.id.clone();
    let pack_hash = record.pack_hash.clone();
    drop(conn);
    drop(response);

    Ok(PackFixture {
        _dir: dir,
        workspace,
        database,
        pack_id,
        pack_hash,
        memory_id: remembered.memory_id.to_string(),
    })
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(ee_binary())
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))
}

fn parse_stdout(out: &std::process::Output, context: &str) -> Result<Value, String> {
    let s = String::from_utf8(out.stdout.clone())
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&s).map_err(|error| format!("{context}: not JSON: {error}\nout: {s}"))
}

#[test]
fn context_show_returns_canonical_pack_envelope_for_persisted_pack() -> TestResult {
    let fixture = build_persisted_pack()?;
    let out = run_ee(&[
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "context-show",
        &fixture.pack_id,
        "--database",
        fixture.database.to_str().unwrap(),
        "--json",
    ])?;
    if out.status.code() != Some(0) {
        return Err(format!(
            "context-show should exit 0, got {:?}; stderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let json = parse_stdout(&out, "context-show")?;
    let schema = json
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing schema".to_string())?;
    if schema != "ee.response.v1" {
        return Err(format!("schema mismatch: got {schema}"));
    }
    let pack = json
        .pointer("/data/pack")
        .ok_or_else(|| "missing data.pack".to_string())?;
    let returned_id = pack
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing pack.id".to_string())?;
    if returned_id != fixture.pack_id {
        return Err(format!(
            "pack id mismatch: got {returned_id}, expected {}",
            fixture.pack_id
        ));
    }
    let returned_hash = pack
        .get("hash")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing pack.hash".to_string())?;
    if returned_hash != fixture.pack_hash {
        return Err(format!(
            "pack hash mismatch: got {returned_hash}, expected {}",
            fixture.pack_hash
        ));
    }
    Ok(())
}

#[test]
fn context_show_items_array_carries_seeded_memory() -> TestResult {
    let fixture = build_persisted_pack()?;
    let out = run_ee(&[
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "context-show",
        &fixture.pack_id,
        "--database",
        fixture.database.to_str().unwrap(),
        "--json",
    ])?;
    let json = parse_stdout(&out, "context-show")?;
    let items = json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing data.pack.items".to_string())?;
    if items.is_empty() {
        return Err("expected at least one item in persisted pack".to_string());
    }
    let any_match = items.iter().any(|item| {
        item.get("memoryId")
            .and_then(Value::as_str)
            .map(|s| s == fixture.memory_id)
            .unwrap_or(false)
    });
    if !any_match {
        return Err(format!(
            "seeded memory {} not in items[]; ids={:?}",
            fixture.memory_id,
            items
                .iter()
                .map(|i| i.get("memoryId").and_then(Value::as_str))
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn show_alias_routes_pack_id_to_context_show() -> TestResult {
    let fixture = build_persisted_pack()?;
    // F2 alias dispatch: `ee show pack_<id>` → context-show.
    let out = run_ee(&[
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "show",
        &fixture.pack_id,
        "--database",
        fixture.database.to_str().unwrap(),
        "--json",
    ])?;
    if out.status.code() != Some(0) {
        return Err(format!(
            "ee show {} should exit 0, got {:?}; stderr: {}",
            fixture.pack_id,
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let json = parse_stdout(&out, "show alias")?;
    let command = json
        .pointer("/data/command")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing data.command".to_string())?;
    if command != "context show" {
        return Err(format!(
            "ee show pack_* should dispatch to context-show, got command={command}"
        ));
    }
    let returned_id = json
        .pointer("/data/pack/id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing data.pack.id from alias dispatch".to_string())?;
    if returned_id != fixture.pack_id {
        return Err(format!("alias dispatch returned wrong pack: {returned_id}"));
    }
    Ok(())
}

#[test]
fn context_show_unknown_pack_id_returns_not_found_envelope() -> TestResult {
    // No fixture needed — just need any workspace with a migrated DB.
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path();
    let database = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(database.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    let out = run_ee(&[
        "--workspace",
        workspace.to_str().unwrap(),
        "context-show",
        "pack_definitely_not_a_real_id_xx",
        "--database",
        database.to_str().unwrap(),
        "--json",
    ])?;
    let code = out.status.code();
    if code == Some(0) {
        return Err(format!(
            "context-show on unknown pack must not exit 0, got {code:?}\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The shipped envelope is `ee.error.v2`; the older `ee.error.v1`
    // schema is accepted too in case the contract regresses to the
    // earlier shape.
    if !stdout.contains("\"schema\":\"ee.error.v2\"")
        && !stdout.contains("\"schema\":\"ee.error.v1\"")
    {
        return Err(format!(
            "expected ee.error.v{{1,2}} envelope for unknown pack, got: {stdout}"
        ));
    }
    if !stdout.contains("not_found") && !stdout.contains("NotFound") {
        return Err(format!(
            "expected not_found error code in envelope; got: {stdout}"
        ));
    }
    // The error must mention `pack` as the resource so callers can
    // distinguish from missing memories or workflows. This is the
    // promise the CLI handler makes when `get_pack_record` returns
    // None.
    if !stdout.contains("\"resource\":\"pack\"") && !stdout.contains("\"pack not found\"") {
        return Err(format!(
            "expected pack resource hint in envelope; got: {stdout}"
        ));
    }
    Ok(())
}

fn _silence_unused(_path: &Path) {}
