//! M2 contract test (eidetic_engine_cli bd-17c65.13.3).
//!
//! Asserts the four-level workspace fingerprint reconciliation
//! introduced in M2:
//!
//! - `compute_workspace_identity_for_capsule` is deterministic: the
//!   same canonical workspace path produces the same fingerprint
//!   across 100 invocations.
//! - Distinct workspaces produce distinct fingerprints.
//! - `compare_workspace_identity` returns:
//!     * `Exact` when fingerprints match,
//!     * `SoftSamePath` when paths match but fingerprints differ
//!       (synthetic — covers the corner case where the producer
//!       hashed via a different canonicalization),
//!     * `SoftSameRepo` when fingerprints differ but
//!       repository_fingerprint matches,
//!     * `Hard` otherwise.
//! - The producer side embeds the structured identity into the
//!   capsule's `workspace_identity` block at create-time.
//! - `resume_handoff` with a bound identity populates
//!   `workspace_match` and derives `workspace_mismatch` from it
//!   consistently.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;

use ee::core::handoff::{
    CapsuleProfile, CreateOptions, ResumeOptions, WorkspaceIdentity, WorkspaceMatch,
    WorkspaceMismatchSeverity, compare_workspace_identity, compute_workspace_identity_for_capsule,
    create_handoff, resume_handoff,
};
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

#[test]
fn workspace_identity_fingerprint_is_deterministic() -> TestResult {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..100 {
        let id = compute_workspace_identity_for_capsule(dir.path());
        seen.insert(id.fingerprint.clone());
    }
    if seen.len() != 1 {
        return Err(format!(
            "fingerprint should be byte-stable across invocations; saw {} distinct values: {seen:?}",
            seen.len()
        ));
    }
    let single = seen.into_iter().next().unwrap();
    if single.len() != 24 {
        return Err(format!(
            "fingerprint should be 24 hex chars, got {} ({single})",
            single.len()
        ));
    }
    Ok(())
}

#[test]
fn workspace_identity_distinguishes_different_paths() -> TestResult {
    let dir_a = tempfile::tempdir().map_err(|error| format!("tempdir a: {error}"))?;
    let dir_b = tempfile::tempdir().map_err(|error| format!("tempdir b: {error}"))?;
    let id_a = compute_workspace_identity_for_capsule(dir_a.path());
    let id_b = compute_workspace_identity_for_capsule(dir_b.path());
    if id_a.fingerprint == id_b.fingerprint {
        return Err(format!(
            "different workspaces should not collide on fingerprint: {} vs {}",
            id_a.fingerprint, id_b.fingerprint
        ));
    }
    Ok(())
}

#[test]
fn compare_workspace_identity_returns_exact_for_same_fingerprint() -> TestResult {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let a = compute_workspace_identity_for_capsule(dir.path());
    let b = a.clone();
    let result = compare_workspace_identity(&a, &b);
    if !matches!(result, WorkspaceMatch::Exact) {
        return Err(format!("expected Exact, got {result:?}"));
    }
    if !matches!(
        result.to_mismatch_severity(),
        WorkspaceMismatchSeverity::None
    ) {
        return Err(format!(
            "Exact should map to mismatch severity None, got {:?}",
            result.to_mismatch_severity()
        ));
    }
    Ok(())
}

#[test]
fn compare_workspace_identity_returns_soft_same_path_when_fingerprint_diverges_but_root_matches()
-> TestResult {
    // Synthetic — same canonical_root, different fingerprint. Captures
    // the corner case where the producer hashed via a different
    // canonicalization scheme but the resolved path string still
    // matches.
    let id_a = WorkspaceIdentity {
        fingerprint: "abc000000000000000000000".to_owned(),
        canonical_root: "/tmp/shared".to_owned(),
        scope_kind: "standalone".to_owned(),
        repository_fingerprint: None,
    };
    let id_b = WorkspaceIdentity {
        fingerprint: "def000000000000000000000".to_owned(),
        canonical_root: "/tmp/shared".to_owned(),
        scope_kind: "standalone".to_owned(),
        repository_fingerprint: None,
    };
    let result = compare_workspace_identity(&id_a, &id_b);
    if !matches!(result, WorkspaceMatch::SoftSamePath) {
        return Err(format!("expected SoftSamePath, got {result:?}"));
    }
    if !matches!(
        result.to_mismatch_severity(),
        WorkspaceMismatchSeverity::Soft
    ) {
        return Err(format!(
            "SoftSamePath should map to mismatch severity Soft, got {:?}",
            result.to_mismatch_severity()
        ));
    }
    Ok(())
}

#[test]
fn compare_workspace_identity_returns_soft_same_repo_for_shared_repository_fingerprint()
-> TestResult {
    let id_a = WorkspaceIdentity {
        fingerprint: "abc000000000000000000000".to_owned(),
        canonical_root: "/tmp/repo/sub-a".to_owned(),
        scope_kind: "repository_subdir".to_owned(),
        repository_fingerprint: Some("repo:deadbeef".to_owned()),
    };
    let id_b = WorkspaceIdentity {
        fingerprint: "def000000000000000000000".to_owned(),
        canonical_root: "/tmp/repo/sub-b".to_owned(),
        scope_kind: "repository_subdir".to_owned(),
        repository_fingerprint: Some("repo:deadbeef".to_owned()),
    };
    let result = compare_workspace_identity(&id_a, &id_b);
    if !matches!(result, WorkspaceMatch::SoftSameRepo) {
        return Err(format!("expected SoftSameRepo, got {result:?}"));
    }
    if !matches!(
        result.to_mismatch_severity(),
        WorkspaceMismatchSeverity::Soft
    ) {
        return Err(format!(
            "SoftSameRepo should map to mismatch severity Soft, got {:?}",
            result.to_mismatch_severity()
        ));
    }
    Ok(())
}

#[test]
fn compare_workspace_identity_returns_hard_when_nothing_matches() -> TestResult {
    let id_a = WorkspaceIdentity {
        fingerprint: "abc000000000000000000000".to_owned(),
        canonical_root: "/tmp/one".to_owned(),
        scope_kind: "standalone".to_owned(),
        repository_fingerprint: None,
    };
    let id_b = WorkspaceIdentity {
        fingerprint: "def000000000000000000000".to_owned(),
        canonical_root: "/tmp/two".to_owned(),
        scope_kind: "standalone".to_owned(),
        repository_fingerprint: None,
    };
    let result = compare_workspace_identity(&id_a, &id_b);
    if !matches!(result, WorkspaceMatch::Hard) {
        return Err(format!("expected Hard, got {result:?}"));
    }
    if !matches!(
        result.to_mismatch_severity(),
        WorkspaceMismatchSeverity::Hard
    ) {
        return Err(format!(
            "Hard should map to mismatch severity Hard, got {:?}",
            result.to_mismatch_severity()
        ));
    }
    Ok(())
}

fn build_capsule_with_identity() -> Result<(TempDir, PathBuf, PathBuf), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn =
        ee::db::DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

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

    Ok((dir, workspace, capsule_path))
}

#[test]
fn capsule_embeds_structured_workspace_identity_at_create_time() -> TestResult {
    let (_dir, workspace, capsule_path) = build_capsule_with_identity()?;
    let body =
        std::fs::read_to_string(&capsule_path).map_err(|error| format!("read capsule: {error}"))?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|error| format!("parse capsule: {error}"))?;
    let identity_node = parsed
        .get("workspace_identity")
        .ok_or_else(|| "capsule must embed workspace_identity block".to_string())?;
    let identity: WorkspaceIdentity = serde_json::from_value(identity_node.clone())
        .map_err(|error| format!("workspace_identity not parseable as struct: {error}"))?;
    if identity.fingerprint.len() != 24 {
        return Err(format!(
            "embedded fingerprint should be 24 hex chars, got {}",
            identity.fingerprint.len()
        ));
    }
    // The producer-side helper run on the same workspace should match
    // what got embedded. Belt-and-braces against silent re-hashing.
    let recomputed = compute_workspace_identity_for_capsule(&workspace);
    if identity.fingerprint != recomputed.fingerprint {
        return Err(format!(
            "embedded fingerprint {} differs from recomputed {}",
            identity.fingerprint, recomputed.fingerprint
        ));
    }
    Ok(())
}

#[test]
fn resume_populates_workspace_match_when_structured_identity_is_bound() -> TestResult {
    let (_dir, workspace, capsule_path) = build_capsule_with_identity()?;

    // Bind to the same workspace — expect Exact.
    let bound_same = compute_workspace_identity_for_capsule(&workspace);
    let report_same = resume_handoff(&ResumeOptions {
        path: capsule_path.clone(),
        use_latest: false,
        workspace: workspace.clone(),
        max_sections: None,
        task_frame_id: None,
        bound_workspace_id: None,
        bound_workspace_identity: Some(bound_same),
        include_prompt_fragment: false,
        require_fresh: false,
        insecure_skip_hmac: false,
        machine_salt_path: None,
    })
    .map_err(|error| format!("resume_handoff(exact): {error:?}"))?;
    if !matches!(report_same.workspace_match, Some(WorkspaceMatch::Exact)) {
        return Err(format!(
            "expected Exact match against own workspace, got {:?}",
            report_same.workspace_match
        ));
    }
    if !matches!(
        report_same.workspace_mismatch,
        WorkspaceMismatchSeverity::None
    ) {
        return Err(format!(
            "Exact must derive mismatch severity None, got {:?}",
            report_same.workspace_mismatch
        ));
    }

    // Bind to a totally unrelated workspace — expect Hard.
    let other_dir = tempfile::tempdir().map_err(|error| format!("tempdir b: {error}"))?;
    let bound_other = compute_workspace_identity_for_capsule(other_dir.path());
    let report_hard = resume_handoff(&ResumeOptions {
        path: capsule_path.clone(),
        use_latest: false,
        workspace: workspace.clone(),
        max_sections: None,
        task_frame_id: None,
        bound_workspace_id: None,
        bound_workspace_identity: Some(bound_other),
        include_prompt_fragment: true,
        require_fresh: false,
        insecure_skip_hmac: false,
        machine_salt_path: None,
    })
    .map_err(|error| format!("resume_handoff(hard): {error:?}"))?;
    if !matches!(report_hard.workspace_match, Some(WorkspaceMatch::Hard)) {
        return Err(format!(
            "expected Hard match against unrelated workspace, got {:?}",
            report_hard.workspace_match
        ));
    }
    if !matches!(
        report_hard.workspace_mismatch,
        WorkspaceMismatchSeverity::Hard
    ) {
        return Err(format!(
            "Hard must derive mismatch severity Hard, got {:?}",
            report_hard.workspace_mismatch
        ));
    }
    // Hard match must surface a degradation with the new code.
    let hits = report_hard
        .degradations
        .iter()
        .filter(|d| d.code == "workspace_mismatch_hard")
        .count();
    if hits == 0 {
        return Err(format!(
            "Hard match must emit workspace_mismatch_hard degradation; got codes={:?}",
            report_hard
                .degradations
                .iter()
                .map(|d| &d.code)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}
