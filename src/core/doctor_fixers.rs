//! 12 auto-fixable fixers wired through the `doctor_runtime::mutate()`
//! chokepoint (bd-tu4s8 Pass-2). Each fixer maps a specific repair-spec
//! finding code to the `Op` that the doctor should call `mutate()` with.
//!
//! Phase-1 scope: the fixers are pure dispatchers — they return the
//! `(path, Op)` pair the caller will hand to `mutate()`. They do NOT call
//! `mutate()` themselves so the test surface stays free of `RunContext`
//! setup. Subsystem-actor wiring for the `RunX` family of variants lands
//! in the bd-3boan CLI-wiring slice and follow-up actor beads; until those
//! land, `RunX` Ops record their planned-mutation evidence through the same
//! `actions.jsonl` channel as `Manual{steps}`.
//!
//! The 12 fixers cover the eight repair_specs/ subsystems
//! (agent_coordination, cass_integration, graph_subsystem, policy_safety,
//! schema_migrations, search_indexes, state_files, workspace_config) plus
//! WAL checkpoint and snapshot-backup as cross-cutting Ops. Each fixer
//! references the stable finding code agents already emit through
//! `Op::EmitDiagnostic { code, severity }` so the detector and fixer
//! layers stay symmetric.

use std::path::{Path, PathBuf};

use super::doctor_runtime::Op;

/// One auto-fix dispatch: the path the fix targets plus the [`Op`] the
/// caller should hand to `mutate()`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixerDispatch {
    /// Stable finding code the detector emitted.
    pub finding_code: &'static str,
    /// Doctor severity (matches the detector's emission).
    pub severity: &'static str,
    /// Target path for `mutate(ctx, path, op)`.
    pub path: PathBuf,
    /// The `Op` to dispatch through the mutate chokepoint.
    pub op: Op,
}

impl FixerDispatch {
    fn manual(
        finding_code: &'static str,
        severity: &'static str,
        path: impl Into<PathBuf>,
        steps: &[&str],
    ) -> Self {
        Self {
            finding_code,
            severity,
            path: path.into(),
            op: Op::Manual {
                steps: steps.iter().map(|step| (*step).to_string()).collect(),
            },
        }
    }
}

/// FM-SI-01: search index manifest is stale relative to the underlying
/// memories table. Auto-fix dispatches an index rebuild plan; the actor
/// handle materialises the rebuild in a follow-up subsystem-wiring bead.
#[must_use]
pub fn fix_search_index_stale(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch {
        finding_code: "search_index_stale",
        severity: "warning",
        path: workspace_root.join(".ee/indexes"),
        op: Op::RunIndexRebuild {
            steps: vec![
                "ee search rebuild --workspace .".to_string(),
                "Confirm `manifest.last_built_at` advances past the latest memories.updated_at."
                    .to_string(),
            ],
        },
    }
}

/// FM-GS-01: graph snapshot is stale relative to memory_links activity.
/// Auto-fix dispatches a graph snapshot refresh plan.
#[must_use]
pub fn fix_graph_snapshot_stale(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch {
        finding_code: "graph_snapshot_stale",
        severity: "warning",
        path: workspace_root.join(".ee/graph"),
        op: Op::RunGraphRefresh {
            steps: vec![
                "ee graph centrality-refresh --workspace .".to_string(),
                "Confirm `graph_snapshot_high_watermark` advances past the latest memory_links.created_at.".to_string(),
            ],
        },
    }
}

/// FM-SF-01: WAL has accumulated frames beyond the configured checkpoint
/// threshold. Auto-fix dispatches a TRUNCATE checkpoint plan.
#[must_use]
pub fn fix_wal_checkpoint_pending(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch {
        finding_code: "wal_checkpoint_pending",
        severity: "warning",
        path: workspace_root.join(".ee/ee.db-wal"),
        op: Op::RunWalCheckpoint {
            mode: "truncate".to_string(),
            steps: vec![
                "ee db checkpoint --mode truncate".to_string(),
                "Verify `.ee/ee.db-wal` shrinks back to the truncation floor.".to_string(),
            ],
        },
    }
}

/// FM-SM-01: schema_migrations table reports a version below the binary's
/// canonical target. Auto-fix dispatches a pending-migration plan; the
/// existing `ee db migrate` command performs the actual mutation under
/// the schema-version actor.
#[must_use]
pub fn fix_schema_migration_pending(
    workspace_root: &Path,
    target_version: impl Into<String>,
) -> FixerDispatch {
    FixerDispatch {
        finding_code: "schema_migration_pending",
        severity: "error",
        path: workspace_root.join(".ee/ee.db"),
        op: Op::RunMigration {
            target_version: target_version.into(),
            steps: vec![
                "ee db migrate".to_string(),
                "Confirm schema_migrations.version matches the binary's MIGRATIONS::TARGET."
                    .to_string(),
            ],
        },
    }
}

/// FM-SF-02: `.beads/issues.jsonl` content has drifted from the canonical
/// SQLite source of truth. Auto-fix dispatches a deterministic JSONL
/// rewrite from the DB.
#[must_use]
pub fn fix_beads_jsonl_drift(workspace_root: &Path, expected_row_count: usize) -> FixerDispatch {
    FixerDispatch {
        finding_code: "beads_jsonl_drift",
        severity: "warning",
        path: workspace_root.join(".beads/issues.jsonl"),
        op: Op::RewriteJsonl {
            row_count: expected_row_count,
            steps: vec![
                "br sync --flush-only".to_string(),
                "Confirm `br doctor` reports `counts.db_vs_jsonl: Both have N records`."
                    .to_string(),
            ],
        },
    }
}

/// FM-WC-04: workspace config TOML is malformed beyond `toml_edit` repair.
/// Auto-fix quarantines the bad file under the run dir so the operator
/// can hand-recover; the doctor never deletes (AGENTS.md RULE 1).
#[must_use]
pub fn fix_workspace_config_malformed_toml(workspace_root: &Path) -> FixerDispatch {
    let path = workspace_root.join(".ee/config.toml");
    FixerDispatch {
        finding_code: "workspace_config_malformed_toml",
        severity: "error",
        path,
        op: Op::QuarantineByRename {
            dest_under_quarantine: PathBuf::from("config.toml"),
        },
    }
}

/// FM-WC-08: workspace config TOML is structurally valid but a doctor
/// repair needs to add or update a key in a format-preserving way. Auto-fix
/// dispatches the atomic rewrite plan; the toml_edit actor performs the
/// mutation in a follow-up wiring bead.
#[must_use]
pub fn fix_workspace_config_atomic_rewrite(workspace_root: &Path, summary: &str) -> FixerDispatch {
    FixerDispatch {
        finding_code: "workspace_config_atomic_rewrite_needed",
        severity: "warning",
        path: workspace_root.join(".ee/config.toml"),
        op: Op::AtomicRewriteToml {
            steps: vec![
                format!("doctor will rewrite config.toml: {summary}"),
                "Verify the resulting file parses under ConfigFile::parse.".to_string(),
            ],
        },
    }
}

/// FM-AC-01: Agent Mail file_reservations row is past its TTL but still
/// holds an exclusive lease. Auto-fix dispatches a quarantine of the
/// reservation marker; the operator must explicitly authorise any
/// repository state mutation that the stale lease was guarding.
#[must_use]
pub fn fix_agent_coordination_stale_lease(
    workspace_root: &Path,
    reservation_marker: &Path,
) -> FixerDispatch {
    let rel = reservation_marker
        .strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| reservation_marker.to_path_buf());
    FixerDispatch {
        finding_code: "agent_coordination_stale_lease",
        severity: "warning",
        path: reservation_marker.to_path_buf(),
        op: Op::QuarantineByRename {
            dest_under_quarantine: rel,
        },
    }
}

/// FM-CI-01: CASS integration cache drift. Auto-fix is advisory because
/// CASS owns its own derived-asset rebuild path; the doctor records
/// guidance in actions.jsonl.
#[must_use]
pub fn fix_cass_integration_drift(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch::manual(
        "cass_integration_drift",
        "warning",
        workspace_root.join(".ee/cass"),
        &[
            "Run `ee cass import --refresh` against the affected workspace.",
            "Confirm `ee cass status` reports cache_high_watermark matching the source corpus.",
        ],
    )
}

/// FM-PS-01: policy_safety rules are out of sync between the project TOML
/// and the running binary's policy registry. Auto-fix is advisory; policy
/// changes require explicit human review per AGENTS.md.
#[must_use]
pub fn fix_policy_safety_inconsistent(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch::manual(
        "policy_safety_inconsistent",
        "error",
        workspace_root.join(".ee/config.toml"),
        &[
            "Diff `.ee/config.toml` `[policy.safety]` block against the binary's policy registry.",
            "Apply the reconciled values manually; doctor never silently mutates policy.",
        ],
    )
}

/// FM-SF-03: a critical-section snapshot backup is owed before further
/// mutation (e.g. before a pending schema migration). Auto-fix dispatches
/// the snapshot-backup plan; the backup writer actor lands in a follow-up
/// slice.
#[must_use]
pub fn fix_snapshot_backup_owed(workspace_root: &Path, label: impl Into<String>) -> FixerDispatch {
    FixerDispatch {
        finding_code: "snapshot_backup_owed",
        severity: "warning",
        path: workspace_root.join(".ee"),
        op: Op::SnapshotBackup {
            label: label.into(),
            steps: vec![
                "ee backup create --label doctor-pre-mutation".to_string(),
                "Verify the backup manifest hash matches the pre-mutation source.".to_string(),
            ],
        },
    }
}

/// FM-SF-04: state file permission drift (a sensitive file is
/// world-readable). Auto-fix dispatches a chmod to the canonical mode.
#[must_use]
pub fn fix_state_file_permission_drift(path: impl Into<PathBuf>) -> FixerDispatch {
    FixerDispatch {
        finding_code: "state_file_permission_drift",
        severity: "warning",
        path: path.into(),
        op: Op::Chmod { mode: 0o600 },
    }
}

/// The closed set of fixer codes this module exposes. Contract tests
/// assert that every entry has a matching `fix_*` function above and that
/// the count matches the bead's "12 auto-fixable" deliverable.
pub const FIXER_FINDING_CODES: &[&str] = &[
    "search_index_stale",
    "graph_snapshot_stale",
    "wal_checkpoint_pending",
    "schema_migration_pending",
    "beads_jsonl_drift",
    "workspace_config_malformed_toml",
    "workspace_config_atomic_rewrite_needed",
    "agent_coordination_stale_lease",
    "cass_integration_drift",
    "policy_safety_inconsistent",
    "snapshot_backup_owed",
    "state_file_permission_drift",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/tmp/doctor-fixers-test")
    }

    #[test]
    fn twelve_fixer_codes_are_registered() {
        assert_eq!(FIXER_FINDING_CODES.len(), 12);
        let mut sorted = FIXER_FINDING_CODES.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            FIXER_FINDING_CODES.len(),
            "codes must be unique"
        );
    }

    #[test]
    fn search_index_stale_dispatches_run_index_rebuild() {
        let dispatch = fix_search_index_stale(&root());
        assert_eq!(dispatch.finding_code, "search_index_stale");
        assert!(matches!(dispatch.op, Op::RunIndexRebuild { .. }));
        assert_eq!(dispatch.op.kind_str(), "run_index_rebuild");
        assert!(dispatch.op.is_advisory());
        assert!(!dispatch.op.is_writing());
    }

    #[test]
    fn graph_snapshot_stale_dispatches_run_graph_refresh() {
        let dispatch = fix_graph_snapshot_stale(&root());
        assert!(matches!(dispatch.op, Op::RunGraphRefresh { .. }));
        assert_eq!(dispatch.op.kind_str(), "run_graph_refresh");
    }

    #[test]
    fn wal_checkpoint_pending_dispatches_run_wal_checkpoint() {
        let dispatch = fix_wal_checkpoint_pending(&root());
        let Op::RunWalCheckpoint { ref mode, .. } = dispatch.op else {
            panic!("expected RunWalCheckpoint");
        };
        assert_eq!(mode, "truncate");
        assert_eq!(dispatch.op.kind_str(), "run_wal_checkpoint");
    }

    #[test]
    fn schema_migration_pending_dispatches_run_migration() {
        let dispatch = fix_schema_migration_pending(&root(), "V099");
        let Op::RunMigration {
            ref target_version, ..
        } = dispatch.op
        else {
            panic!("expected RunMigration");
        };
        assert_eq!(target_version, "V099");
        assert_eq!(dispatch.severity, "error");
    }

    #[test]
    fn beads_jsonl_drift_dispatches_rewrite_jsonl() {
        let dispatch = fix_beads_jsonl_drift(&root(), 2317);
        let Op::RewriteJsonl { row_count, .. } = dispatch.op else {
            panic!("expected RewriteJsonl");
        };
        assert_eq!(row_count, 2317);
    }

    #[test]
    fn workspace_config_malformed_toml_dispatches_quarantine() {
        let dispatch = fix_workspace_config_malformed_toml(&root());
        assert!(matches!(dispatch.op, Op::QuarantineByRename { .. }));
        assert!(dispatch.op.is_writing());
    }

    #[test]
    fn workspace_config_atomic_rewrite_dispatches_atomic_rewrite_toml() {
        let dispatch = fix_workspace_config_atomic_rewrite(&root(), "set runtime.profile = swarm");
        assert!(matches!(dispatch.op, Op::AtomicRewriteToml { .. }));
        assert_eq!(dispatch.op.kind_str(), "atomic_rewrite_toml");
    }

    #[test]
    fn agent_coordination_stale_lease_dispatches_quarantine_under_workspace() {
        let workspace = root();
        let marker = workspace.join(".agent-mail/file_reservations/abc.json");
        let dispatch = fix_agent_coordination_stale_lease(&workspace, &marker);
        let Op::QuarantineByRename {
            ref dest_under_quarantine,
        } = dispatch.op
        else {
            panic!("expected QuarantineByRename");
        };
        assert_eq!(
            dest_under_quarantine,
            &PathBuf::from(".agent-mail/file_reservations/abc.json")
        );
    }

    #[test]
    fn cass_integration_drift_is_manual() {
        let dispatch = fix_cass_integration_drift(&root());
        assert!(matches!(dispatch.op, Op::Manual { .. }));
        assert!(dispatch.op.is_advisory());
    }

    #[test]
    fn policy_safety_inconsistent_is_manual_error() {
        let dispatch = fix_policy_safety_inconsistent(&root());
        assert!(matches!(dispatch.op, Op::Manual { .. }));
        assert_eq!(dispatch.severity, "error");
    }

    #[test]
    fn snapshot_backup_owed_dispatches_snapshot_backup() {
        let dispatch = fix_snapshot_backup_owed(&root(), "doctor-pre-migration");
        let Op::SnapshotBackup { ref label, .. } = dispatch.op else {
            panic!("expected SnapshotBackup");
        };
        assert_eq!(label, "doctor-pre-migration");
    }

    #[test]
    fn state_file_permission_drift_dispatches_chmod_0600() {
        let dispatch = fix_state_file_permission_drift(root().join(".ee/secrets.json"));
        let Op::Chmod { mode } = dispatch.op else {
            panic!("expected Chmod");
        };
        assert_eq!(mode, 0o600);
        assert!(dispatch.op.is_writing());
    }

    #[test]
    fn every_registered_code_emits_a_dispatch_for_its_finding() {
        let workspace = root();
        let dispatches: Vec<&'static str> = vec![
            fix_search_index_stale(&workspace).finding_code,
            fix_graph_snapshot_stale(&workspace).finding_code,
            fix_wal_checkpoint_pending(&workspace).finding_code,
            fix_schema_migration_pending(&workspace, "V001").finding_code,
            fix_beads_jsonl_drift(&workspace, 0).finding_code,
            fix_workspace_config_malformed_toml(&workspace).finding_code,
            fix_workspace_config_atomic_rewrite(&workspace, "noop").finding_code,
            fix_agent_coordination_stale_lease(&workspace, &workspace.join("a.json")).finding_code,
            fix_cass_integration_drift(&workspace).finding_code,
            fix_policy_safety_inconsistent(&workspace).finding_code,
            fix_snapshot_backup_owed(&workspace, "x").finding_code,
            fix_state_file_permission_drift(workspace.join("a")).finding_code,
        ];
        let mut sorted = dispatches.clone();
        sorted.sort();
        let mut expected = FIXER_FINDING_CODES.to_vec();
        expected.sort();
        assert_eq!(sorted, expected);
    }
}
