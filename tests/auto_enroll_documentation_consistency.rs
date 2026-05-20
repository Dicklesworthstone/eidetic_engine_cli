//! Documentation consistency gate for SRR6.46 auto-enrollment.
//!
//! Bead: bd-36bbk.1.18. The implementation beads own the executable mesh
//! surfaces; this test pins the written contract so the ADR, onboarding guide,
//! migration guide, README index, and source/schema references drift together
//! instead of silently diverging.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ee::config::env_registry::EnvVar;
use ee::db::audit_actions;

type TestResult = Result<(), String>;

const ADR_PATH: &str = "docs/adr/0038-auto-enrollment-zero-touch.md";
const ONBOARDING_PATH: &str = "docs/agent-ux/auto_enrollment_onboarding.md";
const MIGRATION_GUIDE_PATH: &str = "docs/migration-guide.md";
const README_PATH: &str = "README.md";

const LOAD_BEARING_DECISIONS: &[&str] = &[
    "Full automation, not a wizard",
    "Consent via forensic audit row, not prompt",
    "Multi-workspace = own peer-group row per workspace",
    "Hello responder lives inside `ee daemon`, not a new `ee mesh serve`",
    "Identity guard covers tailnet AND node-key",
    "Discovery cache (TTL) + per-peer state machine (grace period)",
    "`ee.repair_action_graph.v1` shared schema across doctor and status",
    "Conservative default lane policy",
    "Pre-grant lane visibility preview",
];

const REQUIRED_REJECTIONS: &[&str] = &[
    "Wizard UI",
    "Always-on auto-reconciliation by default",
    "Hello responder as a separate `ee mesh serve` command",
    "Single peer-group shared across workspaces",
    "Body lane default-allow on auto-enrollment",
    "Wall-clock cache TTL only (no per-peer state machine)",
];

const ONBOARDING_SECTIONS: &[&str] = &[
    "## Agent Use/No-Use Checklist",
    "## TL;DR",
    "## Required Preconditions",
    "## Response Envelope Contract",
    "## Per-Command Cheat Sheet",
    "## The Status Surface",
    "## Auto-Enroll Flow",
    "### Common Degraded Codes",
    "## Safety Patterns",
    "## Common Workflows",
    "## What Mesh Auto-Enrollment Does NOT Do",
];

const REQUIRED_SCHEMA_FILES: &[&str] = &[
    "docs/schemas/ee.tailscale.local.v1.json",
    "docs/schemas/ee.mesh.auto_enrollment_summary.v1.json",
    "docs/schemas/ee.mesh.discovery_policy.v1.json",
    "docs/schemas/ee.mesh.hello.v1.json",
    "docs/schemas/ee.mesh.hello.response.v1.json",
    "docs/schemas/ee.mesh.hello.error.v1.json",
    "docs/schemas/ee.mesh.lane_grant_preview.v1.json",
    "docs/schemas/ee.repair_action_graph.v1.json",
];

const ONBOARDING_DEGRADED_CODE_PREFIXES: &[&str] = &[
    "tailscale_",
    "hello_responder_",
    "discovery_policy_",
    "mesh_peer_",
];

const REQUIRED_ONBOARDING_DEGRADED_CODES: &[&str] = &[
    "auto_enrollment_already_complete",
    "auto_enrollment_audit_failed",
    "auto_enrollment_blocked_by_policy",
    "auto_enrollment_concurrent_attempt",
    "auto_enrollment_invalid_override_node_key",
    "auto_enrollment_manual_config_present",
    "auto_enrollment_manual_migration_unmatched_peer_set",
    "auto_enrollment_no_eligible_peers",
    "auto_enrollment_node_key_changed",
    "auto_enrollment_partial_failure",
    "auto_enrollment_sync_once_failed",
    "auto_enrollment_tailnet_changed",
    "lane_grant_preview_peer_not_in_group",
    "mesh_disable_concurrent_attempt",
    "mesh_disable_noop",
    "mesh_revoke_unknown_peer",
    "steward_auto_enroll_disabled",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_file(path: &str) -> PathBuf {
    repo_root().join(path)
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(repo_file(path)).map_err(|error| format!("read {path}: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{context} missing required text `{needle}`"),
    )
}

fn required_srr46_env_vars() -> BTreeSet<&'static str> {
    EnvVar::all()
        .iter()
        .map(|var| var.name())
        .filter(|name| name.starts_with("EE_MESH_") || name.starts_with("EE_TAILSCALE_"))
        .collect()
}

fn taxonomy_codes_with_prefixes(prefixes: &[&str]) -> Result<BTreeSet<String>, String> {
    let taxonomy = read("docs/degraded_code_taxonomy.md")?;
    let mut codes = BTreeSet::new();
    for line in taxonomy.lines() {
        let Some(start) = line.find('`') else {
            continue;
        };
        let Some(end) = line[start + 1..].find('`') else {
            continue;
        };
        let code = &line[start + 1..start + 1 + end];
        if prefixes.iter().any(|prefix| code.starts_with(prefix)) {
            codes.insert(code.to_owned());
        }
    }
    Ok(codes)
}

#[test]
fn adr_0038_contains_all_load_bearing_decisions_named_in_bd_36bbk_1_description() -> TestResult {
    let adr = read(ADR_PATH)?;
    for section in [
        "Status:",
        "Date:",
        "## Context",
        "## Decision",
        "## Invariants",
        "## Rejected Alternatives",
        "## Verification",
    ] {
        ensure_contains(&adr, section, ADR_PATH)?;
    }
    for decision in LOAD_BEARING_DECISIONS {
        ensure_contains(&adr, decision, ADR_PATH)?;
    }
    Ok(())
}

#[test]
fn adr_0038_rejected_alternatives_section_lists_required_explicit_rejections() -> TestResult {
    let adr = read(ADR_PATH)?;
    let rejected = adr
        .split("## Rejected Alternatives")
        .nth(1)
        .ok_or_else(|| format!("{ADR_PATH} missing rejected alternatives section"))?;
    for alternative in REQUIRED_REJECTIONS {
        ensure_contains(rejected, alternative, "ADR 0038 rejected alternatives")?;
    }
    Ok(())
}

#[test]
fn agent_onboarding_doc_cross_references_existing_source_and_schema_files() -> TestResult {
    let doc = read(ONBOARDING_PATH)?;
    for section in ONBOARDING_SECTIONS {
        ensure_contains(&doc, section, ONBOARDING_PATH)?;
    }

    let mut source_refs = BTreeSet::new();
    for token in doc.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '"' | '\''
            )
    }) {
        if let Some(rest) = token.strip_prefix("src/") {
            if let Some(path) = rest.split("::").next().filter(|path| path.ends_with(".rs")) {
                source_refs.insert(format!("src/{path}"));
            }
        }
    }
    ensure(
        !source_refs.is_empty(),
        format!("{ONBOARDING_PATH} must reference at least one source file"),
    )?;
    for source_ref in source_refs {
        ensure(
            repo_file(&source_ref).is_file(),
            format!("{ONBOARDING_PATH} references nonexistent source file `{source_ref}`"),
        )?;
    }

    for schema in REQUIRED_SCHEMA_FILES {
        ensure(
            repo_file(schema).is_file(),
            format!("required schema file missing: {schema}"),
        )?;
        let schema_name = Path::new(schema)
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("schema path has no file stem: {schema}"))?;
        ensure_contains(&doc, schema_name, ONBOARDING_PATH)?;
    }
    Ok(())
}

#[test]
fn agent_onboarding_doc_degraded_codes_table_lists_registered_mesh_codes() -> TestResult {
    let doc = read(ONBOARDING_PATH)?;
    let taxonomy_codes = taxonomy_codes_with_prefixes(ONBOARDING_DEGRADED_CODE_PREFIXES)?;
    ensure(
        !taxonomy_codes.is_empty(),
        "taxonomy did not expose SRR6.46 degraded codes",
    )?;
    for code in taxonomy_codes {
        ensure_contains(&doc, &format!("`{code}`"), ONBOARDING_PATH)?;
    }
    for code in REQUIRED_ONBOARDING_DEGRADED_CODES {
        ensure_contains(&doc, &format!("`{code}`"), ONBOARDING_PATH)?;
    }
    Ok(())
}

#[test]
fn migration_guide_lists_every_ee_mesh_env_var_registered_in_env_registry() -> TestResult {
    let guide = read(MIGRATION_GUIDE_PATH)?;
    for env_var in required_srr46_env_vars() {
        ensure_contains(&guide, &format!("`{env_var}`"), MIGRATION_GUIDE_PATH)?;
    }
    Ok(())
}

#[test]
fn migration_guide_lists_every_ee_mesh_audit_event_type_in_db_allowlist() -> TestResult {
    let guide = read(MIGRATION_GUIDE_PATH)?;
    for event_type in [
        audit_actions::MESH_AUTO_ENROLLMENT_INTENDED,
        audit_actions::MESH_AUTO_ENROLLMENT_OUTCOME_RECORDED,
        audit_actions::MESH_HELLO_RESPONDER_STARTED,
        audit_actions::MESH_HELLO_RESPONDER_STOPPED,
        audit_actions::MESH_HELLO_RESPONDER_CRASHED_RESTARTED,
    ] {
        ensure_contains(&guide, &format!("`{event_type}`"), MIGRATION_GUIDE_PATH)?;
    }
    Ok(())
}

#[test]
fn readme_indexes_auto_enrollment_docs() -> TestResult {
    let readme = read(README_PATH)?;
    for path in [ADR_PATH, ONBOARDING_PATH, MIGRATION_GUIDE_PATH] {
        ensure_contains(&readme, path, README_PATH)?;
    }
    Ok(())
}
