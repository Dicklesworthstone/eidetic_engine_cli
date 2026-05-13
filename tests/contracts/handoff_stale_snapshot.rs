//! M4 contract test (eidetic_engine_cli bd-17c65.13.5).
//!
//! Asserts the stale-snapshot detection surface added in M4:
//!
//! - `ee handoff resume` always emits `stale_snapshot` in the report
//!   when the capsule embeds a `workspace_state_hash` (which it does
//!   after M3 / bd-1xkxi).
//! - When the workspace hasn't changed between capture and resume,
//!   `stale_snapshot.drift_detected` is `false` and no
//!   `handoff_snapshot_stale` degradation fires.
//! - When the workspace memory set changes between capture and
//!   resume, `drift_detected` flips to `true` and the degradation
//!   fires with severity that signals the agent to refresh.
//! - With `require_fresh: true` set, drift converts the resume into
//!   a `UnsatisfiedDegradedMode` error (exit code 6) so callers in
//!   strict mode never silently consume stale context.
//! - Per-signal counts (`memories_added_since`,
//!   `memories_expired_since`, `memories_revised_since`) are `None`
//!   in phase 1 — the capsule does not yet embed the captured
//!   memory ID set. Emitting `Some(0)` would be a lie; explicit
//!   `null` is the honest signal that the count is unmeasured.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use ee::core::handoff::{
    CapsuleProfile, CreateOptions, ResumeOptions, create_handoff, resume_handoff,
};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use tempfile::TempDir;

type TestResult = Result<(), String>;

struct CapsuleFixture {
    _dir: TempDir,
    workspace: PathBuf,
    capsule_path: PathBuf,
}

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
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    }
}

fn build_capsule_with_seeds() -> Result<CapsuleFixture, String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    let mut opts = base_remember_options(&workspace, &db_path);
    opts.content = "Always run cargo fmt --check before tagging a release.";
    opts.level = "procedural";
    opts.kind = "rule";
    opts.tags = Some("release");
    remember_memory(&opts).map_err(|error| format!("remember seed: {error:?}"))?;

    let capsule_path = workspace.join("out").join("capsule.json");
    std::fs::create_dir_all(capsule_path.parent().expect("out parent"))
        .map_err(|error| format!("mkdir out: {error}"))?;
    create_handoff(&CreateOptions {
        workspace: workspace.clone(),
        output: capsule_path.clone(),
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
    })
    .map_err(|error| format!("create_handoff: {error:?}"))?;

    Ok(CapsuleFixture {
        _dir: dir,
        workspace,
        capsule_path,
    })
}

fn base_resume_options(fixture: &CapsuleFixture) -> ResumeOptions {
    ResumeOptions {
        path: fixture.capsule_path.clone(),
        use_latest: false,
        workspace: fixture.workspace.clone(),
        max_sections: None,
        task_frame_id: None,
        bound_workspace_id: None,
        bound_workspace_identity: None,
        include_prompt_fragment: false,
        require_fresh: false,
    }
}

#[test]
fn stale_snapshot_is_present_and_clean_for_unchanged_workspace() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;

    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot must be present after M3+M4 land".to_string())?;
    if stale.captured_state_hash.is_none() {
        return Err("captured_state_hash must populate from the capsule body".to_string());
    }
    if stale.current_state_hash.is_none() {
        return Err("current_state_hash must populate from the live database".to_string());
    }
    if stale.drift_detected {
        return Err(format!(
            "unchanged workspace should not show drift; captured={:?} current={:?}",
            stale.captured_state_hash, stale.current_state_hash
        ));
    }
    if report
        .degradations
        .iter()
        .any(|d| d.code.as_str() == "handoff_snapshot_stale")
    {
        return Err("handoff_snapshot_stale must not fire when nothing changed".to_string());
    }
    Ok(())
}

#[test]
fn stale_snapshot_flips_to_drift_when_memory_set_changes() -> TestResult {
    let fixture = build_capsule_with_seeds()?;

    // Mutate the workspace between capture and resume by adding a new
    // memory. workspace_state_hash should perturb (per bd-1xkxi).
    let db_path = fixture.workspace.join(".ee").join("ee.db");
    let mut opts = base_remember_options(&fixture.workspace, &db_path);
    opts.content = "Newly seeded memory that perturbs the workspace state hash.";
    opts.level = "episodic";
    opts.kind = "fact";
    opts.tags = Some("bench,perturbation");
    remember_memory(&opts).map_err(|error| format!("remember new: {error:?}"))?;

    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;
    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot missing after drift".to_string())?;
    if !stale.drift_detected {
        return Err(format!(
            "drift must be detected after adding a memory; captured={:?} current={:?}",
            stale.captured_state_hash, stale.current_state_hash
        ));
    }
    let fired = report
        .degradations
        .iter()
        .any(|d| d.code.as_str() == "handoff_snapshot_stale");
    if !fired {
        return Err(format!(
            "handoff_snapshot_stale must fire on drift; got degradations={:?}",
            report
                .degradations
                .iter()
                .map(|d| d.code.as_str())
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn require_fresh_converts_drift_into_error() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    let db_path = fixture.workspace.join(".ee").join("ee.db");
    let mut opts = base_remember_options(&fixture.workspace, &db_path);
    opts.content = "Drift-trigger memory under require_fresh mode.";
    opts.tags = Some("strict");
    remember_memory(&opts).map_err(|error| format!("remember new: {error:?}"))?;

    let mut options = base_resume_options(&fixture);
    options.require_fresh = true;

    let result = resume_handoff(&options);
    match result {
        Ok(report) => Err(format!(
            "require_fresh=true on a drifted capsule must error, not return Ok; \
             drift_detected={:?}",
            report.stale_snapshot.as_ref().map(|s| s.drift_detected)
        )),
        Err(error) => {
            // The error must signal degraded-but-could-not-satisfy
            // semantics (AGENTS.md exit code 6). Check the code path
            // by sniffing the variant via Debug — coupling to the
            // exact variant name is acceptable here because changing
            // the variant is a public API change that should break
            // this test.
            let debug = format!("{error:?}");
            if !debug.contains("UnsatisfiedDegradedMode") {
                return Err(format!(
                    "require_fresh drift must map to UnsatisfiedDegradedMode (exit 6); got {debug}"
                ));
            }
            Ok(())
        }
    }
}

#[test]
fn require_fresh_does_not_error_on_clean_resume() -> TestResult {
    // Symmetry check: when there's no drift, require_fresh=true must
    // still succeed. Without this, callers in strict mode could not
    // ever consume a capsule, defeating the purpose.
    let fixture = build_capsule_with_seeds()?;
    let mut options = base_resume_options(&fixture);
    options.require_fresh = true;
    let report = resume_handoff(&options).map_err(|error| {
        format!("require_fresh=true on a clean resume must succeed; got error: {error:?}")
    })?;
    if let Some(stale) = &report.stale_snapshot {
        if stale.drift_detected {
            return Err("clean resume must not show drift".to_string());
        }
    }
    Ok(())
}

#[test]
fn per_signal_counts_are_explicitly_unmeasured_in_phase_1() -> TestResult {
    // M4 phase 1 ships hash-based drift detection only. The
    // per-signal counts (added/expired/revised) require the capsule
    // to embed the captured memory ID set, which is a follow-up
    // bead. In the meantime, the contract is: explicit `None`, not
    // `Some(0)` — agents must be able to tell the difference between
    // "measured zero" and "unmeasured".
    let fixture = build_capsule_with_seeds()?;
    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;
    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot missing".to_string())?;
    if stale.memories_added_since.is_some() {
        return Err(format!(
            "memories_added_since should be None in phase 1, got {:?}",
            stale.memories_added_since
        ));
    }
    if stale.memories_expired_since.is_some() {
        return Err(format!(
            "memories_expired_since should be None in phase 1, got {:?}",
            stale.memories_expired_since
        ));
    }
    if stale.memories_revised_since.is_some() {
        return Err(format!(
            "memories_revised_since should be None in phase 1, got {:?}",
            stale.memories_revised_since
        ));
    }
    Ok(())
}
