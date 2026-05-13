//! M3 contract test (eidetic_engine_cli bd-17c65.13.4).
//!
//! Asserts handoff capsule round-trip determinism. Same workspace
//! state, two consecutive `create_handoff` calls, expected outcomes:
//!
//! - `content_hash` differs because it includes volatile fields
//!   (capsule_id and created_at change every call). This is the
//!   honest baseline.
//! - `canonical_content_hash` matches because it strips those
//!   volatile fields. This is the M3 equivalence anchor: same
//!   workspace state → same canonical hash.
//! - Adding a new memory to the workspace between calls changes the
//!   canonical hash. Without this property, the hash would be a
//!   useless constant.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use ee::core::handoff::{CapsuleProfile, CreateOptions, CreateReport, create_handoff};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn build_workspace_with_seeds() -> Result<(TempDir, PathBuf), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    for (content, level, kind, tags) in [
        (
            "Always run cargo fmt --check before tagging a release.",
            "procedural",
            "rule",
            Some("release"),
        ),
        (
            "Adopt asupersync as the runtime substrate.",
            "semantic",
            "decision",
            Some("runtime,adr"),
        ),
    ] {
        let mut opts = base_remember_options(&workspace, &db_path);
        opts.content = content;
        opts.level = level;
        opts.kind = kind;
        opts.tags = tags;
        remember_memory(&opts).map_err(|error| format!("remember `{content}`: {error:?}"))?;
    }
    Ok((dir, workspace))
}

/// Construct a [`RememberMemoryOptions`] populated with the universal
/// defaults that don't vary between fixtures. Callers override the
/// `content`/`level`/`kind`/`tags` fields per memory. The helper exists
/// so adding a new option field to RememberMemoryOptions only requires
/// updating one place in this fixture.
fn base_remember_options<'a>(workspace: &'a Path, database: &'a Path) -> RememberMemoryOptions<'a> {
    RememberMemoryOptions {
        workspace_path: workspace,
        database_path: Some(database),
        content: "",
        workflow_id: None,
        level: "semantic",
        kind: "fact",
        tags: None,
        confidence: 0.85,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
        // `allow_secret_mention` was added concurrently by another
        // bead. If the field name changes, this `..Default::default()`
        // wouldn't help (RememberMemoryOptions doesn't implement
        // Default). Adding it here keeps the contract test self-contained.
        #[allow(clippy::field_reassign_with_default)]
        // The struct's full field set lives in src/core/memory.rs.
        allow_secret_mention: false,
    }
}

fn create_capsule(workspace: &Path, label: &str) -> Result<CreateReport, String> {
    let out_dir = workspace.join("out");
    std::fs::create_dir_all(&out_dir).map_err(|error| format!("mkdir out: {error}"))?;
    let capsule_path = out_dir.join(format!("{label}.json"));
    create_handoff(&CreateOptions {
        workspace: workspace.to_path_buf(),
        output: capsule_path,
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
    })
    .map_err(|error| format!("create_handoff: {error:?}"))
}

#[test]
fn two_creates_same_workspace_produce_same_canonical_hash() -> TestResult {
    let (_dir, workspace) = build_workspace_with_seeds()?;
    let first = create_capsule(&workspace, "first")?;
    let second = create_capsule(&workspace, "second")?;

    if first.canonical_content_hash.is_empty() {
        return Err("canonical_content_hash must not be empty after create".to_string());
    }
    if first.canonical_content_hash != second.canonical_content_hash {
        return Err(format!(
            "canonical_content_hash should match across two creates of the same workspace state:\n\
             first:  {}\n\
             second: {}",
            first.canonical_content_hash, second.canonical_content_hash
        ));
    }
    Ok(())
}

#[test]
fn two_creates_same_workspace_have_different_legacy_content_hashes() -> TestResult {
    // Sanity baseline: confirm the legacy content_hash differs across
    // two creates because it includes capsule_id + created_at. Without
    // this property, the canonical_content_hash test above wouldn't
    // be exercising a real strip.
    let (_dir, workspace) = build_workspace_with_seeds()?;
    let first = create_capsule(&workspace, "first")?;
    let second = create_capsule(&workspace, "second")?;
    if first.content_hash == second.content_hash {
        return Err(format!(
            "expected legacy content_hash to differ across creates (volatile fields differ), got identical hash {}",
            first.content_hash
        ));
    }
    Ok(())
}

#[test]
fn canonical_hash_changes_when_workspace_state_changes() -> TestResult {
    // bd-1xkxi (M3 follow-up): the capsule body now embeds a
    // deterministic workspace_state_hash computed from the memory set
    // (BLAKE3 over the sorted level/kind/content/tags projection).
    // Adding a memory perturbs that hash, which in turn perturbs the
    // canonical_content_hash. The capsule's canonical hash is now a
    // true workspace-state fingerprint, useful for cache validation
    // and round-trip verification across export/import cycles.
    let (_dir, workspace) = build_workspace_with_seeds()?;
    let before = create_capsule(&workspace, "before")?;

    let db_path = workspace.join(".ee").join("ee.db");
    let mut opts = base_remember_options(&workspace, &db_path);
    opts.content = "Newly seeded memory that should perturb the canonical hash.";
    opts.level = "episodic";
    opts.kind = "fact";
    opts.tags = Some("bench,perturbation");
    remember_memory(&opts).map_err(|error| format!("remember new memory: {error:?}"))?;

    let after = create_capsule(&workspace, "after")?;
    if before.canonical_content_hash == after.canonical_content_hash {
        return Err(format!(
            "canonical_content_hash must change when workspace state changes; before/after both {}",
            before.canonical_content_hash
        ));
    }
    Ok(())
}

#[test]
fn canonical_hash_is_stable_short_hex_prefix() -> TestResult {
    // The hash is a 16-char hex BLAKE3 prefix per compute_content_hash.
    // Pinning the shape catches future changes that quietly widen or
    // narrow it.
    let (_dir, workspace) = build_workspace_with_seeds()?;
    let capsule = create_capsule(&workspace, "shape")?;
    let h = &capsule.canonical_content_hash;
    if h.len() != 16 {
        return Err(format!(
            "canonical_content_hash should be 16 hex chars, got {} ({})",
            h.len(),
            h
        ));
    }
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "canonical_content_hash should be hex-only, got {h}"
        ));
    }
    Ok(())
}
