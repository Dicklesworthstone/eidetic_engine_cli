//! Contract coverage for `CassImportGuidance::detected_agent_count`
//! semantics (bd-gc787).
//!
//! `CassImportGuidance::from_agent_inventory` reads
//! `detected_agent_count` from `agent_inventory.summary.detected_count`
//! directly, NOT from filtering `installed_agents.iter()`. Today peer
//! bd-2cmcu pins the status routing and my bd-3oaub pins
//! detected_root_count, but `detected_agent_count` semantics are
//! unpinned. The contract is meaningful: `summary.detected_count` is
//! the authoritative agent count and could legitimately differ from
//! `installed_agents.iter().filter(|a| a.detected).count()` in edge
//! cases (e.g., degraded inventories where the summary reports a
//! count higher than what the materialized entries reflect).

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

fn inventory_with_summary(
    status: AgentInventoryStatus,
    installed_agents: Vec<InstalledAgentDetectionEntry>,
    summary_detected_count: usize,
    summary_total_count: usize,
) -> AgentInventoryReport {
    AgentInventoryReport {
        schema: AGENT_STATUS_SCHEMA_V1,
        status,
        format_version: 1,
        summary: InstalledAgentDetectionSummary {
            detected_count: summary_detected_count,
            total_count: summary_total_count,
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
fn detected_agent_count_comes_from_summary_in_simple_case() -> TestResult {
    let report = inventory_with_summary(
        AgentInventoryStatus::Ready,
        vec![
            agent_entry("claude_code", true, &["/tmp/.claude"]),
            agent_entry("codex", true, &["/tmp/.codex"]),
        ],
        2,
        2,
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.detected_agent_count,
        &2_usize,
        "detected_agent_count mirrors summary.detected_count",
    )
}

#[test]
fn detected_agent_count_uses_summary_not_installed_filter() -> TestResult {
    // Pin the authoritative-source contract: detected_agent_count is
    // sourced from summary.detected_count directly, not from a
    // re-derivation over installed_agents. Construct a deliberately
    // inconsistent fixture (summary says 5 detected, installed_agents
    // only carries 1 detected entry) — the guidance must surface 5.
    // A future agent who switches the field assignment to
    // `installed_agents.iter().filter(|a| a.detected).count()` would
    // surface 1 instead, breaking this test.
    let report = inventory_with_summary(
        AgentInventoryStatus::Ready,
        vec![agent_entry("claude_code", true, &["/tmp/.claude"])],
        5,
        7,
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.detected_agent_count,
        &5_usize,
        "detected_agent_count must reflect summary.detected_count even when it disagrees with installed_agents",
    )
}

#[test]
fn detected_agent_count_zero_when_summary_reports_zero() -> TestResult {
    let report = inventory_with_summary(AgentInventoryStatus::Empty, Vec::new(), 0, 0);
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.detected_agent_count,
        &0_usize,
        "summary.detected_count = 0 -> detected_agent_count = 0",
    )
}

#[test]
fn detected_agent_count_independent_of_detected_root_count() -> TestResult {
    // detected_agent_count comes from summary, detected_root_count comes
    // from counting root_paths across detected entries. Pin that they
    // are distinct quantities sourced from different paths — a future
    // refactor that collapses them into a single derivation would
    // break this test.
    let report = inventory_with_summary(
        AgentInventoryStatus::Ready,
        vec![
            // One detected agent with three root paths -> detected_root_count = 3.
            agent_entry("claude_code", true, &["/a", "/b", "/c"]),
        ],
        1,
        1,
    );
    let guidance = CassImportGuidance::from_agent_inventory(&report);
    ensure_equal(
        &guidance.detected_agent_count,
        &1_usize,
        "1 agent -> detected_agent_count = 1",
    )?;
    ensure_equal(
        &guidance.detected_root_count,
        &3_usize,
        "3 root paths -> detected_root_count = 3 (independent of agent count)",
    )
}
