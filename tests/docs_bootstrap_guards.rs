//! Bootstrap-compiler guard tests (bd-1n0np.11.4) over the landed pub core
//! `ee::core::docs_bootstrap::compile_docs_bootstrap`.
//!
//! The in-module tests cover the allowlist happy path, determinism, structural
//! extraction (commands/tables/anchors without summarizing), and secret
//! redaction. These lock the SAFETY-GUARD half of the acceptance, which the
//! in-module tests do not exercise:
//! - oversize per-source rejection (degraded, not read);
//! - total-byte-limit short-circuit;
//! - symlinked allowlisted source rejection (no symlink traversal);
//! - missing allowlisted sources degrade low, never panic / mutate;
//! - conservative trust assignment (explicit policy is human_explicit;
//!   extracted commands are agent_assertion);
//! - specificity stays within the gated [40, 100] band and anchors are emitted.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use ee::core::docs_bootstrap::{
    BootstrapDocGlob, CompileDocsBootstrapOptions, compile_docs_bootstrap,
};

fn write_file(root: &Path, relative_path: &str, content: &str) {
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent dir");
    }
    fs::write(path, content).expect("write fixture file");
}

#[test]
fn oversize_source_is_rejected_with_degradation_and_not_read() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    write_file(
        tempdir.path(),
        "AGENTS.md",
        "# Agent rules\n\nNever delete files without written permission.\n",
    );
    write_file(tempdir.path(), "README.md", "# Project\n\nUse `ee pack`.\n");

    // A per-source byte cap below AGENTS.md's size rejects it but not the run.
    let mut options = CompileDocsBootstrapOptions::for_workspace(tempdir.path());
    options.max_source_bytes = 16;

    let run = compile_docs_bootstrap(&options);
    assert!(
        run.degraded
            .iter()
            .any(|degradation| degradation.code == "docs_bootstrap_source_oversized"),
        "an oversize source must surface a degradation; got {:?}",
        run.degraded
    );
    assert!(
        run.sources
            .iter()
            .all(|source| source.relative_path != "AGENTS.md"),
        "the oversize source must not be read"
    );
    assert!(!run.durable_mutation);
}

#[test]
fn total_byte_limit_short_circuits_reads() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    write_file(
        tempdir.path(),
        "AGENTS.md",
        "# Agent rules\n\nNever delete files without written permission.\n",
    );
    write_file(
        tempdir.path(),
        "README.md",
        "# Project\n\nUse `ee pack` to assemble context.\n",
    );

    let mut options = CompileDocsBootstrapOptions::for_workspace(tempdir.path());
    options.max_total_bytes = 8; // smaller than the first source

    let run = compile_docs_bootstrap(&options);
    assert!(
        run.degraded
            .iter()
            .any(|degradation| degradation.code == "docs_bootstrap_total_limit_reached"),
        "exceeding the total limit must surface a degradation; got {:?}",
        run.degraded
    );
}

#[cfg(unix)]
#[test]
fn symlinked_allowlisted_source_is_rejected() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    write_file(
        tempdir.path(),
        "AGENTS.md",
        "# Agent rules\n\nNever guess.\n",
    );
    write_file(
        tempdir.path(),
        "real_readme.md",
        "# Real\n\nUse `ee why`.\n",
    );
    // README.md is a symlink to a real file inside the workspace.
    std::os::unix::fs::symlink("real_readme.md", tempdir.path().join("README.md"))
        .expect("create symlink");

    let run = compile_docs_bootstrap(&CompileDocsBootstrapOptions::for_workspace(tempdir.path()));
    assert!(
        run.degraded
            .iter()
            .any(|degradation| degradation.code == "docs_bootstrap_symlink_rejected"),
        "a symlinked allowlisted source must be rejected, never traversed; got {:?}",
        run.degraded
    );
    assert!(
        run.sources
            .iter()
            .all(|source| source.relative_path != "README.md"),
        "the symlinked source must not be read"
    );
}

#[test]
fn missing_allowlisted_sources_degrade_low_without_panicking_or_mutating() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    // Empty workspace: every allowlisted root source is missing.
    let run = compile_docs_bootstrap(&CompileDocsBootstrapOptions::for_workspace(tempdir.path()));

    assert!(run.sources.is_empty(), "no sources to read");
    assert!(run.candidates.is_empty(), "no candidates without sources");
    assert!(!run.durable_mutation, "compile never mutates");
    assert!(
        run.degraded
            .iter()
            .any(|degradation| degradation.code == "docs_bootstrap_source_missing"),
        "missing allowlisted sources must degrade; got {:?}",
        run.degraded
    );
    assert!(
        run.degraded
            .iter()
            .filter(|degradation| degradation.code == "docs_bootstrap_source_missing")
            .all(|degradation| degradation.severity == "low"),
        "a missing source is a low-severity degradation, not a failure"
    );
}

#[test]
fn trust_is_conservative_explicit_policy_human_commands_agent() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    write_file(
        tempdir.path(),
        "AGENTS.md",
        "# Agent rules\n\nNever delete files.\n\n```bash\ncargo check --all-targets\n```\n",
    );
    write_file(tempdir.path(), "README.md", "# Readme\n");

    let run = compile_docs_bootstrap(&CompileDocsBootstrapOptions::for_workspace(tempdir.path()));

    // Every candidate's trust is one of the two conservative classes.
    assert!(
        run.candidates
            .iter()
            .all(|candidate| matches!(candidate.trust_class, "human_explicit" | "agent_assertion")),
        "trust_class must be a conservative two-class value"
    );

    let policy = run
        .candidates
        .iter()
        .find(|candidate| candidate.proposed_content == "Never delete files.")
        .expect("explicit policy candidate present");
    assert_eq!(
        policy.trust_class, "human_explicit",
        "an explicit policy rule from AGENTS.md is human_explicit"
    );

    let command = run
        .candidates
        .iter()
        .find(|candidate| candidate.proposed_content == "cargo check --all-targets")
        .expect("command candidate present");
    assert_eq!(
        command.trust_class, "agent_assertion",
        "an extracted command is the more conservative agent_assertion, not human_explicit"
    );
}

#[test]
fn specificity_is_gated_and_anchors_are_emitted() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    write_file(
        tempdir.path(),
        "AGENTS.md",
        "# Agent rules\n\nNever delete files.\n",
    );
    write_file(tempdir.path(), "docs/env_vars.md", "# Env\n\n`EE_TEST=1`\n");
    write_file(tempdir.path(), "README.md", "# Readme\n");

    let run = compile_docs_bootstrap(&CompileDocsBootstrapOptions::for_workspace(tempdir.path()));
    assert!(!run.candidates.is_empty());
    assert!(
        run.candidates
            .iter()
            .all(|candidate| (40..=100).contains(&candidate.specificity)),
        "specificity must stay within the gated [40, 100] band"
    );
    assert!(
        run.candidates
            .iter()
            .any(|candidate| !candidate.anchors.is_empty()),
        "at least one candidate must emit anchors"
    );
}

#[test]
fn explicit_reference_globs_add_only_selected_docs_with_durable_source_tags() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    write_file(tempdir.path(), "AGENTS.md", "# Agent rules\n");
    write_file(tempdir.path(), "README.md", "# Project\n");
    write_file(tempdir.path(), "SKILL.md", "# Skill guide\n");
    write_file(
        tempdir.path(),
        "references/operator.md",
        "# Operator library\n",
    );
    write_file(
        tempdir.path(),
        "references/deep/failures.md",
        "# Failure taxonomy\n",
    );
    write_file(
        tempdir.path(),
        "references/deep/ignored.txt",
        "# Not selected\n",
    );
    let include_globs = [
        "SKILL.md"
            .parse::<BootstrapDocGlob>()
            .expect("exact include"),
        "references/**/*.md"
            .parse::<BootstrapDocGlob>()
            .expect("recursive include"),
    ];
    let mut options = CompileDocsBootstrapOptions::for_workspace(tempdir.path());
    options.include_globs = &include_globs;

    let run = compile_docs_bootstrap(&options);

    assert_eq!(
        run.sources
            .iter()
            .filter(|source| source.source_kind == "reference_doc")
            .map(|source| source.relative_path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "SKILL.md",
            "references/deep/failures.md",
            "references/operator.md",
        ]
    );
    assert!(run.candidates.iter().any(|candidate| {
        candidate.source_path == "references/deep/failures.md"
            && candidate.trust_class == "agent_assertion"
            && candidate
                .tags
                .iter()
                .any(|tag| tag == "source_kind:reference_doc")
    }));
    assert!(
        run.sources
            .iter()
            .all(|source| source.relative_path != "references/deep/ignored.txt")
    );
}
