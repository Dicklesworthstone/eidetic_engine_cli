//! Health command handler (EE-026).
//!
//! Provides a quick health check summary with an overall healthy/unhealthy
//! verdict and counts of issues by severity. Simpler than doctor (no fix plans),
//! more binary than status (which reports detailed readiness states).

use std::collections::BTreeSet;
use std::path::Path;

use fnx_classes::Graph;
use fnx_runtime::CompatibilityMode;
use serde::Serialize;

use crate::db::{DbConnection, GraphSnapshotType, MemoryLinkRelation, StoredMemoryLink};
use crate::graph::health::{
    ContradictionCluster, ContradictionSeverity, HEALTH_STRUCTURAL_SCHEMA_V1, KTrussReport,
    compute_k_truss, detect_contradiction_clusters,
};
use crate::models::CapabilityStatus;
use crate::models::degradation::{GRAPH_HEALTH_NO_CONTRADICTIONS_CODE, GRAPH_METRICS_UNAVAILABLE};

use super::build_info;
use super::curate::stable_workspace_id;
use super::status::{
    default_workspace_path, probe_runtime_capability, probe_search_capability,
    probe_storage_capability,
};

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
            return Self::empty_with_degradation(storage_not_ready_degradation());
        };

        let database_path = workspace_path.join(".ee").join("ee.db");
        if !database_path.exists() {
            return Self::empty_with_degradation(storage_not_ready_degradation());
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

fn storage_not_ready_degradation() -> StructuralHealthDegradation {
    StructuralHealthDegradation {
        code: "storage_not_ready".to_owned(),
        severity: "medium".to_owned(),
        message: "Workspace storage has not been initialized.".to_owned(),
        repair: Some("ee init --workspace .".to_owned()),
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
    let workspace_key = workspace_path.to_string_lossy().to_string();
    if let Some(workspace) = connection
        .get_workspace_by_path(&workspace_key)
        .ok()
        .flatten()
    {
        candidates.insert(workspace.id);
    }
    candidates.insert(stable_workspace_id(workspace_path));

    if let Ok(canonical) = workspace_path.canonicalize() {
        let canonical_key = canonical.to_string_lossy().to_string();
        if let Some(workspace) = connection
            .get_workspace_by_path(&canonical_key)
            .ok()
            .flatten()
        {
            candidates.insert(workspace.id);
        }
        candidates.insert(stable_workspace_id(&canonical));
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
