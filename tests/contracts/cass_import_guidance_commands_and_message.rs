//! Contract coverage for `CassImportGuidance` per-status
//! `suggested_commands` and `message` fields (bd-3oaub).
//!
//! Companion to bd-2cmcu (`from_agent_inventory` status mapping +
//! root sort order). This file pins the two remaining per-status
//! transformations in `src/core/doctor.rs::CassImportGuidance::
//! from_agent_inventory` (lines 1527-1565):
//!
//! 1. `suggested_commands: Vec<String>` per status — 4 distinct
//!    command lists that the fix-plan surfaces as next-step actions
//!    for the operator.
//! 2. `message: String` per status — a short human-readable summary
//!    that accompanies the structured fields.
//!
//! Mirrors bd-1u0za / bd-w3iv0 bounded-contract pin pattern: silently
//! rewording any command or message would mislead operators about
//! next steps and slip past existing coverage.

use ee::core::agent_detect::{
    AGENT_STATUS_SCHEMA_V1, AgentInventoryReport, AgentInventoryStatus,
    InstalledAgentDetectionEntry, InstalledAgentDetectionSummary,
};
use ee::core::doctor::CassImportGuidance;

type TestResult = Result<(), String>;

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: &T,
    expected: &T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn inventory(
    status: AgentInventoryStatus,
    installed_agents: Vec<InstalledAgentDetectionEntry>,
) -> AgentInventoryReport {
    let detected_count = installed_agents
        .iter()
        .filter(|agent| agent.detected)
        .count();
    AgentInventoryReport {
        schema: AGENT_STATUS_SCHEMA_V1,
        status,
        format_version: 1,
        summary: InstalledAgentDetectionSummary {
            detected_count,
            total_count: installed_agents.len().max(detected_count),
        },
        installed_agents,
        degraded: Vec::new(),
        inspection_command: "ee agent status --json",
    }
}

fn agent_entry(slug: &str, detected: bool, root_paths: &[&str]) -> InstalledAgentDetectionEntry {
    InstalledAgentDetectionEntry {
        slug: slug.to_string(),
        detected,
        evidence: Vec::new(),
        root_paths: root_paths.iter().map(|s| (*s).to_string()).collect(),
    }
}

#[test]
fn agent_roots_detected_suggests_three_commands() -> TestResult {
    let report = inventory(
        AgentInventoryStatus::Ready,
        vec![agent_entry("claude_code", true, &["/tmp/.claude"])],
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected: Vec<String> = vec![
        "ee agent status --json".to_string(),
        "ee import cass --dry-run --json".to_string(),
        "ee import cass --json".to_string(),
    ];
    ensure_equal(
        &guidance.suggested_commands,
        &expected,
        "AgentRootsDetected -> status/dry-run/import command triplet",
    )
}

#[test]
fn no_agent_roots_detected_suggests_scan_then_dry_run() -> TestResult {
    let report = inventory(AgentInventoryStatus::Ready, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected: Vec<String> = vec![
        "ee agent scan --existing-only --json".to_string(),
        "ee import cass --dry-run --json".to_string(),
    ];
    ensure_equal(
        &guidance.suggested_commands,
        &expected,
        "NoAgentRootsDetected (Ready + zero roots) -> scan-then-dry-run",
    )
}

#[test]
fn not_inspected_suggests_status_then_scan_then_dry_run() -> TestResult {
    let report = inventory(AgentInventoryStatus::NotInspected, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected: Vec<String> = vec![
        "ee agent status --json".to_string(),
        "ee agent scan --existing-only --json".to_string(),
        "ee import cass --dry-run --json".to_string(),
    ];
    ensure_equal(
        &guidance.suggested_commands,
        &expected,
        "NotInspected -> status/scan/dry-run triple",
    )
}

#[test]
fn unavailable_suggests_sources_catalog_then_dry_run() -> TestResult {
    let report = inventory(AgentInventoryStatus::Unavailable, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected: Vec<String> = vec![
        "ee agent sources --json".to_string(),
        "ee import cass --dry-run --json".to_string(),
    ];
    ensure_equal(
        &guidance.suggested_commands,
        &expected,
        "Unavailable -> static-source-catalog + dry-run fallback",
    )
}

#[test]
fn agent_roots_detected_message_includes_root_count() -> TestResult {
    let report = inventory(
        AgentInventoryStatus::Ready,
        vec![
            agent_entry("claude_code", true, &["/tmp/.claude"]),
            agent_entry("codex", true, &["/tmp/.codex/a", "/tmp/.codex/b"]),
        ],
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected =
        "Detected 3 local agent source root(s); run a CASS dry-run before importing evidence.";
    ensure_equal(
        &guidance.message,
        &expected.to_string(),
        "AgentRootsDetected message interpolates the detected_root_count",
    )?;
    ensure_equal(
        &guidance.detected_root_count,
        &3_usize,
        "detected_root_count == sum of detected agents' root_paths",
    )
}

#[test]
fn no_agent_roots_detected_message_is_pinned() -> TestResult {
    let report = inventory(AgentInventoryStatus::Empty, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected = "No local agent source roots were detected; CASS import can still report available sessions.";
    ensure_equal(
        &guidance.message,
        &expected.to_string(),
        "NoAgentRootsDetected (Empty) message",
    )
}

#[test]
fn not_inspected_message_is_pinned() -> TestResult {
    let report = inventory(AgentInventoryStatus::NotInspected, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected = "Agent source roots were not inspected for this fix plan; run agent status for root-level guidance.";
    ensure_equal(
        &guidance.message,
        &expected.to_string(),
        "NotInspected message",
    )
}

#[test]
fn unavailable_message_is_pinned() -> TestResult {
    let report = inventory(AgentInventoryStatus::Unavailable, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    let expected = "Agent source root detection is unavailable; use the static source catalog and CASS dry-run output.";
    ensure_equal(
        &guidance.message,
        &expected.to_string(),
        "Unavailable message",
    )
}
