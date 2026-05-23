//! Contract coverage for `CassImportGuidance::from_agent_inventory`
//! status mapping and root sort order (bd-2cmcu).
//!
//! Companion to bd-1u0za, which pins
//! `CassImportGuidanceStatus::as_str` per variant. This file pins the
//! two non-obvious transformations inside
//! `src/core/doctor.rs::CassImportGuidance::from_agent_inventory`
//! (line 1494):
//!
//! 1. The mapping from `AgentInventoryStatus` to
//!    `CassImportGuidanceStatus`:
//!
//!    | AgentInventoryStatus | detected roots empty? | Guidance status         |
//!    | -------------------- | --------------------- | ----------------------- |
//!    | Ready                | yes                   | NoAgentRootsDetected    |
//!    | Ready                | no                    | AgentRootsDetected      |
//!    | Empty                | (any)                 | NoAgentRootsDetected    |
//!    | NotInspected         | (any)                 | NotInspected            |
//!    | Unavailable          | (any)                 | Unavailable             |
//!
//! 2. The sort order applied to `roots`: `(connector, root_path)`
//!    ascending. A future agent could flip this to root_path-then-
//!    connector and no test would catch it.
//!
//! Neither contract is exercised at the unit level today.
//! Mirrors bd-1u0za / bd-w3iv0 bounded-contract pin pattern.

use ee::core::agent_detect::{
    AGENT_STATUS_SCHEMA_V1, AgentInventoryReport, AgentInventoryStatus,
    InstalledAgentDetectionEntry, InstalledAgentDetectionSummary,
};
use ee::core::doctor::{CassImportGuidance, CassImportGuidanceStatus};

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
fn not_inspected_inventory_maps_to_not_inspected_guidance() -> TestResult {
    let report = inventory(AgentInventoryStatus::NotInspected, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.status,
        &CassImportGuidanceStatus::NotInspected,
        "NotInspected agent inventory must surface as NotInspected guidance",
    )
}

#[test]
fn unavailable_inventory_maps_to_unavailable_guidance() -> TestResult {
    let report = inventory(AgentInventoryStatus::Unavailable, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.status,
        &CassImportGuidanceStatus::Unavailable,
        "Unavailable agent inventory must surface as Unavailable guidance",
    )
}

#[test]
fn empty_inventory_maps_to_no_agent_roots_detected_guidance() -> TestResult {
    let report = inventory(AgentInventoryStatus::Empty, Vec::new());
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.status,
        &CassImportGuidanceStatus::NoAgentRootsDetected,
        "Empty agent inventory must surface as NoAgentRootsDetected guidance",
    )
}

#[test]
fn ready_inventory_with_no_detected_roots_maps_to_no_agent_roots_detected() -> TestResult {
    // The Ready+empty-roots branch is the non-obvious one: even
    // though the inventory is `Ready`, the guidance must downgrade to
    // NoAgentRootsDetected when no detected agent contributes any
    // root_path. Construct an entry that is `detected=false` (so the
    // flat_map filter drops it) to land on this branch.
    let report = inventory(
        AgentInventoryStatus::Ready,
        vec![agent_entry("codex", false, &["/should/not/appear"])],
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.status,
        &CassImportGuidanceStatus::NoAgentRootsDetected,
        "Ready agent inventory with no detected roots must surface as NoAgentRootsDetected",
    )
}

#[test]
fn ready_inventory_with_detected_roots_maps_to_agent_roots_detected() -> TestResult {
    let report = inventory(
        AgentInventoryStatus::Ready,
        vec![agent_entry("codex", true, &["/home/user/.codex"])],
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.status,
        &CassImportGuidanceStatus::AgentRootsDetected,
        "Ready agent inventory with at least one detected root must surface as AgentRootsDetected",
    )
}

#[test]
fn roots_are_sorted_by_connector_then_root_path() -> TestResult {
    // Provide entries in an order the sort must rearrange so a regression
    // (e.g. sort by root_path then connector, or no sort at all) trips
    // this test. We use distinct connectors and distinct root_paths so
    // both sort keys are visibly relevant.
    let report = inventory(
        AgentInventoryStatus::Ready,
        vec![
            agent_entry(
                "gemini",
                true,
                &["/home/user/.gemini/zeta", "/home/user/.gemini/alpha"],
            ),
            agent_entry(
                "claude",
                true,
                &["/home/user/.claude/beta", "/home/user/.claude/alpha"],
            ),
        ],
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);

    let actual: Vec<(String, String)> = guidance
        .roots
        .iter()
        .map(|root| (root.connector.clone(), root.root_path.clone()))
        .collect();
    let expected: Vec<(String, String)> = vec![
        ("claude".to_string(), "/home/user/.claude/alpha".to_string()),
        ("claude".to_string(), "/home/user/.claude/beta".to_string()),
        ("gemini".to_string(), "/home/user/.gemini/alpha".to_string()),
        ("gemini".to_string(), "/home/user/.gemini/zeta".to_string()),
    ];
    ensure_equal(
        &actual,
        &expected,
        "roots must sort by (connector, root_path) ascending — claude before gemini, \
         and per-connector root_paths must sort lexicographically",
    )
}
