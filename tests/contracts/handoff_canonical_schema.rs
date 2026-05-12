//! M0 contract test (eidetic_engine_cli bd-17c65.13.1).
//!
//! Audits `ee.handoff.capsule.v1` against the D1 canonical field
//! contract. The handoff capsule was implemented before D1; this test
//! pins the invariants D1 asks for so any future drift fails CI:
//!
//! - Section bodies use the canonical field name `content`. The
//!   inspect side reads them under `content` too. There must be no
//!   `content_preview` field anywhere in the capsule.
//! - The top-level schema is `ee.handoff.capsule.v1` (frozen for
//!   v1; M1's resume-side work will produce v2).
//! - The capsule_id is non-empty and the workspace is recorded.
//! - Every section carries the four canonical fields (id, title,
//!   content, token_estimate) without legacy aliases.
//!
//! These are honest assertions: when the producer side adds new
//! sections the test still passes; when a future commit renames a
//! field (e.g. content -> content_preview), it fails loudly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::core::handoff::{CapsuleProfile, CreateOptions, HANDOFF_CAPSULE_SCHEMA_V1, create_handoff};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn build_workspace() -> Result<(TempDir, PathBuf), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    // Seed enough variety that the capsule has real sections to
    // populate (objective, decisions, next_actions).
    for (content, level, kind, tags) in [
        (
            "Cut release v0.2.0 after migrations land.",
            "procedural",
            "rule",
            Some("release"),
        ),
        (
            "Decided to use BLAKE3 for memory dedupe hashing.",
            "semantic",
            "decision",
            Some("blake3,decision"),
        ),
        (
            "Run `cargo fmt --check` before tagging the release.",
            "procedural",
            "rule",
            Some("formatting,release"),
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
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
        })
        .map_err(|error| format!("remember `{content}`: {error:?}"))?;
    }
    Ok((dir, workspace))
}

fn produce_capsule(workspace: &std::path::Path) -> Result<Value, String> {
    let out_dir = workspace.join("out");
    std::fs::create_dir_all(&out_dir).map_err(|error| format!("mkdir out: {error}"))?;
    let capsule_path = out_dir.join("capsule.json");
    create_handoff(&CreateOptions {
        workspace: workspace.to_path_buf(),
        output: capsule_path.clone(),
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
    })
    .map_err(|error| format!("create_handoff: {error:?}"))?;

    let body =
        std::fs::read_to_string(&capsule_path).map_err(|error| format!("read capsule: {error}"))?;
    let parsed: Value = serde_json::from_str(&body)
        .map_err(|error| format!("parse capsule: {error}\nbody: {body}"))?;
    Ok(parsed)
}

/// Walk every JSON node and collect keys that violate D1: anything named
/// `content_preview`. Returns a list of dotted paths for reporting.
fn find_forbidden_field(value: &Value, prefix: &str, forbidden: &str) -> Vec<String> {
    let mut hits = Vec::new();
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if key == forbidden {
                    hits.push(path.clone());
                }
                hits.extend(find_forbidden_field(child, &path, forbidden));
            }
        }
        Value::Array(arr) => {
            for (idx, child) in arr.iter().enumerate() {
                let path = format!("{prefix}[{idx}]");
                hits.extend(find_forbidden_field(child, &path, forbidden));
            }
        }
        _ => {}
    }
    hits
}

#[test]
fn capsule_top_level_schema_is_v1_with_canonical_envelope_keys() -> TestResult {
    let (_dir, workspace) = build_workspace()?;
    let capsule = produce_capsule(&workspace)?;
    let object = capsule
        .as_object()
        .ok_or_else(|| "capsule must be a JSON object".to_string())?;

    let schema = object
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing top-level schema".to_string())?;
    if schema != HANDOFF_CAPSULE_SCHEMA_V1 {
        return Err(format!(
            "top-level schema must be {HANDOFF_CAPSULE_SCHEMA_V1}, got {schema}"
        ));
    }

    for required in ["capsule_id", "sections", "created_at"] {
        if !object.contains_key(required) {
            return Err(format!(
                "capsule envelope missing required key `{required}`"
            ));
        }
    }

    let capsule_id = object
        .get("capsule_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "capsule_id missing".to_string())?;
    if capsule_id.is_empty() {
        return Err("capsule_id must be non-empty".to_string());
    }
    Ok(())
}

#[test]
fn capsule_sections_use_canonical_content_field_no_content_preview() -> TestResult {
    let (_dir, workspace) = build_workspace()?;
    let capsule = produce_capsule(&workspace)?;

    let sections = capsule
        .get("sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "sections[] missing".to_string())?;
    if sections.is_empty() {
        return Err("capsule has no sections — fixture should seed at least one".to_string());
    }
    for (i, section) in sections.iter().enumerate() {
        let obj = section
            .as_object()
            .ok_or_else(|| format!("section[{i}] is not an object"))?;
        for required in ["id", "title", "content", "token_estimate"] {
            if !obj.contains_key(required) {
                return Err(format!(
                    "section[{i}] missing canonical field `{required}` (D1 contract)"
                ));
            }
        }
        if obj.contains_key("content_preview") {
            return Err(format!(
                "section[{i}] uses legacy `content_preview` instead of canonical `content` (D1 violation)"
            ));
        }
    }
    Ok(())
}

#[test]
fn capsule_carries_no_content_preview_anywhere_in_envelope() -> TestResult {
    // Belt-and-braces walk: scan the entire capsule for any `content_preview`
    // key, anywhere — sections, task_frame, focus, swarm brief, future
    // additions. D1's invariant is that the canonical field is `content`
    // everywhere; the legacy name must never appear.
    let (_dir, workspace) = build_workspace()?;
    let capsule = produce_capsule(&workspace)?;
    let leaks = find_forbidden_field(&capsule, "", "content_preview");
    if !leaks.is_empty() {
        return Err(format!(
            "capsule leaks legacy `content_preview` at: {leaks:?} (D1 violation)"
        ));
    }
    Ok(())
}

#[test]
fn capsule_sections_id_set_covers_canonical_resume_topics() -> TestResult {
    // Resume profile must produce at least the canonical resume topics:
    // workspace identity, current objective, next actions, recent
    // decisions. Section IDs are stable strings used by the resume
    // surface to extract structured data — pinning them here documents
    // the contract and catches accidental renames.
    let (_dir, workspace) = build_workspace()?;
    let capsule = produce_capsule(&workspace)?;
    let sections = capsule
        .get("sections")
        .and_then(Value::as_array)
        .ok_or_else(|| "sections[] missing".to_string())?;
    let ids: Vec<String> = sections
        .iter()
        .filter_map(|s| s.get("id").and_then(Value::as_str).map(String::from))
        .collect();

    // Per src/core/handoff.rs create_handoff: workspace, objective,
    // next_actions, decisions are always synthesized (decisions even
    // emits a placeholder when none are recorded).
    for canonical_id in ["workspace", "objective", "next_actions", "decisions"] {
        if !ids.iter().any(|id| id == canonical_id) {
            return Err(format!(
                "resume capsule missing canonical section `{canonical_id}`; got ids={ids:?}"
            ));
        }
    }
    Ok(())
}
