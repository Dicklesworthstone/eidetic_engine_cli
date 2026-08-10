//! bd-1n0np.7.3 — `ee conflict list|explain|cluster` read-only contradiction surface.
//!
//! Thin CLI layer over
//! [`crate::core::contradiction_detect::assemble_conflict_surface`] (which reuses
//! the 7.2 gather + explicit-evidence detector). list|explain|cluster are
//! read-only. `resolve` (bd-3a1op.4, ADR 0066) plans via the pure
//! [`crate::core::contradiction_detect::plan_conflict_resolution`] engine and,
//! only under `--apply`, executes the plan through EXISTING audited core atoms
//! (`decide_record`, `expire_memory`, `update_memory_link`,
//! `update_memory_tags`) — no novel mutation paths.

use std::path::Path;

use clap::{Args, Subcommand};

use crate::core::contradiction_detect::{
    ConflictResolutionPlan, ConflictSurface, ContradictionDetectionConfig, PlannedResolutionAction,
    assemble_conflict_surface,
};
use crate::db::DbConnection;
use crate::models::{DomainError, RESPONSE_SCHEMA_V2};

/// Subcommands for `ee conflict` (read-only contradiction surfacing).
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ConflictCommand {
    /// List ranked conflicting memory pairs with both bodies + the preferred side.
    List(ConflictListArgs),
    /// Explain the conflicts implicating a specific memory id.
    Explain(ConflictExplainArgs),
    /// List detected contradiction clusters (k-truss + Louvain).
    Cluster(ConflictClusterArgs),
    /// Resolve one conflicting pair through audited mutations (dry-run default).
    Resolve(ConflictResolveArgs),
}

/// `ee conflict resolve <MEMORY_A> <MEMORY_B> --verb ...` (bd-3a1op.4).
#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ConflictResolveArgs {
    /// One side of the conflicting pair (order-independent).
    #[arg(value_name = "MEMORY_A")]
    pub memory_a: String,
    /// The other side of the conflicting pair.
    #[arg(value_name = "MEMORY_B")]
    pub memory_b: String,
    /// Resolution verb: supersede | reject-one | scope-split | both-valid.
    #[arg(long, value_name = "VERB")]
    pub verb: String,
    /// Surviving memory id (required by supersede and reject-one).
    #[arg(long, value_name = "MEMORY_ID")]
    pub keep: Option<String>,
    /// Rationale persisted as the decision memory's rationale.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
    /// scope-split only: comma-separated tags scoping memory A.
    #[arg(long, value_name = "TAGS")]
    pub scope_a_tags: Option<String>,
    /// scope-split only: comma-separated tags scoping memory B.
    #[arg(long, value_name = "TAGS")]
    pub scope_b_tags: Option<String>,
    /// Execute the plan. Without this flag the command is a dry-run report.
    #[arg(long)]
    pub apply: bool,
    /// Actor recorded in audit rows.
    #[arg(long, value_name = "ACTOR")]
    pub actor: Option<String>,
}

/// Per-atom execution evidence: every applied mutation names its audit trail.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionActionResult {
    pub action: String,
    pub audit_ids: Vec<String>,
    pub created_memory_id: Option<String>,
}

/// Execute an approved plan through the existing audited core atoms,
/// fail-fast, returning per-atom audit evidence.
pub fn execute_conflict_resolution(
    workspace: &Path,
    database_path: &Path,
    plan: &ConflictResolutionPlan,
    reason: &str,
    actor: Option<&str>,
) -> Result<Vec<ResolutionActionResult>, DomainError> {
    use crate::core::decide::{DecideRecordOptions, decide_record};
    use crate::core::memory::{
        ExpireMemoryOptions, MemoryLinkMode, MemoryLinkOptions, MemoryTagsMode, MemoryTagsOptions,
        expire_memory, update_memory_link, update_memory_tags,
    };
    use crate::db::{MemoryLinkRelation, MemoryLinkSource};

    let mut results = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let result = match action {
            PlannedResolutionAction::RecordDecision {
                topic,
                chosen,
                alternatives,
                supersedes,
            } => {
                let report = decide_record(&DecideRecordOptions {
                    workspace_path: workspace,
                    database_path: Some(database_path),
                    topic,
                    chosen,
                    alternatives: alternatives.clone(),
                    rationale: reason,
                    revisit_by: None,
                    supersedes: supersedes.as_deref(),
                    dry_run: false,
                    actor,
                    now: None,
                })?;
                ResolutionActionResult {
                    action: "recordDecision".to_owned(),
                    audit_ids: [
                        report.memory_audit_id.clone(),
                        report.link_audit_id.clone(),
                        report.expire_audit_id.clone(),
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                    created_memory_id: Some(report.decision.memory_id.clone()),
                }
            }
            PlannedResolutionAction::ExpireMemory { memory_id, reason } => {
                let report = expire_memory(&ExpireMemoryOptions {
                    workspace_path: workspace,
                    database_path,
                    memory_id,
                    reason: Some(reason),
                    actor,
                    dry_run: false,
                    include_tombstoned: false,
                })?;
                ResolutionActionResult {
                    action: "expireMemory".to_owned(),
                    audit_ids: report.audit_id.into_iter().collect(),
                    created_memory_id: None,
                }
            }
            PlannedResolutionAction::CreateLink { from, to, relation } => {
                let relation =
                    MemoryLinkRelation::parse(relation).ok_or_else(|| DomainError::Usage {
                        message: format!("Unknown link relation {relation}."),
                        repair: None,
                    })?;
                let report = update_memory_link(&MemoryLinkOptions {
                    workspace_path: workspace,
                    database_path,
                    memory_id: from,
                    mode: MemoryLinkMode::Create {
                        target_memory_id: to.clone(),
                        relation,
                        weight: 1.0,
                        confidence: 1.0,
                        directed: false,
                        evidence_count: 0,
                        source: MemoryLinkSource::Agent,
                        metadata_json: Some(
                            serde_json::json!({
                                "resolution": "both_valid",
                                "conflictId": plan.conflict_id,
                            })
                            .to_string(),
                        ),
                    },
                    actor,
                    dry_run: false,
                    include_tombstoned: false,
                })?;
                ResolutionActionResult {
                    action: "createLink".to_owned(),
                    audit_ids: report.audit_id.into_iter().collect(),
                    created_memory_id: None,
                }
            }
            PlannedResolutionAction::AddTags { memory_id, tags } => {
                let report = update_memory_tags(&MemoryTagsOptions {
                    workspace_path: workspace,
                    database_path,
                    memory_id,
                    mode: MemoryTagsMode::Patch {
                        add: tags.clone(),
                        remove: Vec::new(),
                    },
                    actor,
                    dry_run: false,
                    include_tombstoned: false,
                })?;
                ResolutionActionResult {
                    action: "addTags".to_owned(),
                    audit_ids: report.audit_ids,
                    created_memory_id: None,
                }
            }
        };
        results.push(result);
    }
    Ok(results)
}

/// `ee conflict list`
#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ConflictListArgs {}

/// `ee conflict explain <MEMORY_ID>`
#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ConflictExplainArgs {
    /// Memory id whose conflicts should be explained.
    #[arg(value_name = "MEMORY_ID")]
    pub memory_id: String,
}

/// `ee conflict cluster`
#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ConflictClusterArgs {}

fn open_workspace_db(workspace: &Path) -> Result<DbConnection, DomainError> {
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("No workspace database at {}.", database_path.display()),
            repair: Some("Run `ee init --workspace . --json` first.".to_owned()),
        });
    }
    DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
        message: format!("Failed to open workspace database: {error}"),
        repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
    })
}

/// Build the full read-only conflict surface for a workspace.
pub fn build_conflict_surface(workspace: &Path) -> Result<ConflictSurface, DomainError> {
    let connection = open_workspace_db(workspace)?;
    Ok(assemble_conflict_surface(
        &connection,
        ContradictionDetectionConfig::default(),
    ))
}

/// Build the surface filtered to the conflicts implicating one memory
/// (`ee conflict explain <memory_id>`).
pub fn build_conflict_surface_for_memory(
    workspace: &Path,
    memory_id: &str,
) -> Result<ConflictSurface, DomainError> {
    Ok(build_conflict_surface(workspace)?.focused_on(memory_id))
}

/// Render the `ee.response.v2` envelope wrapping the `ee.conflict.v1` surface.
/// The same stable data schema is emitted for list/explain/cluster (explain is
/// pre-filtered); subcommands differ in the human rendering only.
#[must_use]
pub fn render_conflict_json(surface: &ConflictSurface) -> String {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V2,
        "success": true,
        "data": surface,
        "degraded": [],
    })
    .to_string()
}

fn truncate_body(content: &str) -> String {
    const MAX: usize = 72;
    let oneline = content.replace('\n', " ");
    if oneline.chars().count() <= MAX {
        oneline
    } else {
        let kept: String = oneline.chars().take(MAX).collect();
        format!("{kept}…")
    }
}

/// Compact human-readable summary, emphasizing pairs (list/explain) or clusters.
#[must_use]
pub fn render_conflict_human(surface: &ConflictSurface, command: &ConflictCommand) -> String {
    let mut out = String::new();
    match command {
        ConflictCommand::Cluster(_) => {
            out.push_str(&format!(
                "Contradiction clusters: {}\n",
                surface.clusters.len()
            ));
            for cluster in &surface.clusters {
                out.push_str(&format!(
                    "  - cluster {} ({:?}, size {}): centrality {}, load-bearing {}m, score {:.3}\n",
                    cluster.louvain_id,
                    cluster.severity,
                    cluster.size,
                    cluster.centrality,
                    cluster.load_bearing_milli,
                    cluster.rank_score,
                ));
            }
        }
        _ => {
            out.push_str(&format!("Conflicting pairs: {}\n", surface.pairs.len()));
            for pair in &surface.pairs {
                out.push_str(&format!(
                    "  - {} [{}] prefers side {} ({})\n      A {}: {}\n      B {}: {}\n",
                    pair.conflict_id,
                    pair.signal,
                    pair.preferred_side,
                    pair.preferred_reason,
                    pair.memory_a.id,
                    truncate_body(&pair.memory_a.content),
                    pair.memory_b.id,
                    truncate_body(&pair.memory_b.content),
                ));
            }
        }
    }
    if !surface.deferred_signals.is_empty() {
        out.push_str(&format!(
            "Deferred signal kinds (not yet gathered): {}\n",
            surface.deferred_signals.join(", ")
        ));
    }
    if !surface.degraded.is_empty() {
        out.push_str(&format!("Degraded: {}\n", surface.degraded.join("; ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        ConflictCommand, ConflictListArgs, render_conflict_human, render_conflict_json,
        truncate_body,
    };
    use crate::core::contradiction_detect::{CONFLICT_SURFACE_SCHEMA_V1, ConflictSurface};

    fn empty_surface() -> ConflictSurface {
        ConflictSurface {
            schema: CONFLICT_SURFACE_SCHEMA_V1,
            pairs: Vec::new(),
            clusters: Vec::new(),
            explicit_edge_count: 0,
            gathered_signals: vec!["contradiction_link".to_owned()],
            deferred_signals: vec!["validity_window_overlap".to_owned()],
            fuzzy_near_conflict_skipped: false,
            degraded: Vec::new(),
        }
    }

    #[test]
    fn human_render_reports_zero_pairs_and_deferred_kinds() {
        let surface = empty_surface();
        let text = render_conflict_human(&surface, &ConflictCommand::List(ConflictListArgs {}));
        assert!(text.contains("Conflicting pairs: 0"));
        // No-silent-cap: deferred signal kinds are visible even with no pairs.
        assert!(text.contains("validity_window_overlap"));
    }

    #[test]
    fn truncate_body_collapses_newlines_and_caps_length() {
        let body = "line one\nline two";
        assert_eq!(truncate_body(body), "line one line two");
        let long = "x".repeat(200);
        let truncated = truncate_body(&long);
        assert!(truncated.chars().count() <= 73, "capped with ellipsis");
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn conflict_json_envelope_includes_clean_degraded_array() {
        let surface = empty_surface();
        let raw = render_conflict_json(&surface);
        let envelope: serde_json::Value =
            serde_json::from_str(&raw).expect("conflict json envelope");

        assert_eq!(envelope["schema"], crate::models::RESPONSE_SCHEMA_V2);
        assert_eq!(envelope["success"], true);
        assert_eq!(envelope["data"]["schema"], CONFLICT_SURFACE_SCHEMA_V1);
        assert_eq!(envelope["degraded"], serde_json::json!([]));
    }
}
