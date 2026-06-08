//! bd-1n0np.7.3 — `ee conflict list|explain|cluster` read-only contradiction surface.
//!
//! Thin CLI layer over
//! [`crate::core::contradiction_detect::assemble_conflict_surface`] (which reuses
//! the 7.2 gather + explicit-evidence detector). Read-only: opens the workspace
//! DB, assembles the ranked conflicting pairs + clusters, projects per subcommand,
//! and renders the `ee.response.v2` envelope around the `ee.conflict.v1` surface
//! (or a compact human summary). Never mutates.

use std::path::Path;

use clap::{Args, Subcommand};

use crate::core::contradiction_detect::{
    ConflictSurface, ContradictionDetectionConfig, assemble_conflict_surface,
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
    use super::{ConflictCommand, ConflictListArgs, render_conflict_human, truncate_body};
    use crate::core::contradiction_detect::ConflictSurface;

    fn empty_surface() -> ConflictSurface {
        ConflictSurface {
            schema: "ee.conflict.v1",
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
}
