//! Mutating apply-mode entry point for the shard fan-out migration owned by
//! `bd-f6jfs.4`. This module wraps the existing planner and source-preservation
//! helpers in [`crate::db::shard`] to provide a single deterministic call site
//! the CLI invokes when running `ee migrate shard-fanout` without `--dry-run`.
//!
//! Per the bead's scope and `AGENTS.md`, the apply path:
//!
//! * never deletes the legacy database — preservation is by rename/copy via
//!   [`shard::preserve_shard_fanout_source_database`];
//! * refuses to advance when the planner reported blockers, returning an
//!   `Outcome::Blocked` report rather than partially mutating state;
//! * detects an already-applied state by comparing the preserved-source hash
//!   to the source database hash recorded in the plan, so a second invocation
//!   reports `Outcome::AlreadyApplied` instead of double-preserving rows or
//!   audit entries;
//! * leaves the actual per-table row copy (`copy_workspace_to_shard` events)
//!   stubbed behind a `shard_fanout_row_copy_unimplemented` degraded code,
//!   because the cross-table copy spans memory/workspace/audit/pack/graph/cache
//!   surfaces that warrant their own focused slice with RCH proofs.
//!
//! Down-stream beads (`bd-f6jfs.7`, `bd-f6jfs.9`) consume the structured
//! report shape defined here so backup/restore and rollback paths can compose
//! against a single source of truth for "what did the migration actually do".

use std::path::PathBuf;

use serde::Serialize;

use crate::db::shard::{
    self, ShardFanoutMigrationPlan, ShardFanoutMigrationWorkspacePlan,
    ShardFanoutPreserveSourceError, ShardFanoutPreservedSourceReport,
};
use crate::models::DomainError;

/// Schema id for the apply-mode report. Distinct from the planner schema so
/// CLI consumers can discriminate dry-run output from a mutating-apply outcome.
pub const SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1: &str =
    "ee.migration.shard_fanout.apply.v1";

/// Degraded code emitted while the row-copy slice (memory/workspace/audit/
/// pack/graph/cache row migration) is still owned by a follow-up slice.
/// The presence of this code in `degraded[]` is the signal to downstream
/// consumers that source preservation succeeded but per-workspace shards
/// have not yet been populated.
pub const SHARD_FANOUT_ROW_COPY_UNIMPLEMENTED_CODE: &str = "shard_fanout_row_copy_unimplemented";

/// Degraded code emitted when the planner reported blockers and the apply
/// path refuses to advance. The structured blockers are echoed into the
/// report's `degraded` array via `ShardFanoutMigrationApplyDegradation`.
pub const SHARD_FANOUT_APPLY_PLAN_BLOCKED_CODE: &str = "shard_fanout_apply_plan_blocked";

/// Outcome classification used by the apply report and persisted into the
/// `localBuildPolicy`-style structured-error surface so support bundles and
/// completion-audit consumers can describe the migration without re-running
/// it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardFanoutMigrationOutcome {
    /// Apply succeeded for everything the current slice covers: source was
    /// preserved and per-workspace shard paths were resolved.
    Applied,
    /// A prior apply already produced the preserved-source artifact with a
    /// matching hash; nothing was mutated this call.
    AlreadyApplied,
    /// The planner emitted blockers or the source database was unreachable;
    /// the apply path refused to advance and reported the blockers verbatim.
    Blocked,
}

impl ShardFanoutMigrationOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::AlreadyApplied => "already_applied",
            Self::Blocked => "blocked",
        }
    }
}

/// Per-workspace summary returned by the apply path. Mirrors the planner
/// row counts but drops blocker text (echoed at report level) so consumers
/// can iterate by workspace without re-walking the planner output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationWorkspaceOutcome {
    pub workspace_id: String,
    pub workspace_root: PathBuf,
    pub shard_id: Option<String>,
    pub shard_path: Option<PathBuf>,
    pub row_copy_status: &'static str,
    pub planned_row_count: Option<u64>,
    pub source_hash: Option<String>,
    pub blocker_count: usize,
}

impl ShardFanoutMigrationWorkspaceOutcome {
    fn from_plan(plan: &ShardFanoutMigrationWorkspacePlan, row_copy_status: &'static str) -> Self {
        Self {
            workspace_id: plan.workspace_id.clone(),
            workspace_root: plan.workspace_root.clone(),
            shard_id: plan.shard_id.clone(),
            shard_path: plan.shard_path.clone(),
            row_copy_status,
            planned_row_count: plan.planned_row_count,
            source_hash: plan.source_hash.clone(),
            blocker_count: plan.blockers.len(),
        }
    }
}

/// Structured degradation echoed into the apply report. Keeps the planner's
/// stable codes verbatim and lets callers distinguish planner blockers from
/// stubbed row-copy work without re-parsing message text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationApplyDegradation {
    pub code: String,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

/// Apply-mode report. Stable, redaction-safe shape. The `preservedSource`
/// field is `Some(..)` whenever the apply path actually invoked the source
/// preservation helper; on `Blocked` outcomes it is `None`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardFanoutMigrationApplyReport {
    pub schema: &'static str,
    pub outcome: ShardFanoutMigrationOutcome,
    pub plan_schema: &'static str,
    pub source_database_path: PathBuf,
    pub preserved_source_database_path: PathBuf,
    pub source_database_hash: Option<String>,
    pub shard_root: Option<PathBuf>,
    pub catalog_path: Option<PathBuf>,
    pub workspaces: Vec<ShardFanoutMigrationWorkspaceOutcome>,
    pub preserved_source: Option<ShardFanoutPreservedSourceReport>,
    pub degraded: Vec<ShardFanoutMigrationApplyDegradation>,
}

impl ShardFanoutMigrationApplyReport {
    /// Identifier for the row-copy slice that still needs to land in a
    /// follow-up bead. Exposed so downstream tests can assert that this slice
    /// did NOT silently claim to migrate per-table rows.
    #[must_use]
    pub fn row_copy_unimplemented(&self) -> bool {
        self.degraded
            .iter()
            .any(|entry| entry.code == SHARD_FANOUT_ROW_COPY_UNIMPLEMENTED_CODE)
    }
}

/// Apply the shard fan-out migration described by `plan`.
///
/// The plan MUST come from [`shard::plan_shard_fanout_migration`] so the apply
/// path can preserve the planner's source hash and audit row pre-computation
/// without recomputing them. Per `AGENTS.md` and the bead acceptance:
///
/// * if the plan carries blockers, the apply path refuses to advance and
///   returns an `Outcome::Blocked` report with the blockers verbatim;
/// * otherwise the existing source-preservation helper runs and its outcome is
///   surfaced in `preserved_source`. If preservation indicates the legacy
///   database had already been preserved (i.e. the rename/copy was a no-op
///   against a matching hash), the report returns `Outcome::AlreadyApplied`;
/// * in either successful case the per-workspace `row_copy_status` is set to
///   `pending_row_copy_implementation` and the `shard_fanout_row_copy_unimplemented`
///   degraded code is emitted, because the cross-table row copy is owned by a
///   focused follow-up slice. This avoids silently claiming the migration moved
///   rows when it did not.
pub fn apply_shard_fanout_migration(
    plan: &ShardFanoutMigrationPlan,
) -> Result<ShardFanoutMigrationApplyReport, DomainError> {
    if !plan.blockers.is_empty() {
        let degraded = plan
            .blockers
            .iter()
            .map(|blocker| ShardFanoutMigrationApplyDegradation {
                code: blocker.code.to_owned(),
                severity: blocker.severity,
                message: blocker.message.to_owned(),
                repair: if blocker.repair.is_empty() {
                    None
                } else {
                    Some(blocker.repair.to_owned())
                },
            })
            .chain(std::iter::once(ShardFanoutMigrationApplyDegradation {
                code: SHARD_FANOUT_APPLY_PLAN_BLOCKED_CODE.to_owned(),
                severity: "high",
                message: "Refusing to apply shard fan-out migration: planner reported blockers."
                    .to_owned(),
                repair: Some(
                    "Resolve the listed blockers and rerun `ee migrate shard-fanout --dry-run --json` before retrying."
                        .to_owned(),
                ),
            }))
            .collect::<Vec<_>>();

        return Ok(ShardFanoutMigrationApplyReport {
            schema: SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1,
            outcome: ShardFanoutMigrationOutcome::Blocked,
            plan_schema: plan.schema,
            source_database_path: plan.source_database_path.clone(),
            preserved_source_database_path: plan.preserved_source_database_path.clone(),
            source_database_hash: plan.source_database_hash.clone(),
            shard_root: plan.shard_root.clone(),
            catalog_path: plan.catalog_path.clone(),
            workspaces: plan
                .workspaces
                .iter()
                .map(|workspace| {
                    ShardFanoutMigrationWorkspaceOutcome::from_plan(workspace, "blocked_by_plan")
                })
                .collect(),
            preserved_source: None,
            degraded,
        });
    }

    let preserved_source = match shard::preserve_shard_fanout_source_database(plan) {
        Ok(report) => report,
        Err(error) => {
            return Err(domain_error_for_preserve_failure(plan, &error));
        }
    };

    let outcome = if preserved_source.copied {
        ShardFanoutMigrationOutcome::Applied
    } else {
        ShardFanoutMigrationOutcome::AlreadyApplied
    };

    let degraded = vec![ShardFanoutMigrationApplyDegradation {
        code: SHARD_FANOUT_ROW_COPY_UNIMPLEMENTED_CODE.to_owned(),
        severity: "warning",
        message: "Source preservation completed; per-workspace shard row copy is owned by a follow-up slice."
            .to_owned(),
        repair: Some(
            "Track follow-up bead bd-f6jfs.4 for memory/workspace/audit/pack/graph/cache row copy implementation and idempotence proof."
                .to_owned(),
        ),
    }];

    Ok(ShardFanoutMigrationApplyReport {
        schema: SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1,
        outcome,
        plan_schema: plan.schema,
        source_database_path: plan.source_database_path.clone(),
        preserved_source_database_path: plan.preserved_source_database_path.clone(),
        source_database_hash: plan.source_database_hash.clone(),
        shard_root: plan.shard_root.clone(),
        catalog_path: plan.catalog_path.clone(),
        workspaces: plan
            .workspaces
            .iter()
            .map(|workspace| {
                ShardFanoutMigrationWorkspaceOutcome::from_plan(
                    workspace,
                    "pending_row_copy_implementation",
                )
            })
            .collect(),
        preserved_source: Some(preserved_source),
        degraded,
    })
}

fn domain_error_for_preserve_failure(
    plan: &ShardFanoutMigrationPlan,
    error: &ShardFanoutPreserveSourceError,
) -> DomainError {
    DomainError::Storage {
        message: format!(
            "shard fan-out migration preserve-source step failed for {}: {}",
            plan.source_database_path.display(),
            error
        ),
        repair: Some(
            "Inspect the legacy database file permissions and disk space, then rerun `ee migrate shard-fanout --dry-run --json` before retrying the apply path."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::shard::{
        SHARD_FANOUT_CATALOG_SCHEMA_VERSION, SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
        SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1, ShardFanoutDegradation,
    };

    fn blocking_plan() -> ShardFanoutMigrationPlan {
        ShardFanoutMigrationPlan {
            schema: SHARD_FANOUT_MIGRATION_PLAN_SCHEMA_V1,
            dry_run: false,
            source_database_path: PathBuf::from("/tmp/ee-test-source.db"),
            preserved_source_database_path: PathBuf::from(
                "/tmp/ee-test-source.pre-shard-fanout.db",
            ),
            source_database_hash: None,
            shard_root: None,
            catalog_path: None,
            catalog_schema_version: SHARD_FANOUT_CATALOG_SCHEMA_VERSION,
            workspaces: Vec::new(),
            expected_audit_rows: vec![shard::ShardFanoutMigrationAuditRowPlan {
                schema: SHARD_FANOUT_MIGRATION_AUDIT_SCHEMA_V1,
                event: "preserve_legacy_database",
                workspace_id: None,
                source_path: PathBuf::from("/tmp/ee-test-source.db"),
                target_path: PathBuf::from("/tmp/ee-test-source.pre-shard-fanout.db"),
            }],
            blockers: vec![ShardFanoutDegradation {
                code: "shards_dir_unresolved",
                severity: "warning",
                message: "shards directory could not be resolved from configuration",
                repair: "set EE_SHARDS_DIR or pass --shards-dir",
            }],
        }
    }

    #[test]
    fn apply_refuses_to_advance_when_plan_blockers_present() {
        let plan = blocking_plan();
        let report =
            apply_shard_fanout_migration(&plan).expect("apply should succeed structurally");

        assert_eq!(report.schema, SHARD_FANOUT_MIGRATION_APPLY_REPORT_SCHEMA_V1);
        assert_eq!(report.outcome, ShardFanoutMigrationOutcome::Blocked);
        assert!(report.preserved_source.is_none());
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "shards_dir_unresolved")
        );
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == SHARD_FANOUT_APPLY_PLAN_BLOCKED_CODE)
        );
        assert!(
            !report.row_copy_unimplemented(),
            "blocked outcome must not emit the row-copy-unimplemented code: that only \
             applies after source preservation actually ran"
        );
    }

    #[test]
    fn shard_fanout_migration_outcome_serializes_to_stable_snake_case() {
        assert_eq!(ShardFanoutMigrationOutcome::Applied.as_str(), "applied");
        assert_eq!(
            ShardFanoutMigrationOutcome::AlreadyApplied.as_str(),
            "already_applied"
        );
        assert_eq!(ShardFanoutMigrationOutcome::Blocked.as_str(), "blocked");

        let json = serde_json::to_value(ShardFanoutMigrationOutcome::Applied).expect("serialize");
        assert_eq!(json, serde_json::json!("applied"));
    }
}
