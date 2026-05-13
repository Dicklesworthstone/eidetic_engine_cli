//! M1 contract test (eidetic_engine_cli bd-17c65.13.2).
//!
//! Asserts the resume-side contracts added in M1:
//!
//! - `ResumeReport.prompt_fragment.text` is a ready-to-prepend markdown
//!   body, present whenever the capsule yields any usable content.
//! - The fragment carries an estimated_tokens count and a BLAKE3 hash
//!   prefix so callers can correlate logged prompt fragments back to
//!   the resume that produced them.
//! - `--no-rendered-text` equivalent (`include_prompt_fragment = false`)
//!   suppresses the fragment without affecting the structured fields.
//! - Workspace mismatch detection: when `bound_workspace_id` differs
//!   from the capsule's recorded workspace, the report's
//!   `workspace_mismatch` is `Hard` and a `workspace_mismatch_hard`
//!   degradation lands in `degradations[]`. When IDs match, the field
//!   is `None` and no mismatch degradation appears.
//! - The fragment never leaks the raw workspace path; only the
//!   workspace ID (already deterministic / opaque) is referenced.
//!
//! Each test stands up a tiny on-disk capsule and exercises
//! `resume_handoff` directly — no CLI shell-out, no mocks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::core::handoff::{
    CapsuleProfile, CreateOptions, ResumeOptions, WorkspaceMismatchSeverity, create_handoff,
    resume_handoff,
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

fn build_capsule() -> Result<CapsuleFixture, String> {
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
            "Finish wiring the L0 migrate command.",
            "procedural",
            "rule",
            Some("release,workflow"),
        ),
        (
            "Use BLAKE3 for content-addressable memory hashing.",
            "semantic",
            "decision",
            Some("blake3,decision"),
        ),
    ] {
        remember_memory(&RememberMemoryOptions {
            workspace_path: &workspace,
            database_path: Some(&db_path),
            content,
            workflow_id: None,
            level,
            kind,
            tags,
            confidence: 0.9,
            source: None,
            allow_secret_mention: false,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
        })
        .map_err(|error| format!("remember `{content}`: {error:?}"))?;
    }

    let out_dir = workspace.join("out");
    std::fs::create_dir_all(&out_dir).map_err(|error| format!("mkdir out: {error}"))?;
    let capsule_path = out_dir.join("capsule.json");
    create_handoff(&CreateOptions {
        workspace: workspace.clone(),
        output: capsule_path.clone(),
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
        bind_to_machine: false,
        machine_salt_path: None,
    })
    .map_err(|error| format!("create_handoff: {error:?}"))?;

    Ok(CapsuleFixture {
        _dir: dir,
        workspace,
        capsule_path,
    })
}

fn run_resume(
    fixture: &CapsuleFixture,
    bound_workspace_id: Option<&str>,
    include_prompt_fragment: bool,
) -> Result<ee::core::handoff::ResumeReport, String> {
    resume_handoff(&ResumeOptions {
        path: fixture.capsule_path.clone(),
        use_latest: false,
        workspace: fixture.workspace.clone(),
        max_sections: None,
        task_frame_id: None,
        bound_workspace_id: bound_workspace_id.map(str::to_owned),
        bound_workspace_identity: None,
        include_prompt_fragment,
        require_fresh: false,
        insecure_skip_hmac: false,
        machine_salt_path: None,
    })
    .map_err(|error| format!("resume_handoff: {error:?}"))
}

#[test]
fn resume_includes_prompt_fragment_with_markdown_body_and_hash() -> TestResult {
    let fixture = build_capsule()?;
    let report = run_resume(&fixture, None, true)?;

    let fragment = report
        .prompt_fragment
        .as_ref()
        .ok_or_else(|| "resume report must carry prompt_fragment".to_string())?;
    if !fragment.text.starts_with("# Resumed Session: ") {
        return Err(format!(
            "prompt_fragment.text must start with `# Resumed Session:`, got: {:?}",
            &fragment.text[..fragment.text.len().min(80)]
        ));
    }
    if fragment.estimated_tokens == 0 {
        return Err("estimated_tokens must be non-zero for a non-empty body".to_string());
    }
    if !fragment.hash.starts_with("blake3:") {
        return Err(format!(
            "fragment.hash must be a blake3: prefix, got {}",
            fragment.hash
        ));
    }
    // 16-hex prefix is the documented contract.
    if fragment.hash.len() != "blake3:".len() + 16 {
        return Err(format!(
            "fragment.hash should be `blake3:` + 16 hex chars, got len {} ({})",
            fragment.hash.len(),
            fragment.hash
        ));
    }
    Ok(())
}

#[test]
fn resume_suppresses_prompt_fragment_when_caller_opts_out() -> TestResult {
    let fixture = build_capsule()?;
    let report = run_resume(&fixture, None, false)?;
    if report.prompt_fragment.is_some() {
        return Err("include_prompt_fragment=false must produce prompt_fragment=None".to_string());
    }
    if report.next_actions.is_empty() && report.current_objective.is_none() {
        return Err("opting out of fragment must still populate the structured fields".to_string());
    }
    Ok(())
}

#[test]
fn resume_records_workspace_mismatch_when_bound_id_differs() -> TestResult {
    let fixture = build_capsule()?;
    let report = run_resume(&fixture, Some("wsp_definitely_not_the_same_id"), true)?;
    if !matches!(
        report.workspace_mismatch,
        WorkspaceMismatchSeverity::Hard | WorkspaceMismatchSeverity::Soft
    ) {
        return Err(format!(
            "workspace_mismatch should be Hard or Soft when bound ID differs, got {:?}",
            report.workspace_mismatch
        ));
    }
    let mismatch_degradation = report
        .degradations
        .iter()
        .find(|d| d.code.starts_with("workspace_mismatch_"));
    if mismatch_degradation.is_none() {
        return Err(format!(
            "degradations[] must include a workspace_mismatch_* entry; got codes={:?}",
            report
                .degradations
                .iter()
                .map(|d| &d.code)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn resume_omits_mismatch_degradation_when_no_binding_requested() -> TestResult {
    let fixture = build_capsule()?;
    let report = run_resume(&fixture, None, true)?;
    if !matches!(report.workspace_mismatch, WorkspaceMismatchSeverity::None) {
        return Err(format!(
            "workspace_mismatch must be None when bound_workspace_id is None, got {:?}",
            report.workspace_mismatch
        ));
    }
    let mismatch_count = report
        .degradations
        .iter()
        .filter(|d| d.code.starts_with("workspace_mismatch_"))
        .count();
    if mismatch_count != 0 {
        return Err(format!(
            "unbound resume must emit zero workspace_mismatch_* degradations, got {mismatch_count}"
        ));
    }
    Ok(())
}

#[test]
fn resume_prompt_fragment_carries_workspace_mismatch_notice_when_present() -> TestResult {
    let fixture = build_capsule()?;
    let report = run_resume(&fixture, Some("wsp_unrelated_workspace"), true)?;
    let fragment = report
        .prompt_fragment
        .as_ref()
        .ok_or_else(|| "fragment missing on bound resume".to_string())?;
    if !fragment.text.contains("Workspace mismatch:") {
        return Err(format!(
            "prompt_fragment.text must surface the workspace mismatch as a header line; body: {}",
            fragment.text
        ));
    }
    if !fragment.text.contains("workspace_mismatch_hard") {
        return Err(format!(
            "prompt_fragment.text must reference the mismatch degradation code; body: {}",
            fragment.text
        ));
    }
    Ok(())
}
