//! Health command handler (EE-026).
//!
//! Provides a quick health check summary with an overall healthy/unhealthy
//! verdict and counts of issues by severity. Simpler than doctor (no fix plans),
//! more binary than status (which reports detailed readiness states).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use fnx_classes::Graph;
use fnx_runtime::CompatibilityMode;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::db::{
    DbConnection, GraphSnapshotType, MemoryLinkRelation, StoredDebtSnapshot, StoredMemory,
    StoredMemoryLink,
};
use crate::graph::health::{
    ContradictionCluster, ContradictionSeverity, HEALTH_STRUCTURAL_SCHEMA_V1, KTrussReport,
    compute_k_truss, detect_contradiction_clusters,
};
use crate::models::degradation::{GRAPH_HEALTH_NO_CONTRADICTIONS_CODE, GRAPH_METRICS_UNAVAILABLE};
use crate::models::{CapabilityStatus, DomainError};

use super::build_info;
use super::curate::stable_workspace_id;
use super::learn::{LearnGapsOptions, show_gaps};
use super::memory_debt::{
    MemoryDebtDoctorOptions, MemoryDebtReport, MemoryDebtSnapshotOptions, MemoryDebtSnapshotReport,
    run_memory_debt_doctor, run_memory_debt_snapshot,
};
use super::status::{
    default_workspace_path, probe_runtime_capability, probe_search_capability,
    probe_storage_capability,
};

pub const HEALTH_SCORECARD_SCHEMA_V1: &str = "ee.health_scorecard.v1";

const HEALTH_SCORECARD_TREND_LIMIT: u32 = 12;
const HEALTH_SCORECARD_LEARN_GAP_LIMIT: u32 = 50;
const SCORE_EPSILON: f64 = 0.000_001;

/// Overall health verdict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthVerdict {
    Healthy,
    Degraded,
    Unhealthy,
}

impl HealthVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }

    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy)
    }
}

/// A single health issue.
#[derive(Clone, Debug)]
pub struct HealthIssue {
    pub subsystem: &'static str,
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
}

/// Health report returned by the health command.
#[derive(Clone, Debug)]
pub struct HealthReport {
    pub version: &'static str,
    pub verdict: HealthVerdict,
    pub runtime_ok: bool,
    pub storage_ok: bool,
    pub search_ok: bool,
    pub issues: Vec<HealthIssue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralHealthReport {
    pub schema: &'static str,
    pub snapshot_version: u32,
    pub k_truss: StructuralKTrussSummary,
    pub contradiction_clusters: Vec<StructuralContradictionCluster>,
    pub summary: StructuralHealthSummary,
    pub degraded: Vec<StructuralHealthDegradation>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralKTrussSummary {
    pub max_k: usize,
    pub support_subgraph_memory_count: usize,
    pub top_members: Vec<StructuralKTrussMember>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralKTrussMember {
    pub memory_id: String,
    pub k: usize,
    pub triangle_support: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralContradictionCluster {
    pub cluster_id: String,
    pub memory_count: usize,
    pub contradiction_density: f64,
    pub example_memory_ids: Vec<String>,
    pub severity: String,
    pub suggested_action: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralHealthSummary {
    pub status: String,
    pub k_truss_max_k: usize,
    pub support_subgraph_memory_count: usize,
    pub contradiction_cluster_count: usize,
    pub recommended_command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralHealthDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HealthScorecardOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub record_snapshot: bool,
    pub history_limit: u32,
    pub now_rfc3339: Option<&'a str>,
}

impl<'a> HealthScorecardOptions<'a> {
    #[must_use]
    pub fn new(workspace_path: &'a Path) -> Self {
        Self {
            workspace_path,
            database_path: None,
            record_snapshot: false,
            history_limit: HEALTH_SCORECARD_TREND_LIMIT,
            now_rfc3339: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScorecardReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub score: u32,
    pub status: String,
    pub sub_scores: Vec<HealthScorecardSubScore>,
    pub trend: HealthScorecardTrend,
    pub top_actions: Vec<HealthScorecardAction>,
    pub snapshot: HealthScorecardSnapshotSummary,
    pub evidence: HealthScorecardEvidence,
    pub degraded: Vec<HealthScorecardDegradation>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScorecardSubScore {
    pub name: String,
    pub score: u32,
    pub weight: f64,
    pub status: String,
    pub current: f64,
    pub target: f64,
    pub rationale: String,
    pub signals: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScorecardTrend {
    pub direction: String,
    pub delta: i32,
    pub current_score: u32,
    pub previous_score: Option<u32>,
    pub snapshot_count: usize,
    pub source: String,
    pub latest_snapshot_day: Option<String>,
    pub latest_snapshot_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScorecardAction {
    pub rank: u32,
    pub id: String,
    pub sub_score: String,
    pub title: String,
    pub reason: String,
    pub command: String,
    pub expected_impact: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScorecardSnapshotSummary {
    pub requested: bool,
    pub recorded_this_run: bool,
    pub status: String,
    pub snapshot_day: Option<String>,
    pub report_hash: Option<String>,
    pub inserted: Option<bool>,
    pub history_count: usize,
    pub next_command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScorecardEvidence {
    pub memory_count: usize,
    pub query_miss_count: u32,
    pub gap_count: u32,
    pub debt_item_count: usize,
    pub stale_anchor_count: usize,
    pub low_trust_high_rank_count: usize,
    pub orphan_count: usize,
    pub never_retrieved_count: usize,
    pub missing_provenance_count: usize,
    pub unverified_provenance_count: usize,
    pub mismatched_provenance_count: usize,
    pub low_trust_count: usize,
    pub exact_duplicate_group_count: usize,
    pub exact_duplicate_memory_count: usize,
    pub graph_contradiction_cluster_count: usize,
    pub graph_support_memory_count: usize,
    pub graph_max_k: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScorecardDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

impl HealthReport {
    /// Gather current health status.
    #[must_use]
    pub fn gather() -> Self {
        let workspace_path = default_workspace_path();
        Self::gather_with_workspace(workspace_path.as_deref())
    }

    #[must_use]
    pub fn gather_for_workspace(workspace_path: &Path) -> Self {
        Self::gather_with_workspace(Some(workspace_path))
    }

    #[must_use]
    pub fn gather_with_workspace(workspace_path: Option<&Path>) -> Self {
        let version = build_info().version;
        let runtime_status = probe_runtime_capability();
        let runtime_ok = runtime_status == CapabilityStatus::Ready;
        let storage_status = probe_storage_capability(workspace_path);
        let search_status = probe_search_capability(workspace_path);
        let storage_ok = storage_status == CapabilityStatus::Ready;
        let search_ok = search_status == CapabilityStatus::Ready;

        let mut issues = Vec::new();

        if !runtime_ok {
            issues.push(HealthIssue {
                subsystem: "runtime",
                code: "runtime_unavailable",
                severity: "high",
                message: "Asupersync runtime failed to initialize.",
            });
        }

        if storage_status == CapabilityStatus::Pending {
            issues.push(HealthIssue {
                subsystem: "storage",
                code: "storage_not_ready",
                severity: "medium",
                message: "Workspace storage has not been initialized.",
            });
        } else if !storage_ok {
            issues.push(HealthIssue {
                subsystem: "storage",
                code: "storage_degraded",
                severity: "high",
                message: "Workspace storage exists but failed readiness checks.",
            });
        }

        if search_status == CapabilityStatus::Pending {
            issues.push(HealthIssue {
                subsystem: "search",
                code: "search_not_ready",
                severity: "medium",
                message: "Search is waiting for workspace storage before it can be inspected.",
            });
        } else if !search_ok {
            issues.push(HealthIssue {
                subsystem: "search",
                code: "search_index_degraded",
                severity: "medium",
                message: "Search is compiled but the selected workspace index is not ready.",
            });
        }

        let verdict = if issues.is_empty() {
            HealthVerdict::Healthy
        } else if issues.iter().any(|i| i.severity == "high") {
            HealthVerdict::Unhealthy
        } else {
            HealthVerdict::Degraded
        };

        Self {
            version,
            verdict,
            runtime_ok,
            storage_ok,
            search_ok,
            issues,
        }
    }

    /// Count of issues by severity.
    #[must_use]
    pub fn issue_count(&self) -> usize {
        self.issues.len()
    }

    /// Count of high-severity issues.
    #[must_use]
    pub fn high_severity_count(&self) -> usize {
        self.issues.iter().filter(|i| i.severity == "high").count()
    }

    /// Count of medium-severity issues.
    #[must_use]
    pub fn medium_severity_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == "medium")
            .count()
    }
}

impl HealthScorecardReport {
    pub fn gather(options: &HealthScorecardOptions<'_>) -> Result<Self, DomainError> {
        let workspace_path = options.workspace_path;
        let database_path = options
            .database_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
        let workspace_id = stable_workspace_id(workspace_path);
        let mut degraded = Vec::new();

        if !database_path.exists() {
            degraded.push(HealthScorecardDegradation {
                code: "storage_not_ready".to_owned(),
                severity: "medium".to_owned(),
                message: "Workspace storage has not been initialized.".to_owned(),
                repair: "ee init --workspace .".to_owned(),
            });
            return Ok(Self::from_parts(
                workspace_path,
                &database_path,
                workspace_id,
                Vec::new(),
                None,
                None,
                StructuralHealthReport::empty_with_degradation(storage_not_ready_degradation(
                    Some(&database_path),
                )),
                Vec::new(),
                HealthScorecardSnapshotSummary::not_recorded(options.record_snapshot, 0),
                degraded,
            ));
        }

        let snapshot_summary = if options.record_snapshot {
            match run_memory_debt_snapshot(&MemoryDebtSnapshotOptions {
                workspace_path,
                database_path: Some(&database_path),
                now_rfc3339: options.now_rfc3339,
                dry_run: false,
                limit: Some(50),
            }) {
                Ok(report) => HealthScorecardSnapshotSummary::from_snapshot_report(&report),
                Err(error) => {
                    degraded.push(HealthScorecardDegradation {
                        code: "health_scorecard_snapshot_failed".to_owned(),
                        severity: "warning".to_owned(),
                        message: format!("Could not record memory-debt trend snapshot: {error}"),
                        repair: "ee steward run --job memory_debt_snapshot --workspace . --json"
                            .to_owned(),
                    });
                    HealthScorecardSnapshotSummary::not_recorded(true, 0)
                }
            }
        } else {
            HealthScorecardSnapshotSummary::not_recorded(false, 0)
        };

        let connection = DbConnection::open_file_read_only(&database_path).map_err(|error| {
            DomainError::Storage {
                message: format!(
                    "Failed to open database {} for health scorecard: {error}",
                    database_path.display()
                ),
                repair: Some("ee doctor --workspace . --json".to_owned()),
            }
        })?;
        let workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
            &connection,
            &workspace_id,
            &[workspace_path],
        )
        .unwrap_or(workspace_id);

        let memories = connection
            .list_memories(&workspace_id, None, false)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to list memories for health scorecard: {error}"),
                repair: Some("ee doctor --workspace . --json".to_owned()),
            })?;
        let history = connection
            .list_debt_snapshots(&workspace_id, options.history_limit.max(1))
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to list memory-debt snapshots for health scorecard: {error}"
                ),
                repair: Some(
                    "ee steward run --job memory_debt_snapshot --workspace . --json".to_owned(),
                ),
            })?;
        let deterministic_now = health_scorecard_now(options.now_rfc3339, &memories, &history);

        let debt_report = match run_memory_debt_doctor(&MemoryDebtDoctorOptions {
            workspace_path,
            database_path: Some(&database_path),
            class_filter: None,
            limit: 50,
            trend: true,
            now_rfc3339: Some(deterministic_now.as_str()),
            audit_scan_limit: None,
        }) {
            Ok(report) => Some(report),
            Err(error) => {
                degraded.push(HealthScorecardDegradation {
                    code: "health_scorecard_debt_unavailable".to_owned(),
                    severity: "warning".to_owned(),
                    message: format!("Memory-debt report was unavailable: {error}"),
                    repair: "ee curate doctor --workspace . --trend --json".to_owned(),
                });
                None
            }
        };

        let learn_gaps = match show_gaps(&LearnGapsOptions {
            workspace: workspace_path.to_path_buf(),
            since: None,
            limit: HEALTH_SCORECARD_LEARN_GAP_LIMIT,
        }) {
            Ok(report) => Some(report),
            Err(error) => {
                degraded.push(HealthScorecardDegradation {
                    code: "health_scorecard_gaps_unavailable".to_owned(),
                    severity: "info".to_owned(),
                    message: format!("Learn-gaps demand could not be read: {error}"),
                    repair: "ee learn gaps --workspace . --json".to_owned(),
                });
                None
            }
        };

        let structural =
            StructuralHealthReport::gather_from_connection(&connection, workspace_path);
        let mut snapshot_summary = snapshot_summary;
        snapshot_summary.history_count = history.len();

        Ok(Self::from_parts(
            workspace_path,
            &database_path,
            workspace_id,
            memories,
            debt_report.as_ref(),
            learn_gaps.as_ref(),
            structural,
            history,
            snapshot_summary,
            degraded,
        ))
    }

    fn from_parts(
        workspace_path: &Path,
        database_path: &Path,
        workspace_id: String,
        memories: Vec<StoredMemory>,
        debt_report: Option<&MemoryDebtReport>,
        learn_gaps: Option<&super::learn::LearnGapsReport>,
        structural: StructuralHealthReport,
        history: Vec<StoredDebtSnapshot>,
        snapshot_summary: HealthScorecardSnapshotSummary,
        mut degraded: Vec<HealthScorecardDegradation>,
    ) -> Self {
        let evidence = health_scorecard_evidence(&memories, debt_report, learn_gaps, &structural);
        degraded.extend(structural.degraded.iter().map(|entry| {
            HealthScorecardDegradation {
                code: format!("structural_health.{}", entry.code),
                severity: entry.severity.clone(),
                message: entry.message.clone(),
                repair: entry.repair.clone().unwrap_or_else(|| {
                    "ee health --robot-insights --workspace . --json".to_owned()
                }),
            }
        }));
        if learn_gaps.is_none() {
            degraded.push(HealthScorecardDegradation {
                code: "learn_gaps_unavailable".to_owned(),
                severity: "info".to_owned(),
                message: "No query-miss demand was available for coverage scoring.".to_owned(),
                repair: "Run searches normally, then rerun ee learn gaps --workspace . --json."
                    .to_owned(),
            });
        }

        let sub_scores = health_scorecard_sub_scores(&evidence, &structural);
        let score = composite_health_score(&sub_scores);
        let trend = health_scorecard_trend(score, &history);
        let top_actions = top_scorecard_actions(&sub_scores, &evidence, &trend);
        let status = health_scorecard_status(score).to_owned();

        Self {
            schema: HEALTH_SCORECARD_SCHEMA_V1,
            command: "health scorecard",
            version: build_info().version,
            workspace_id,
            workspace_path: workspace_path.display().to_string(),
            database_path: database_path.display().to_string(),
            score,
            status,
            sub_scores,
            trend,
            top_actions,
            snapshot: snapshot_summary,
            evidence,
            degraded,
        }
    }
}

impl HealthScorecardSnapshotSummary {
    fn not_recorded(requested: bool, history_count: usize) -> Self {
        Self {
            requested,
            recorded_this_run: false,
            status: if requested {
                "not_recorded".to_owned()
            } else {
                "not_requested".to_owned()
            },
            snapshot_day: None,
            report_hash: None,
            inserted: None,
            history_count,
            next_command: "ee steward run --job memory_debt_snapshot --workspace . --json"
                .to_owned(),
        }
    }

    fn from_snapshot_report(report: &MemoryDebtSnapshotReport) -> Self {
        Self {
            requested: true,
            recorded_this_run: report.inserted,
            status: report.status.clone(),
            snapshot_day: Some(report.snapshot_day.clone()),
            report_hash: Some(report.report_hash.clone()),
            inserted: Some(report.inserted),
            history_count: 0,
            next_command: "ee health scorecard --workspace . --json".to_owned(),
        }
    }
}

fn health_scorecard_evidence(
    memories: &[StoredMemory],
    debt_report: Option<&MemoryDebtReport>,
    learn_gaps: Option<&super::learn::LearnGapsReport>,
    structural: &StructuralHealthReport,
) -> HealthScorecardEvidence {
    let class_counts = debt_report
        .map(|report| &report.summary.class_counts)
        .cloned()
        .unwrap_or_default();
    let duplicate_stats = exact_duplicate_stats(memories);
    HealthScorecardEvidence {
        memory_count: memories.len(),
        query_miss_count: learn_gaps.map_or(0, |report| report.scanned_miss_count),
        gap_count: learn_gaps.map_or(0, |report| report.cluster_count),
        debt_item_count: debt_report.map_or(0, |report| report.summary.item_count),
        stale_anchor_count: class_count(&class_counts, "stale_anchor"),
        low_trust_high_rank_count: class_count(&class_counts, "low_trust_high_rank"),
        orphan_count: class_count(&class_counts, "orphan"),
        never_retrieved_count: class_count(&class_counts, "never_retrieved"),
        missing_provenance_count: memories
            .iter()
            .filter(|memory| {
                memory
                    .provenance_uri
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            })
            .count(),
        unverified_provenance_count: memories
            .iter()
            .filter(|memory| {
                !matches!(
                    memory.provenance_verification_status.as_str(),
                    "verified" | "current" | "fresh"
                )
            })
            .count(),
        mismatched_provenance_count: memories
            .iter()
            .filter(|memory| {
                matches!(
                    memory.provenance_verification_status.as_str(),
                    "mismatch" | "missing" | "stale" | "suspect"
                )
            })
            .count(),
        low_trust_count: memories
            .iter()
            .filter(|memory| trust_class_score(&memory.trust_class) < 0.7)
            .count(),
        exact_duplicate_group_count: duplicate_stats.0,
        exact_duplicate_memory_count: duplicate_stats.1,
        graph_contradiction_cluster_count: structural.contradiction_clusters.len(),
        graph_support_memory_count: structural.k_truss.support_subgraph_memory_count,
        graph_max_k: structural.k_truss.max_k,
    }
}

fn health_scorecard_now(
    override_now: Option<&str>,
    memories: &[StoredMemory],
    history: &[StoredDebtSnapshot],
) -> String {
    if let Some(now) = override_now {
        return now.to_owned();
    }
    memories
        .iter()
        .map(|memory| memory.updated_at.as_str())
        .chain(history.iter().map(|snapshot| snapshot.created_at.as_str()))
        .max()
        .unwrap_or("1970-01-01T00:00:00Z")
        .to_owned()
}

fn class_count(class_counts: &BTreeMap<String, usize>, class: &str) -> usize {
    class_counts.get(class).copied().unwrap_or(0)
}

fn exact_duplicate_stats(memories: &[StoredMemory]) -> (usize, usize) {
    let mut by_content = BTreeMap::<String, usize>::new();
    for memory in memories {
        let normalized = normalize_duplicate_content(&memory.content);
        if normalized.is_empty() {
            continue;
        }
        *by_content.entry(normalized).or_insert(0) += 1;
    }
    by_content
        .values()
        .filter(|count| **count > 1)
        .fold((0, 0), |(groups, memories), count| {
            (groups + 1, memories + count)
        })
}

fn normalize_duplicate_content(content: &str) -> String {
    content
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn health_scorecard_sub_scores(
    evidence: &HealthScorecardEvidence,
    structural: &StructuralHealthReport,
) -> Vec<HealthScorecardSubScore> {
    vec![
        coverage_sub_score(evidence),
        freshness_sub_score(evidence),
        trust_sub_score(evidence),
        redundancy_sub_score(evidence),
        graph_sub_score(evidence, structural),
    ]
}

fn coverage_sub_score(evidence: &HealthScorecardEvidence) -> HealthScorecardSubScore {
    let missed = f64::from(evidence.gap_count)
        + f64::from(evidence.query_miss_count) * 0.25
        + evidence.orphan_count as f64 * 0.75
        + evidence.never_retrieved_count as f64 * 0.75;
    let current = evidence.memory_count as f64;
    let score = if evidence.memory_count == 0 {
        20
    } else {
        ratio_score(current, current + missed)
    };
    sub_score(
        "coverage",
        score,
        0.25,
        current,
        current + missed,
        "Demand met by existing memories versus query-miss and debt evidence.",
        vec![
            format!("memories={}", evidence.memory_count),
            format!("queryMisses={}", evidence.query_miss_count),
            format!("gapClusters={}", evidence.gap_count),
            format!("orphanDebt={}", evidence.orphan_count),
            format!("neverRetrievedDebt={}", evidence.never_retrieved_count),
        ],
    )
}

fn freshness_sub_score(evidence: &HealthScorecardEvidence) -> HealthScorecardSubScore {
    let denominator = evidence.memory_count.max(1) as f64;
    let penalty = (evidence.missing_provenance_count as f64 * 12.0
        + evidence.unverified_provenance_count as f64 * 8.0
        + evidence.mismatched_provenance_count as f64 * 18.0
        + evidence.stale_anchor_count as f64 * 14.0)
        / denominator;
    let score = bounded_score(100.0 - penalty);
    sub_score(
        "freshness",
        score,
        0.20,
        denominator - evidence.mismatched_provenance_count as f64,
        denominator,
        "Provenance verification and stale-anchor debt for active memories.",
        vec![
            format!("missingProvenance={}", evidence.missing_provenance_count),
            format!(
                "unverifiedProvenance={}",
                evidence.unverified_provenance_count
            ),
            format!(
                "mismatchedProvenance={}",
                evidence.mismatched_provenance_count
            ),
            format!("staleAnchors={}", evidence.stale_anchor_count),
        ],
    )
}

fn trust_sub_score(evidence: &HealthScorecardEvidence) -> HealthScorecardSubScore {
    let denominator = evidence.memory_count.max(1) as f64;
    let low_trust_ratio = evidence.low_trust_count as f64 / denominator;
    let high_rank_ratio = evidence.low_trust_high_rank_count as f64 / denominator;
    let score = bounded_score(100.0 - low_trust_ratio * 35.0 - high_rank_ratio * 45.0);
    sub_score(
        "trust",
        score,
        0.20,
        denominator - evidence.low_trust_count as f64,
        denominator,
        "Trust-class calibration and low-trust memories that still win retrieval.",
        vec![
            format!("lowTrust={}", evidence.low_trust_count),
            format!("lowTrustHighRank={}", evidence.low_trust_high_rank_count),
        ],
    )
}

fn redundancy_sub_score(evidence: &HealthScorecardEvidence) -> HealthScorecardSubScore {
    let denominator = evidence.memory_count.max(1) as f64;
    let duplicate_ratio = evidence.exact_duplicate_memory_count as f64 / denominator;
    let score = bounded_score(
        100.0 - duplicate_ratio * 70.0 - evidence.exact_duplicate_group_count as f64 * 5.0,
    );
    sub_score(
        "redundancy",
        score,
        0.15,
        denominator - evidence.exact_duplicate_memory_count as f64,
        denominator,
        "Exact duplicate burden that can dilute retrieval and curation queues.",
        vec![
            format!("duplicateGroups={}", evidence.exact_duplicate_group_count),
            format!(
                "duplicateMemories={}",
                evidence.exact_duplicate_memory_count
            ),
        ],
    )
}

fn graph_sub_score(
    evidence: &HealthScorecardEvidence,
    structural: &StructuralHealthReport,
) -> HealthScorecardSubScore {
    let contradiction_penalty = evidence.graph_contradiction_cluster_count as f64 * 24.0;
    let support_ratio = if evidence.memory_count == 0 {
        1.0
    } else {
        (evidence.graph_support_memory_count as f64 / evidence.memory_count as f64).min(1.0)
    };
    let support_penalty = (1.0 - support_ratio) * 12.0;
    let feature_penalty = structural
        .degraded
        .iter()
        .filter(|entry| entry.severity != "info")
        .count() as f64
        * 10.0;
    let score = bounded_score(
        100.0 + evidence.graph_max_k.min(5) as f64 * 2.0
            - contradiction_penalty
            - support_penalty
            - feature_penalty,
    );
    sub_score(
        "graph",
        score,
        0.20,
        evidence.graph_support_memory_count as f64,
        evidence.memory_count.max(1) as f64,
        "Structural coherence from k-truss support and contradiction clusters.",
        vec![
            format!(
                "contradictionClusters={}",
                evidence.graph_contradiction_cluster_count
            ),
            format!("supportMemories={}", evidence.graph_support_memory_count),
            format!("maxK={}", evidence.graph_max_k),
        ],
    )
}

fn sub_score(
    name: &str,
    score: u32,
    weight: f64,
    current: f64,
    target: f64,
    rationale: &str,
    signals: Vec<String>,
) -> HealthScorecardSubScore {
    HealthScorecardSubScore {
        name: name.to_owned(),
        score,
        weight,
        status: health_scorecard_status(score).to_owned(),
        current: round_f64(current),
        target: round_f64(target),
        rationale: rationale.to_owned(),
        signals,
    }
}

fn ratio_score(numerator: f64, denominator: f64) -> u32 {
    if denominator <= SCORE_EPSILON {
        100
    } else {
        bounded_score((numerator / denominator) * 100.0)
    }
}

fn bounded_score(score: f64) -> u32 {
    score.round().clamp(0.0, 100.0) as u32
}

fn composite_health_score(sub_scores: &[HealthScorecardSubScore]) -> u32 {
    let total_weight = sub_scores.iter().map(|score| score.weight).sum::<f64>();
    if total_weight <= SCORE_EPSILON {
        return 0;
    }
    bounded_score(
        sub_scores
            .iter()
            .map(|score| f64::from(score.score) * score.weight)
            .sum::<f64>()
            / total_weight,
    )
}

const fn health_scorecard_status(score: u32) -> &'static str {
    if score >= 85 {
        "ok"
    } else if score >= 65 {
        "degraded_recoverable"
    } else if score >= 45 {
        "degraded_required"
    } else {
        "blocked"
    }
}

fn health_scorecard_trend(
    current_score: u32,
    history: &[StoredDebtSnapshot],
) -> HealthScorecardTrend {
    let previous = history.iter().find_map(health_score_from_debt_snapshot);
    let delta = previous.map(|score| current_score as i32 - score as i32);
    let direction = match delta {
        None => "no_baseline",
        Some(value) if value > 1 => "improving",
        Some(value) if value < -1 => "declining",
        Some(_) => "flat",
    };
    let latest = history.first();
    HealthScorecardTrend {
        direction: direction.to_owned(),
        delta: delta.unwrap_or(0),
        current_score,
        previous_score: previous,
        snapshot_count: history.len(),
        source: if history.is_empty() {
            "none".to_owned()
        } else {
            "debt_snapshots".to_owned()
        },
        latest_snapshot_day: latest.map(|snapshot| snapshot.snapshot_day.clone()),
        latest_snapshot_hash: latest.map(|snapshot| snapshot.report_hash.clone()),
    }
}

fn health_score_from_debt_snapshot(snapshot: &StoredDebtSnapshot) -> Option<u32> {
    if let Ok(value) = serde_json::from_str::<JsonValue>(&snapshot.report_json) {
        if value.get("schema").and_then(JsonValue::as_str) == Some(HEALTH_SCORECARD_SCHEMA_V1) {
            return value
                .get("score")
                .and_then(JsonValue::as_u64)
                .map(|score| score.min(100) as u32);
        }
        let memory_count = value
            .get("summary")
            .and_then(|summary| summary.get("memoryCount"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(snapshot.item_count)
            .max(1) as f64;
        let item_count = snapshot.item_count as f64;
        let debt_ratio = item_count / (memory_count + item_count);
        let average_debt = if item_count <= SCORE_EPSILON {
            0.0
        } else {
            f64::from(snapshot.total_score) / item_count
        };
        return Some(bounded_score(
            100.0 - debt_ratio * 45.0 - average_debt * 25.0,
        ));
    }
    None
}

fn top_scorecard_actions(
    sub_scores: &[HealthScorecardSubScore],
    evidence: &HealthScorecardEvidence,
    trend: &HealthScorecardTrend,
) -> Vec<HealthScorecardAction> {
    let mut candidates = sub_scores
        .iter()
        .filter(|sub_score| sub_score.score < 95)
        .map(|sub_score| {
            let impact = round_f64((100.0 - f64::from(sub_score.score)) * sub_score.weight);
            action_for_sub_score(sub_score, evidence, trend, impact)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .expected_impact
            .total_cmp(&left.expected_impact)
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.truncate(3);
    for (index, action) in candidates.iter_mut().enumerate() {
        action.rank = index as u32 + 1;
    }
    candidates
}

fn action_for_sub_score(
    sub_score: &HealthScorecardSubScore,
    evidence: &HealthScorecardEvidence,
    trend: &HealthScorecardTrend,
    expected_impact: f64,
) -> HealthScorecardAction {
    let (title, reason, command) = match sub_score.name.as_str() {
        "coverage" if evidence.memory_count == 0 => (
            "Capture the first sourced memory",
            "The store has no active memories, so retrieval demand cannot be met.",
            "ee remember --workspace . --level procedural --kind rule \"<sourced lesson>\" --json",
        ),
        "coverage" => (
            "Close repeated query-miss gaps",
            "Coverage is limited by retained miss demand or unretrieved/orphan debt.",
            "ee learn gaps --workspace . --limit 5 --json",
        ),
        "freshness" => (
            "Refresh stale or unverifiable provenance",
            "Freshness is limited by missing, pending, stale, or mismatched provenance.",
            "ee memory drift --workspace . --mode all-memories --json",
        ),
        "trust" => (
            "Validate low-trust high-rank memories",
            "Low-trust memories are still shaping retrieval and should receive outcome evidence.",
            "ee curate doctor --workspace . --class low_trust_high_rank --json",
        ),
        "redundancy" => (
            "Collapse duplicate memories",
            "Duplicate memories dilute ranking and should be merged or retired deliberately.",
            "ee curate candidates --workspace . --group-duplicates --json",
        ),
        "graph" => (
            "Inspect structural contradictions",
            "Graph health is limited by weak support or contradiction clusters.",
            "ee health --robot-insights --workspace . --json",
        ),
        _ if trend.direction == "no_baseline" => (
            "Record a trend baseline",
            "No steward trend snapshot exists, so the scorecard cannot show movement yet.",
            "ee steward run --job memory_debt_snapshot --workspace . --json",
        ),
        _ => (
            "Review memory hygiene queue",
            "The scorecard found a lower-scoring area without a more specific action.",
            "ee curate doctor --workspace . --trend --json",
        ),
    };
    HealthScorecardAction {
        rank: 0,
        id: format!("health_scorecard.{}", sub_score.name),
        sub_score: sub_score.name.clone(),
        title: title.to_owned(),
        reason: reason.to_owned(),
        command: command.to_owned(),
        expected_impact,
    }
}

fn trust_class_score(trust_class: &str) -> f64 {
    match trust_class {
        "human_explicit" | "human_verified" => 1.0,
        "peer_human_attested" => 0.95,
        "agent_validated" | "source_verified" => 0.9,
        "imported" | "cass_import" => 0.7,
        "agent_assertion" => 0.6,
        "cass_evidence" => 0.55,
        "derived" | "inferred" => 0.55,
        "legacy_import" => 0.4,
        _ => 0.5,
    }
}

fn round_f64(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

impl StructuralHealthReport {
    #[must_use]
    pub fn disabled_by_feature_flag() -> Self {
        Self::empty_with_degradation(graph_feature_disabled_degradation())
    }

    #[must_use]
    pub fn gather() -> Self {
        let workspace_path = default_workspace_path();
        Self::gather_with_workspace(workspace_path.as_deref())
    }

    #[must_use]
    pub fn gather_for_workspace(workspace_path: &Path) -> Self {
        Self::gather_with_workspace(Some(workspace_path))
    }

    #[must_use]
    pub fn gather_with_workspace(workspace_path: Option<&Path>) -> Self {
        let Some(workspace_path) = workspace_path else {
            return Self::empty_with_degradation(storage_not_ready_degradation(None));
        };

        let database_path = workspace_path.join(".ee").join("ee.db");
        if !database_path.exists() {
            return Self::empty_with_degradation(storage_not_ready_degradation(Some(
                &database_path,
            )));
        }

        let connection = match DbConnection::open_file(&database_path) {
            Ok(connection) => connection,
            Err(error) => {
                return Self::empty_with_degradation(StructuralHealthDegradation {
                    code: "storage_degraded".to_owned(),
                    severity: "high".to_owned(),
                    message: format!("Workspace storage could not be opened: {error}"),
                    repair: Some("ee doctor --workspace .".to_owned()),
                });
            }
        };

        Self::gather_from_connection(&connection, workspace_path)
    }

    pub fn gather_from_connection(connection: &DbConnection, workspace_path: &Path) -> Self {
        let workspace_ids = resolve_health_workspace_ids(connection, workspace_path);
        let snapshot_version = latest_memory_link_snapshot_version(connection, &workspace_ids);
        let memory_ids = workspace_memory_ids(connection, &workspace_ids);
        let links = match connection.list_all_memory_links(None) {
            Ok(links) => links,
            Err(error) => {
                return Self::empty_with_degradation(StructuralHealthDegradation {
                    code: GRAPH_METRICS_UNAVAILABLE.id.to_owned(),
                    severity: GRAPH_METRICS_UNAVAILABLE.severity.as_str().to_owned(),
                    message: format!("Memory graph links could not be read: {error}"),
                    repair: GRAPH_METRICS_UNAVAILABLE.repair.map(str::to_owned),
                });
            }
        };
        let links = health_visible_memory_links(links);

        let support_graph = relation_graph(&links, &memory_ids, MemoryLinkRelation::Supports);
        let contradiction_graph =
            relation_graph(&links, &memory_ids, MemoryLinkRelation::Contradicts);
        let k_truss = compute_k_truss(&support_graph);
        let contradiction_clusters = detect_contradiction_clusters(&contradiction_graph);
        Self::from_graph_reports(
            snapshot_version,
            &support_graph,
            &k_truss,
            &contradiction_clusters,
            contradiction_graph.edge_count(),
        )
    }

    fn empty_with_degradation(degradation: StructuralHealthDegradation) -> Self {
        let support_graph = Graph::new(CompatibilityMode::Strict);
        let k_truss = compute_k_truss(&support_graph);
        Self::from_graph_reports(0, &support_graph, &k_truss, &[], 1).with_degradation(degradation)
    }

    fn from_graph_reports(
        snapshot_version: u32,
        support_graph: &Graph,
        k_truss: &KTrussReport,
        contradiction_clusters: &[ContradictionCluster],
        contradiction_edge_count: usize,
    ) -> Self {
        let mut degraded = Vec::new();
        if contradiction_edge_count == 0 {
            degraded.push(StructuralHealthDegradation {
                code: GRAPH_HEALTH_NO_CONTRADICTIONS_CODE.to_owned(),
                severity: "info".to_owned(),
                message: "Structural health found 0 contradiction edges.".to_owned(),
                repair: None,
            });
        }

        let k_truss_summary = StructuralKTrussSummary {
            max_k: k_truss.max_k,
            support_subgraph_memory_count: support_graph.node_count(),
            top_members: k_truss
                .top_memories_at_k
                .iter()
                .take(10)
                .map(|member| StructuralKTrussMember {
                    memory_id: member.memory_id.clone(),
                    k: member.max_k,
                    triangle_support: triangle_support(support_graph, &member.memory_id),
                })
                .collect(),
        };

        let contradiction_clusters = contradiction_clusters
            .iter()
            .map(|cluster| StructuralContradictionCluster {
                cluster_id: format!("contradictions.{}", cluster.louvain_id),
                memory_count: cluster.size,
                contradiction_density: cluster.density,
                example_memory_ids: cluster.exemplar_memory_ids.clone(),
                severity: contradiction_severity_str(cluster.severity).to_owned(),
                suggested_action: cluster.suggested_action.to_owned(),
            })
            .collect::<Vec<_>>();

        let status = if contradiction_clusters.is_empty() {
            "ok"
        } else {
            "degraded_recoverable"
        };
        let mut report = Self {
            schema: HEALTH_STRUCTURAL_SCHEMA_V1,
            snapshot_version,
            k_truss: k_truss_summary,
            summary: StructuralHealthSummary {
                status: status.to_owned(),
                k_truss_max_k: k_truss.max_k,
                support_subgraph_memory_count: support_graph.node_count(),
                contradiction_cluster_count: contradiction_clusters.len(),
                recommended_command: "ee curate candidates --workspace . --json".to_owned(),
            },
            contradiction_clusters,
            degraded,
        };

        if !report.degraded.is_empty() && report.summary.status == "ok" {
            report.summary.status = "degraded_recoverable".to_owned();
        }
        report
    }

    fn with_degradation(mut self, degradation: StructuralHealthDegradation) -> Self {
        self.degraded.push(degradation);
        self.summary.status = "degraded_required".to_owned();
        self
    }
}

fn storage_not_ready_degradation(database_path: Option<&Path>) -> StructuralHealthDegradation {
    StructuralHealthDegradation {
        code: "storage_not_ready".to_owned(),
        severity: "medium".to_owned(),
        message: "Workspace storage has not been initialized.".to_owned(),
        repair: Some(match database_path {
            Some(path) => crate::core::storeless_workspace_repair(path),
            None => "ee init --workspace .".to_owned(),
        }),
    }
}

fn graph_feature_disabled_degradation() -> StructuralHealthDegradation {
    StructuralHealthDegradation {
        code: "graph_feature_disabled".to_owned(),
        severity: "medium".to_owned(),
        message: "Structural graph health is disabled by graph.feature.structural_health.enabled."
            .to_owned(),
        repair: Some("ee config set graph.feature.structural_health.enabled true".to_owned()),
    }
}

fn resolve_health_workspace_ids(
    connection: &DbConnection,
    workspace_path: &Path,
) -> BTreeSet<String> {
    let mut candidates = BTreeSet::new();
    candidates.insert(stable_workspace_id(workspace_path));
    if let Ok(canonical) = workspace_path.canonicalize() {
        candidates.insert(stable_workspace_id(&canonical));
    }
    if let Ok(Some(workspace)) = crate::core::workspace::select_existing_workspace_row(
        connection,
        &stable_workspace_id(workspace_path),
        &[workspace_path],
    ) {
        candidates.insert(workspace.id);
    }
    candidates
}

fn health_visible_memory_links(links: Vec<StoredMemoryLink>) -> Vec<StoredMemoryLink> {
    links
        .into_iter()
        .filter(|link| {
            crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
        })
        .collect()
}

fn workspace_memory_ids(
    connection: &DbConnection,
    workspace_ids: &BTreeSet<String>,
) -> BTreeSet<String> {
    workspace_ids
        .iter()
        .filter_map(|workspace_id| connection.list_memories(workspace_id, None, true).ok())
        .flatten()
        .map(|memory| memory.id)
        .collect()
}

fn latest_memory_link_snapshot_version(
    connection: &DbConnection,
    workspace_ids: &BTreeSet<String>,
) -> u32 {
    workspace_ids
        .iter()
        .filter_map(|workspace_id| {
            connection
                .get_latest_graph_snapshot(workspace_id, GraphSnapshotType::MemoryLinks)
                .ok()
                .flatten()
                .map(|snapshot| snapshot.snapshot_version)
        })
        .max()
        .unwrap_or(0)
}

fn relation_graph(
    links: &[StoredMemoryLink],
    memory_ids: &BTreeSet<String>,
    relation: MemoryLinkRelation,
) -> Graph {
    let mut graph = Graph::new(CompatibilityMode::Strict);
    for link in links {
        if link.relation_enum() != Some(relation)
            || !memory_ids.contains(&link.src_memory_id)
            || !memory_ids.contains(&link.dst_memory_id)
        {
            continue;
        }
        graph.add_node(&link.src_memory_id);
        graph.add_node(&link.dst_memory_id);
        let _ = graph
            .extend_edges_unrecorded([(link.src_memory_id.as_str(), link.dst_memory_id.as_str())]);
    }
    graph
}

fn triangle_support(graph: &Graph, memory_id: &str) -> usize {
    let Some(neighbors) = graph.neighbors(memory_id) else {
        return 0;
    };
    let neighbors = neighbors.into_iter().collect::<Vec<_>>();
    let mut count = 0;
    for (index, left) in neighbors.iter().enumerate() {
        for right in neighbors.iter().skip(index + 1) {
            if graph.has_edge(left, right) {
                count += 1;
            }
        }
    }
    count
}

const fn contradiction_severity_str(severity: ContradictionSeverity) -> &'static str {
    match severity {
        ContradictionSeverity::Inconsistent => "inconsistent",
        ContradictionSeverity::Incoherent => "incoherent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
        MemoryLinkRelation, MemoryLinkSource,
    };

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn trust_class_health_scores_follow_canonical_order() {
        let ordered = [
            "human_explicit",
            "peer_human_attested",
            "agent_validated",
            "agent_assertion",
            "cass_evidence",
            "legacy_import",
        ];
        for adjacent in ordered.windows(2) {
            assert!(
                trust_class_score(adjacent[0]) > trust_class_score(adjacent[1]),
                "{} must rank above {}",
                adjacent[0],
                adjacent[1]
            );
        }
    }

    #[test]
    fn health_report_gather_returns_valid_report() -> TestResult {
        let report = HealthReport::gather_with_workspace(None);

        ensure(
            report.version,
            env!("CARGO_PKG_VERSION"),
            "version from cargo",
        )?;
        ensure(report.runtime_ok, true, "runtime is ok")?;
        ensure(report.storage_ok, false, "storage not yet ok")?;
        ensure(report.search_ok, false, "search not yet ok")
    }

    #[test]
    fn health_report_verdict_is_degraded_when_medium_issues() -> TestResult {
        let report = HealthReport::gather_with_workspace(None);

        ensure(
            report.verdict,
            HealthVerdict::Degraded,
            "verdict is degraded with medium issues",
        )?;
        ensure(
            report.verdict.as_str(),
            "degraded",
            "verdict string is degraded",
        )
    }

    #[test]
    fn health_report_issue_counts_are_correct() -> TestResult {
        let report = HealthReport::gather_with_workspace(None);

        ensure(report.issue_count(), 2, "two issues total")?;
        ensure(report.high_severity_count(), 0, "no high severity issues")?;
        ensure(
            report.medium_severity_count(),
            2,
            "two medium severity issues",
        )
    }

    #[test]
    fn health_verdict_strings_are_stable() -> TestResult {
        ensure(HealthVerdict::Healthy.as_str(), "healthy", "healthy")?;
        ensure(HealthVerdict::Degraded.as_str(), "degraded", "degraded")?;
        ensure(HealthVerdict::Unhealthy.as_str(), "unhealthy", "unhealthy")
    }

    #[test]
    fn health_verdict_is_healthy_predicate() -> TestResult {
        ensure(
            HealthVerdict::Healthy.is_healthy(),
            true,
            "healthy is healthy",
        )?;
        ensure(
            HealthVerdict::Degraded.is_healthy(),
            false,
            "degraded is not healthy",
        )?;
        ensure(
            HealthVerdict::Unhealthy.is_healthy(),
            false,
            "unhealthy is not healthy",
        )
    }

    #[test]
    fn structural_health_without_workspace_reports_degradation() -> TestResult {
        let report = StructuralHealthReport::gather_with_workspace(None);

        ensure(
            report.schema,
            HEALTH_STRUCTURAL_SCHEMA_V1,
            "structural schema",
        )?;
        ensure(report.snapshot_version, 0, "snapshot version default")?;
        ensure(
            report.summary.status,
            "degraded_required".to_owned(),
            "missing workspace status",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|degraded| degraded.code == "storage_not_ready"),
            true,
            "storage degradation present",
        )
    }

    #[test]
    fn structural_health_disabled_by_feature_flag_is_schema_valid() -> TestResult {
        let report = StructuralHealthReport::disabled_by_feature_flag();

        ensure(
            report.schema,
            HEALTH_STRUCTURAL_SCHEMA_V1,
            "structural schema",
        )?;
        ensure(report.snapshot_version, 0, "disabled snapshot version")?;
        ensure(
            report.summary.status,
            "degraded_required".to_owned(),
            "disabled status",
        )?;
        ensure(report.k_truss.max_k, 3, "disabled k-truss baseline max k")?;
        ensure(
            report.k_truss.support_subgraph_memory_count,
            0,
            "disabled support graph memory count",
        )?;
        ensure(
            report.k_truss.top_members.is_empty(),
            true,
            "disabled support graph members",
        )?;
        ensure(
            report.summary.k_truss_max_k,
            report.k_truss.max_k,
            "disabled summary k-truss max k",
        )?;
        ensure(
            report.contradiction_clusters.is_empty(),
            true,
            "disabled contradiction clusters",
        )?;
        ensure(report.degraded.len(), 1, "disabled degraded count")?;
        let degraded = &report.degraded[0];
        ensure(
            degraded.code.as_str(),
            "graph_feature_disabled",
            "disabled degraded code",
        )?;
        ensure(
            degraded.severity.as_str(),
            "medium",
            "disabled degraded severity",
        )?;
        ensure(
            degraded.repair.as_deref(),
            Some("ee config set graph.feature.structural_health.enabled true"),
            "disabled repair",
        )
    }

    #[test]
    fn structural_health_from_links_reports_k_truss_and_contradictions() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-health-structural-fixture");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().to_string(),
                    name: Some("health structural".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let support_a = "mem_00000000000000000000008101";
        let support_b = "mem_00000000000000000000008102";
        let support_c = "mem_00000000000000000000008103";
        let conflict_x = "mem_00000000000000000000008104";
        let conflict_y = "mem_00000000000000000000008105";
        let conflict_z = "mem_00000000000000000000008106";
        for memory_id in [
            support_a, support_b, support_c, conflict_x, conflict_y, conflict_z,
        ] {
            insert_test_memory(&connection, &workspace_id, memory_id)?;
        }
        for (link_id, left, right, relation) in [
            (
                "link_00000000000000000000008101",
                support_a,
                support_b,
                MemoryLinkRelation::Supports,
            ),
            (
                "link_00000000000000000000008102",
                support_a,
                support_c,
                MemoryLinkRelation::Supports,
            ),
            (
                "link_00000000000000000000008103",
                support_b,
                support_c,
                MemoryLinkRelation::Supports,
            ),
            (
                "link_00000000000000000000008104",
                conflict_x,
                conflict_y,
                MemoryLinkRelation::Contradicts,
            ),
            (
                "link_00000000000000000000008105",
                conflict_x,
                conflict_z,
                MemoryLinkRelation::Contradicts,
            ),
            (
                "link_00000000000000000000008106",
                conflict_y,
                conflict_z,
                MemoryLinkRelation::Contradicts,
            ),
        ] {
            insert_test_link(&connection, link_id, left, right, relation)?;
        }

        let report = StructuralHealthReport::gather_from_connection(&connection, workspace_path);

        ensure(report.k_truss.max_k, 3, "support triangle max k")?;
        ensure(
            report.k_truss.support_subgraph_memory_count,
            3,
            "support graph node count",
        )?;
        ensure(
            report.contradiction_clusters.len(),
            1,
            "contradiction cluster count",
        )?;
        ensure(
            report.summary.status,
            "degraded_recoverable".to_owned(),
            "cluster status",
        )
    }

    #[test]
    fn structural_health_without_contradictions_emits_graph_health_sentinel() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-health-no-contradictions-fixture");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().to_string(),
                    name: Some("health no contradictions".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let support_a = "mem_00000000000000000000008201";
        let support_b = "mem_00000000000000000000008202";
        insert_test_memory(&connection, &workspace_id, support_a)?;
        insert_test_memory(&connection, &workspace_id, support_b)?;
        insert_test_link(
            &connection,
            "link_00000000000000000000008201",
            support_a,
            support_b,
            MemoryLinkRelation::Supports,
        )?;

        let report = StructuralHealthReport::gather_from_connection(&connection, workspace_path);

        ensure(
            report.k_truss.support_subgraph_memory_count,
            2,
            "support graph memory count",
        )?;
        ensure(
            report.contradiction_clusters.len(),
            0,
            "no contradiction clusters",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|degraded| degraded.code == GRAPH_HEALTH_NO_CONTRADICTIONS_CODE),
            true,
            "graph health sentinel present",
        )
    }

    #[test]
    fn structural_health_ignores_denied_mesh_links() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_path = Path::new("/tmp/ee-health-mesh-filter-fixture");
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().to_string(),
                    name: Some("health mesh filter".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        let local_a = "mem_00000000000000000000008301";
        let local_b = "mem_00000000000000000000008302";
        let denied_c = "mem_00000000000000000000008303";
        for memory_id in [local_a, local_b, denied_c] {
            insert_test_memory(&connection, &workspace_id, memory_id)?;
        }
        insert_test_link(
            &connection,
            "link_00000000000000000000008301",
            local_a,
            local_b,
            MemoryLinkRelation::Supports,
        )?;
        insert_test_link_with_metadata(
            &connection,
            "link_00000000000000000000008302",
            local_b,
            denied_c,
            MemoryLinkRelation::Contradicts,
            Some(health_denied_mesh_link_metadata()),
        )?;

        let report = StructuralHealthReport::gather_from_connection(&connection, workspace_path);

        ensure(
            report.k_truss.support_subgraph_memory_count,
            2,
            "visible support graph memory count",
        )?;
        ensure(
            report.contradiction_clusters.len(),
            0,
            "denied mesh contradiction link must not create a cluster",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|degraded| degraded.code == GRAPH_HEALTH_NO_CONTRADICTIONS_CODE),
            true,
            "no-contradictions sentinel remains present",
        )
    }

    #[test]
    fn health_scorecard_empty_store_surfaces_capture_action() -> TestResult {
        let report = HealthScorecardReport::from_parts(
            Path::new("/tmp/ee-health-scorecard-empty"),
            Path::new("/tmp/ee-health-scorecard-empty/.ee/ee.db"),
            "wsp_scorecard_empty".to_owned(),
            Vec::new(),
            None,
            None,
            structural_health_ok_fixture(),
            Vec::new(),
            HealthScorecardSnapshotSummary::not_recorded(false, 0),
            Vec::new(),
        );

        ensure(report.schema, HEALTH_SCORECARD_SCHEMA_V1, "schema")?;
        ensure(report.evidence.memory_count, 0, "empty memory count")?;
        ensure(
            report.trend.direction.as_str(),
            "no_baseline",
            "empty trend direction",
        )?;
        ensure(
            report
                .top_actions
                .iter()
                .any(|action| action.command.contains("ee remember")),
            true,
            "empty scorecard recommends capture",
        )
    }

    #[test]
    fn health_scorecard_duplicate_memories_lower_redundancy_score() -> TestResult {
        let clean = HealthScorecardEvidence {
            memory_count: 4,
            query_miss_count: 0,
            gap_count: 0,
            debt_item_count: 0,
            stale_anchor_count: 0,
            low_trust_high_rank_count: 0,
            orphan_count: 0,
            never_retrieved_count: 0,
            missing_provenance_count: 0,
            unverified_provenance_count: 0,
            mismatched_provenance_count: 0,
            low_trust_count: 0,
            exact_duplicate_group_count: 0,
            exact_duplicate_memory_count: 0,
            graph_contradiction_cluster_count: 0,
            graph_support_memory_count: 4,
            graph_max_k: 3,
        };
        let mut duplicated = clean.clone();
        duplicated.exact_duplicate_group_count = 1;
        duplicated.exact_duplicate_memory_count = 2;

        let clean_score = redundancy_sub_score(&clean).score;
        let duplicate_score = redundancy_sub_score(&duplicated).score;

        ensure(clean_score, 100, "clean redundancy score")?;
        if duplicate_score >= clean_score {
            return Err(format!(
                "expected duplicate score below clean score, got {duplicate_score} >= {clean_score}"
            ));
        }
        Ok(())
    }

    #[test]
    fn health_scorecard_trend_declines_from_prior_debt_snapshot() -> TestResult {
        let snapshot = StoredDebtSnapshot {
            workspace_id: "wsp_scorecard_trend".to_owned(),
            snapshot_day: "2026-06-17".to_owned(),
            generation: 7,
            report_hash: "blake3:prior".to_owned(),
            report_json: serde_json::json!({
                "schema": "ee.curate.doctor.v1",
                "summary": {
                    "memoryCount": 10,
                    "itemCount": 1,
                    "totalScore": 0.2
                }
            })
            .to_string(),
            item_count: 1,
            total_score: 0.2,
            created_at: "2026-06-17T00:00:00Z".to_owned(),
        };

        let trend = health_scorecard_trend(70, &[snapshot]);

        ensure(trend.direction.as_str(), "declining", "declining trend")?;
        ensure(trend.current_score, 70, "current score")?;
        if trend.previous_score.unwrap_or(0) <= trend.current_score {
            return Err(format!(
                "expected previous score above current score, got {:?} and {}",
                trend.previous_score, trend.current_score
            ));
        }
        Ok(())
    }

    #[test]
    fn health_scorecard_top_actions_rank_weighted_impact() -> TestResult {
        let evidence = HealthScorecardEvidence {
            memory_count: 6,
            query_miss_count: 8,
            gap_count: 3,
            debt_item_count: 3,
            stale_anchor_count: 1,
            low_trust_high_rank_count: 1,
            orphan_count: 0,
            never_retrieved_count: 1,
            missing_provenance_count: 1,
            unverified_provenance_count: 2,
            mismatched_provenance_count: 0,
            low_trust_count: 2,
            exact_duplicate_group_count: 1,
            exact_duplicate_memory_count: 2,
            graph_contradiction_cluster_count: 0,
            graph_support_memory_count: 4,
            graph_max_k: 3,
        };
        let sub_scores = vec![
            sub_score("coverage", 40, 0.25, 4.0, 10.0, "coverage low", vec![]),
            sub_score("trust", 78, 0.20, 4.0, 6.0, "trust low", vec![]),
            sub_score("redundancy", 55, 0.15, 4.0, 6.0, "duplicate low", vec![]),
        ];
        let trend = HealthScorecardTrend {
            direction: "flat".to_owned(),
            delta: 0,
            current_score: 60,
            previous_score: Some(60),
            snapshot_count: 1,
            source: "debt_snapshots".to_owned(),
            latest_snapshot_day: Some("2026-06-17".to_owned()),
            latest_snapshot_hash: Some("blake3:prior".to_owned()),
        };

        let actions = top_scorecard_actions(&sub_scores, &evidence, &trend);

        ensure(actions.len(), 3, "three actions")?;
        ensure(
            actions[0].id.as_str(),
            "health_scorecard.coverage",
            "coverage has highest weighted impact",
        )?;
        ensure(actions[0].rank, 1, "rank one")
    }

    #[test]
    fn health_scorecard_from_parts_counts_duplicate_and_provenance_evidence() -> TestResult {
        let memories = vec![
            stored_memory_fixture(
                "mem_scorecard_a",
                "Run remote verify before closeout.",
                "human_explicit",
                Some("file://AGENTS.md"),
                "verified",
            ),
            stored_memory_fixture(
                "mem_scorecard_b",
                "Run remote verify before closeout.",
                "agent_assertion",
                None,
                "pending",
            ),
        ];

        let report = HealthScorecardReport::from_parts(
            Path::new("/tmp/ee-health-scorecard-fixture"),
            Path::new("/tmp/ee-health-scorecard-fixture/.ee/ee.db"),
            "wsp_scorecard_fixture".to_owned(),
            memories,
            None,
            None,
            structural_health_ok_fixture(),
            Vec::new(),
            HealthScorecardSnapshotSummary::not_recorded(false, 0),
            Vec::new(),
        );

        ensure(report.evidence.memory_count, 2, "memory count")?;
        ensure(
            report.evidence.exact_duplicate_group_count,
            1,
            "duplicate group count",
        )?;
        ensure(
            report.evidence.missing_provenance_count,
            1,
            "missing provenance count",
        )?;
        ensure(
            report.evidence.unverified_provenance_count,
            1,
            "unverified provenance count",
        )
    }

    fn insert_test_memory(
        connection: &DbConnection,
        workspace_id: &str,
        memory_id: &str,
    ) -> TestResult {
        connection
            .insert_memory(
                memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("fixture {memory_id}"),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn structural_health_ok_fixture() -> StructuralHealthReport {
        StructuralHealthReport {
            schema: HEALTH_STRUCTURAL_SCHEMA_V1,
            snapshot_version: 1,
            k_truss: StructuralKTrussSummary {
                max_k: 3,
                support_subgraph_memory_count: 3,
                top_members: Vec::new(),
            },
            contradiction_clusters: Vec::new(),
            summary: StructuralHealthSummary {
                status: "ok".to_owned(),
                k_truss_max_k: 3,
                support_subgraph_memory_count: 3,
                contradiction_cluster_count: 0,
                recommended_command: "ee curate candidates --workspace . --json".to_owned(),
            },
            degraded: Vec::new(),
        }
    }

    fn stored_memory_fixture(
        id: &str,
        content: &str,
        trust_class: &str,
        provenance_uri: Option<&str>,
        provenance_verification_status: &str,
    ) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "wsp_scorecard_fixture".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.7,
            importance: 0.6,
            provenance_uri: provenance_uri.map(str::to_owned),
            trust_class: trust_class.to_owned(),
            trust_subclass: None,
            provenance_chain_hash: Some("blake3:fixture".to_owned()),
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: provenance_verification_status.to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-06-17T00:00:00Z".to_owned(),
            updated_at: "2026-06-17T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    fn insert_test_link(
        connection: &DbConnection,
        link_id: &str,
        src_memory_id: &str,
        dst_memory_id: &str,
        relation: MemoryLinkRelation,
    ) -> TestResult {
        insert_test_link_with_metadata(
            connection,
            link_id,
            src_memory_id,
            dst_memory_id,
            relation,
            None,
        )
    }

    fn insert_test_link_with_metadata(
        connection: &DbConnection,
        link_id: &str,
        src_memory_id: &str,
        dst_memory_id: &str,
        relation: MemoryLinkRelation,
        metadata_json: Option<String>,
    ) -> TestResult {
        connection
            .insert_memory_link(
                link_id,
                &CreateMemoryLinkInput {
                    src_memory_id: src_memory_id.to_owned(),
                    dst_memory_id: dst_memory_id.to_owned(),
                    relation,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("health-test".to_owned()),
                    metadata_json,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn health_denied_mesh_link_metadata() -> String {
        serde_json::json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_health_denied",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_health_decision_denied",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }
}
