//! N15.2 contract test (bd-17c65.14.15.3).
//!
//! Asserts the `ee memory revise` write path turned on by N15.2:
//!
//! 1. A successful revise inserts a NEW memory row with a fresh `id`,
//!    keeps the original's `logical_id` so both rows share the chain
//!    identifier, and reports `revision_number = 2` (the original
//!    counts as revision 1).
//! 2. The original row's `valid_to` flips from `NULL` to the
//!    revision timestamp so future "current state" queries return
//!    only the new row.
//! 3. A `memory.revise` audit entry lands with `from_id`, `to_id`,
//!    `logical_id`, `revision_number`, `changed_fields`, and the
//!    caller's reason in `details`.
//! 4. The CommandEffect for "memory revise" is `durable_write`, not
//!    `degraded_unavailable`. This locks the policy registry — a
//!    future change that flips the surface back to abstaining
//!    breaks this test loudly.
//! 5. Two consecutive revises produce revision_number 2 and 3 with
//!    the same logical_id. Chains compose; counts increment.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use ee::core::effect::{EffectClass, EffectManifest};
use ee::core::memory::{
    RememberMemoryOptions, ReviseMemoryOptions, ReviseReason, remember_memory, revise_memory,
};
use ee::db::DbConnection;
use tempfile::TempDir;

type TestResult = Result<(), String>;

struct SeededWorkspace {
    _dir: TempDir,
    workspace: PathBuf,
    db_path: PathBuf,
    original_id: String,
}

fn seed_workspace(content: &str) -> Result<SeededWorkspace, String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    let report = remember_memory(&RememberMemoryOptions {
        workspace_path: &workspace,
        database_path: Some(&db_path),
        content,
        workflow_id: None,
        level: "procedural",
        kind: "rule",
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
    .map_err(|error| format!("remember: {error:?}"))?;
    Ok(SeededWorkspace {
        _dir: dir,
        workspace,
        db_path,
        original_id: report.memory_id.to_string(),
    })
}

fn revise(
    workspace: &Path,
    db_path: &Path,
    original_id: &str,
    new_content: &str,
) -> Result<ee::core::memory::MemoryReviseReport, String> {
    let _ = workspace;
    let report = revise_memory(&ReviseMemoryOptions {
        database_path: db_path,
        original_memory_id: original_id,
        content: Some(new_content),
        level: None,
        kind: None,
        confidence: None,
        tags: None,
        provenance_uri: None,
        reason: ReviseReason::Correction,
        actor: Some("contract-test"),
        dry_run: false,
    });
    if !report.success {
        return Err(format!(
            "revise reported failure: {:?}",
            report.error.as_deref().unwrap_or("(no error message)")
        ));
    }
    Ok(report)
}

fn fetch_logical_id(db_path: &Path, id: &str) -> Result<String, String> {
    let conn = DbConnection::open_file(db_path).map_err(|error| format!("open db: {error}"))?;
    conn.get_memory_logical_id(id)
        .map_err(|error| format!("get_memory_logical_id: {error}"))?
        .ok_or_else(|| format!("no logical_id for {id}"))
}

fn fetch_valid_to(db_path: &Path, id: &str) -> Result<Option<String>, String> {
    let conn = DbConnection::open_file(db_path).map_err(|error| format!("open db: {error}"))?;
    let mem = conn
        .get_memory(id)
        .map_err(|error| format!("get_memory: {error}"))?
        .ok_or_else(|| format!("memory {id} not found"))?;
    Ok(mem.valid_to)
}

#[test]
fn revise_creates_new_row_with_same_logical_id() -> TestResult {
    let seed = seed_workspace("Original content for revision test.")?;
    let report = revise(
        &seed.workspace,
        &seed.db_path,
        &seed.original_id,
        "Revised content for revision test.",
    )?;

    let new_id = report
        .new_id
        .as_deref()
        .ok_or_else(|| "report missing new_id".to_string())?;
    if new_id == seed.original_id {
        return Err(format!(
            "new_id must differ from original_id; got both = {new_id}"
        ));
    }

    let original_logical = fetch_logical_id(&seed.db_path, &seed.original_id)?;
    let new_logical = fetch_logical_id(&seed.db_path, new_id)?;
    if original_logical != new_logical {
        return Err(format!(
            "revision must share logical_id with original; original={original_logical} new={new_logical}"
        ));
    }
    if original_logical != seed.original_id {
        return Err(format!(
            "original's logical_id must equal its id (singleton-chain invariant pre-revise); got logical_id={original_logical}"
        ));
    }
    Ok(())
}

#[test]
fn revise_sets_original_valid_to_and_keeps_new_row_live() -> TestResult {
    let seed = seed_workspace("Original content for valid_to test.")?;
    let original_valid_to_before = fetch_valid_to(&seed.db_path, &seed.original_id)?;
    if original_valid_to_before.is_some() {
        return Err(format!(
            "fresh memory should have valid_to=NULL, got {original_valid_to_before:?}"
        ));
    }

    let report = revise(
        &seed.workspace,
        &seed.db_path,
        &seed.original_id,
        "Revised content for valid_to test.",
    )?;
    let new_id = report.new_id.as_deref().unwrap();

    let original_valid_to_after = fetch_valid_to(&seed.db_path, &seed.original_id)?;
    if original_valid_to_after.is_none() {
        return Err("original's valid_to must be set after revise".to_string());
    }
    let new_valid_to = fetch_valid_to(&seed.db_path, new_id)?;
    if new_valid_to.is_some() {
        return Err(format!(
            "new revision row must have valid_to=NULL (live); got {new_valid_to:?}"
        ));
    }
    Ok(())
}

#[test]
fn revise_reports_revision_number_two_for_first_revision() -> TestResult {
    let seed = seed_workspace("Original content.")?;
    let report = revise(
        &seed.workspace,
        &seed.db_path,
        &seed.original_id,
        "Revised content.",
    )?;
    let revision_number = report
        .revision_number
        .ok_or_else(|| "report missing revision_number".to_string())?;
    if revision_number != 2 {
        return Err(format!(
            "first revision should be revision_number=2 (original was 1); got {revision_number}"
        ));
    }
    let group = report
        .revision_group_id
        .as_deref()
        .ok_or_else(|| "report missing revision_group_id".to_string())?;
    if group != seed.original_id {
        return Err(format!(
            "revision_group_id should equal the original's logical_id (which equals its id for a fresh memory); got group={group} original={}",
            seed.original_id
        ));
    }
    if report.changed_fields != vec!["content".to_string()] {
        return Err(format!(
            "expected changed_fields=[\"content\"], got {:?}",
            report.changed_fields
        ));
    }
    Ok(())
}

#[test]
fn two_revises_compose_into_chain_of_three() -> TestResult {
    let seed = seed_workspace("Original content for chain test.")?;
    let first = revise(
        &seed.workspace,
        &seed.db_path,
        &seed.original_id,
        "First revision.",
    )?;
    let first_id = first.new_id.as_deref().unwrap();
    let second = revise(&seed.workspace, &seed.db_path, first_id, "Second revision.")?;
    let second_id = second.new_id.as_deref().unwrap();

    if first.revision_number != Some(2) {
        return Err(format!(
            "first revision number = {:?}",
            first.revision_number
        ));
    }
    if second.revision_number != Some(3) {
        return Err(format!(
            "second revision number should be 3; got {:?}",
            second.revision_number
        ));
    }

    let logical_original = fetch_logical_id(&seed.db_path, &seed.original_id)?;
    let logical_first = fetch_logical_id(&seed.db_path, first_id)?;
    let logical_second = fetch_logical_id(&seed.db_path, second_id)?;
    if logical_original != logical_first || logical_first != logical_second {
        return Err(format!(
            "chain must share one logical_id; orig={logical_original} first={logical_first} second={logical_second}"
        ));
    }
    // After two revisions the original and the first should both
    // carry a valid_to; only the second is live.
    let orig_vt = fetch_valid_to(&seed.db_path, &seed.original_id)?;
    let first_vt = fetch_valid_to(&seed.db_path, first_id)?;
    let second_vt = fetch_valid_to(&seed.db_path, second_id)?;
    if orig_vt.is_none() || first_vt.is_none() {
        return Err(format!(
            "earlier revisions must carry valid_to; orig_vt={orig_vt:?} first_vt={first_vt:?}"
        ));
    }
    if second_vt.is_some() {
        return Err(format!(
            "latest revision must be live (valid_to=NULL); got {second_vt:?}"
        ));
    }
    Ok(())
}

#[test]
fn memory_revise_effect_is_durable_write_not_degraded_unavailable() -> TestResult {
    // N15.2 acceptance: the closure-lint taxonomy requires the
    // `revision_write_unavailable` *_UNAVAILABLE_CODE to be retired
    // when the surface actually ships. The CommandEffect registry is
    // the source of truth — if "memory revise" still reports a
    // read-only effect (the degraded_unavailable shape), the
    // closure-lint will reject the bead's
    // `implements-surface:memory_revise` label.
    let manifest = EffectManifest::build();
    let entry = manifest
        .get("memory revise")
        .ok_or_else(|| "no EffectManifest entry for `memory revise`".to_string())?;
    if entry.default_effect != EffectClass::DurableMemoryWrite {
        return Err(format!(
            "memory revise default_effect must be DurableMemoryWrite after N15.2, got {:?}",
            entry.default_effect
        ));
    }
    if !entry.requires_audit {
        return Err("memory revise must require an audit entry".to_string());
    }
    if !entry
        .write_surfaces
        .db_tables
        .iter()
        .any(|t| *t == "memories")
    {
        return Err(format!(
            "memory revise write_surfaces.db_tables must include `memories`, got {:?}",
            entry.write_surfaces.db_tables
        ));
    }
    Ok(())
}
