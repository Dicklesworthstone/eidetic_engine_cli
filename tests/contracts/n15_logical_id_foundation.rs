//! N15.1 foundation contract test (bd-17c65.14.15.2).
//!
//! N15.1 introduces immutable revisions: every memory belongs to a
//! revision chain whose members share a `logical_id` but carry
//! distinct `id` values. This contract test pins the **foundation**
//! that V043 ships:
//!
//! - Every memory row has a populated `logical_id`.
//! - For a freshly inserted memory the chain is a singleton, so
//!   `logical_id == id`. The future `ee memory revise` write path
//!   will extend the chain by appending a sibling row with the same
//!   `logical_id` and a fresh `id`; until that path lands the
//!   singleton invariant holds for every row.
//! - Two distinct memories produce distinct `logical_id` values
//!   (no accidental collapse to a workspace-wide default).
//!
//! Tests use the in-process `DbConnection` API + `remember_memory`
//! so the assertions exercise the canonical insert path that the
//! CLI uses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn build_workspace() -> Result<(TempDir, std::path::PathBuf), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);
    Ok((dir, workspace))
}

fn remember(workspace: &Path, content: &str) -> Result<String, String> {
    let db_path = workspace.join(".ee").join("ee.db");
    let report = remember_memory(&RememberMemoryOptions {
        workspace_path: workspace,
        database_path: Some(&db_path),
        content,
        workflow_id: None,
        level: "semantic",
        kind: "fact",
        tags: None,
        confidence: 0.85,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .map_err(|error| format!("remember `{content}`: {error:?}"))?;
    Ok(report.memory_id.to_string())
}

fn fetch_logical_id(workspace: &Path, id: &str) -> Result<String, String> {
    let db_path = workspace.join(".ee").join("ee.db");
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.get_memory_logical_id(id)
        .map_err(|error| format!("get_memory_logical_id: {error}"))?
        .ok_or_else(|| format!("no logical_id row found for memory id {id}"))
}

#[test]
fn new_memory_has_logical_id_equal_to_id() -> TestResult {
    let (_dir, workspace) = build_workspace()?;
    let memory_id = remember(&workspace, "First seeded memory for logical_id check.")?;
    let logical = fetch_logical_id(&workspace, &memory_id)?;
    if logical != memory_id {
        return Err(format!(
            "newly inserted memory should have logical_id == id; \
             got id={memory_id} logical_id={logical}"
        ));
    }
    Ok(())
}

#[test]
fn distinct_memories_have_distinct_logical_ids() -> TestResult {
    let (_dir, workspace) = build_workspace()?;
    let id_a = remember(&workspace, "Memory A — first.")?;
    let id_b = remember(&workspace, "Memory B — second.")?;
    if id_a == id_b {
        return Err(format!(
            "remember should produce distinct memory ids; got two of {id_a}"
        ));
    }
    let logical_a = fetch_logical_id(&workspace, &id_a)?;
    let logical_b = fetch_logical_id(&workspace, &id_b)?;
    if logical_a == logical_b {
        return Err(format!(
            "distinct memories must not collapse to the same logical_id; \
             a={logical_a} b={logical_b}"
        ));
    }
    Ok(())
}

#[test]
fn logical_id_lookup_returns_none_for_unknown_id() -> TestResult {
    let (_dir, workspace) = build_workspace()?;
    let db_path = workspace.join(".ee").join("ee.db");
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    let result = conn
        .get_memory_logical_id("mem_definitely_not_real_xx")
        .map_err(|error| format!("get_memory_logical_id: {error}"))?;
    if result.is_some() {
        return Err(format!("unknown memory id must yield None; got {result:?}"));
    }
    Ok(())
}

#[test]
fn revision_chain_singleton_invariant_holds_for_every_memory() -> TestResult {
    // The N15.1 foundation guarantees that until the revise write
    // path lands, every memory's chain is a singleton — its
    // logical_id equals its id. Seed a small batch and verify the
    // invariant across all of them in one sweep, so a future change
    // that accidentally diverges the two values gets caught even
    // when it slips past the per-memory test.
    let (_dir, workspace) = build_workspace()?;
    let mut ids = Vec::new();
    for content in [
        "Always run cargo fmt --check before tagging a release.",
        "Adopt asupersync as the runtime substrate.",
        "BLAKE3 short hash is 16 hex chars in pack envelopes.",
        "Workspace state hash feeds capsule canonical_content_hash.",
        "Pack records persist under .ee/ee.db with pack_ prefix IDs.",
    ] {
        ids.push(remember(&workspace, content)?);
    }
    for id in &ids {
        let logical = fetch_logical_id(&workspace, id)?;
        if &logical != id {
            return Err(format!(
                "singleton-chain invariant violated for {id}: logical_id={logical}"
            ));
        }
    }
    Ok(())
}
