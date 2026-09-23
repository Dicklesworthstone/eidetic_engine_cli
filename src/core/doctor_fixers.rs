//! 15 fixers wired through the `doctor_runtime::mutate()` chokepoint: 13
//! auto-fixable ones (bd-tu4s8 Pass-2; bd-pbyay added `search_index_missing`)
//! and 2 guidance-only database fixers (bd-xa6ud / bd-wswg0). Each
//! fixer maps a specific repair-spec finding code to the `Op` that the doctor
//! should call `mutate()` with.
//!
//! Only `Op::is_writing` Ops change the filesystem. Index rebuild executes
//! through reversible primitives. Other unwired subsystem operations record
//! guidance, and `ee doctor --fix` reports them as
//! `guidance_recorded`, never `applied`.
//!
//! The fixers are pure dispatchers — they return the
//! `(path, Op)` pair the caller will hand to `mutate()`. They do NOT call
//! `mutate()` themselves so the test surface stays free of `RunContext`
//! setup. Index repair builds from a read-only canonical source snapshot and
//! journals each real mutation with hash-checked backups and inverse actions.
//!
//! The 13 auto-fixable fixers cover the eight docs/doctor/repair-specs/ subsystems
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

/// FM-SI-01: the search index generation is stale relative to the source
/// corpus. The runtime stages a validated generation and journals
/// every live-file change so the repair can be undone without source writes.
#[must_use]
pub fn fix_search_index_stale(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch {
        finding_code: "search_index_stale",
        severity: "warning",
        path: search_index_dir(workspace_root),
        op: Op::RunIndexRebuild {
            steps: search_index_repair_steps(workspace_root),
        },
    }
}

/// FM-SI (EE-E300): the search index is missing while the source database
/// exists. Distinct from [`fix_search_index_stale`]: there is no manifest to
/// compare, so the follow-up check is that doctor stops reporting EE-E300.
#[must_use]
pub fn fix_search_index_missing(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch {
        finding_code: "search_index_missing",
        severity: "warning",
        path: search_index_dir(workspace_root),
        op: Op::RunIndexRebuild {
            steps: search_index_repair_steps(workspace_root),
        },
    }
}

/// bd-wswg0 (EE-E206): the database file is empty (0 bytes), so the
/// workspace's data is not present. Migrating or rebuilding over it would build
/// a fresh store and hide the loss, so this records guidance only.
#[must_use]
pub fn fix_database_empty(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch::manual(
        "database_empty",
        "error",
        workspace_root.join(".ee").join("ee.db"),
        &[
            "Do not run `ee init` or a migration over the empty file: that builds a fresh store and hides the loss.",
            "List recoverable backups: `ee backup list --workspace .`.",
            "Recover one with `ee backup restore` into a side path, inspect it, then move it into `.ee/`.",
            "Or accept the loss: move `.ee/ee.db` aside and run `ee init --workspace .` to start an empty store.",
        ],
    )
}

/// bd-xa6ud (EE-E202): the database cannot be opened (e.g. truncated). Index
/// repair and migration both read it and would fail, so this records guidance.
#[must_use]
pub fn fix_database_corrupted(workspace_root: &Path) -> FixerDispatch {
    FixerDispatch::manual(
        "database_corrupted",
        "error",
        workspace_root.join(".ee").join("ee.db"),
        &[
            "Keep the damaged file: copy `.ee/ee.db` aside before any recovery attempt.",
            "Do not run `ee index rebuild` or a migration against it; both read the damaged store.",
            "List recoverable backups: `ee backup list --workspace .`, recover one into a side path with `ee backup restore`, inspect it, then move it into `.ee/`.",
            "Or accept the loss: move `.ee/ee.db` aside and run `ee init --workspace .` to start an empty store.",
        ],
    )
}

/// The index directory the doctor's `search_index` detector inspects: no
/// database or index override, so the workspace default (`.ee/index`).
fn search_index_dir(workspace_root: &Path) -> PathBuf {
    crate::config::workspace::resolve_store_index_dir(workspace_root, None, None)
}

fn search_index_repair_steps(workspace_root: &Path) -> Vec<String> {
    let workspace = shell_quote_arg(&workspace_root.to_string_lossy());
    vec![
        format!("ee index rebuild --workspace {workspace} --json"),
        format!("ee index status --workspace {workspace} --json"),
        "Confirm the index is ready and its source and index generations agree.".to_owned(),
    ]
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
                "ee maintenance wal-checkpoint --workspace . --mode truncate --json".to_string(),
                "Verify `.ee/ee.db-wal` shrinks back to the truncation floor.".to_string(),
            ],
        },
    }
}

/// FM-SM-01: schema_migrations table reports a version below the binary's
/// canonical target. Auto-fix dispatches a pending-migration plan; the
/// existing `ee migrate run` command performs the actual mutation under
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
                "ee migrate run --workspace . --json".to_string(),
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
            "Run `ee import cass --dry-run --json` to preview the affected workspace import.",
            "Run `ee import cass --json` to refresh imported CASS evidence from the source corpus.",
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
    let label = label.into();
    let backup_command = format!(
        "ee backup create --workspace . --label {} --json",
        shell_quote_arg(&label)
    );
    FixerDispatch {
        finding_code: "snapshot_backup_owed",
        severity: "warning",
        path: workspace_root.join(".ee"),
        op: Op::SnapshotBackup {
            label: label.clone(),
            steps: vec![
                backup_command,
                "Verify the backup manifest hash matches the pre-mutation source.".to_string(),
            ],
        },
    }
}

fn shell_quote_arg(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
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

/// The closed set of fixer codes this module exposes. bd-tu4s8 delivered 12;
/// bd-pbyay split `search_index_missing` out of `search_index_stale`, because a
/// missing index was being repaired as stale; bd-xa6ud / bd-wswg0 added the
/// guidance-only `database_empty` and `database_corrupted`. A contract test
/// derives the codes from every fixer above and from the `ee doctor --fix`
/// dispatch ([`fix_dispatch_for_finding`], [`FIX_DISPATCHED_FINDINGS`] and
/// `doctor_fix_json`), so this list cannot drift from either (bd-ynfuu).
pub const FIXER_FINDING_CODES: &[&str] = &[
    "search_index_stale",
    "search_index_missing",
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

/// Every finding [`fix_finding_for_check`] can return: what `ee doctor --fix`
/// actually dispatches, as opposed to every fixer this module defines.
/// `ee doctor --capabilities` reports these (bd-223vl M5); a unit test proves
/// the table never returns a finding outside this list.
pub const FIX_DISPATCHED_FINDINGS: &[&str] = &[
    "database_empty",
    "database_corrupted",
    "search_index_missing",
    "search_index_stale",
    "schema_migration_pending",
    "cass_integration_drift",
];

/// How `ee doctor --fix` treats one failing doctor check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixMode {
    /// `--fix` dispatches a writing fixer that repairs the finding.
    AutoRepair,
    /// `--fix` dispatches an advisory fixer: it records guidance and repairs
    /// nothing, so the finding is still reported afterwards.
    AutoGuidance,
    /// `--fix` dispatches nothing; only the check's repair hint applies.
    Manual,
}

impl FixMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AutoRepair => "auto_repair",
            Self::AutoGuidance => "auto_guidance",
            Self::Manual => "manual",
        }
    }
}

/// bd-xa6ud / bd-wswg0: the store is unreadable when the `database` check
/// reports it empty (EE-E206) or unopenable (EE-E202). `checks` yields each
/// check's name and error code.
#[must_use]
pub fn store_unreadable<'a>(checks: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> bool {
    checks
        .into_iter()
        .any(|(name, code)| name == "database" && matches!(code, Some("EE-E206" | "EE-E202")))
}

/// The finding `ee doctor --fix` dispatches for a failing doctor check.
/// `--fix` and `--fix-plan` both read this one table, so the plan cannot call
/// a step fixable that `--fix` never touches (bd-223vl M3).
///
/// An empty or unreadable store (see [`store_unreadable`]) gets guidance for
/// the database, and the repairs that read it (index rebuild, migration) are
/// skipped: running them crashes or builds over lost data (bd-xa6ud).
/// Otherwise the finding is keyed on the error code, and any other failing
/// `search_index` check is repaired as stale.
#[must_use]
pub fn fix_finding_for_check(
    error_code: Option<&str>,
    check_name: &str,
    store_unreadable: bool,
) -> Option<&'static str> {
    if check_name == "database" {
        match error_code {
            Some("EE-E206") => return Some("database_empty"),
            Some("EE-E202") => return Some("database_corrupted"),
            _ => {}
        }
    }
    if store_unreadable
        && (check_name == "search_index"
            || matches!(error_code, Some("EE-E300" | "EE-E301" | "EE-E700")))
    {
        return None;
    }
    match error_code {
        Some("EE-E300") => Some("search_index_missing"),
        Some("EE-E301") => Some("search_index_stale"),
        Some("EE-E700") => Some("schema_migration_pending"),
        Some("EE-E507") => Some("cass_integration_drift"),
        _ if check_name == "search_index" => Some("search_index_stale"),
        _ => None,
    }
}

/// The dispatch for a finding [`fix_finding_for_check`] returns; `None` for
/// any other finding.
#[must_use]
pub fn fix_dispatch_for_finding(workspace_root: &Path, finding: &str) -> Option<FixerDispatch> {
    match finding {
        "database_empty" => Some(fix_database_empty(workspace_root)),
        "database_corrupted" => Some(fix_database_corrupted(workspace_root)),
        "search_index_missing" => Some(fix_search_index_missing(workspace_root)),
        "search_index_stale" => Some(fix_search_index_stale(workspace_root)),
        "schema_migration_pending" => {
            Some(fix_schema_migration_pending(workspace_root, "V_LATEST"))
        }
        "cass_integration_drift" => Some(fix_cass_integration_drift(workspace_root)),
        _ => None,
    }
}

/// What `ee doctor --fix` does for a failing check, and the finding it
/// dispatches, if any. The Op kind does not depend on the workspace path.
#[must_use]
pub fn fix_mode_for_check(
    error_code: Option<&str>,
    check_name: &str,
    store_unreadable: bool,
) -> (FixMode, Option<&'static str>) {
    let Some(finding) = fix_finding_for_check(error_code, check_name, store_unreadable) else {
        return (FixMode::Manual, None);
    };
    match fix_dispatch_for_finding(Path::new("."), finding) {
        Some(dispatch) if dispatch.op.is_advisory() => (FixMode::AutoGuidance, Some(finding)),
        Some(_) => (FixMode::AutoRepair, Some(finding)),
        None => (FixMode::Manual, None),
    }
}

/// A required core check that this run cannot repair. Keep this separate from
/// fixer receipts: an undispatched check has no mutation, action sequence or
/// undo operation. Only static diagnostic metadata is exposed, never the
/// check's potentially private message or source content.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedCoreCheck {
    pub name: &'static str,
    pub severity: &'static str,
    pub error_code: Option<&'static str>,
    pub repair: Option<&'static str>,
    pub fix_mode: &'static str,
    pub fix_finding: Option<&'static str>,
}

/// Report all unhealthy core checks that receive only guidance or no dispatch.
/// Use the same dispatch and tier contracts as `--fix-plan` and the top-line
/// health verdict. In particular, missing optional CASS tooling is advisory,
/// while a damaged database or unavailable required capability remains pending.
#[must_use]
pub fn unresolved_core_checks(checks: &[super::doctor::CheckResult]) -> Vec<UnresolvedCoreCheck> {
    let unreadable = store_unreadable(
        checks
            .iter()
            .map(|check| (check.name, check.error_code.map(|code| code.id))),
    );
    checks
        .iter()
        .filter(|check| !check.is_topline_healthy())
        .filter_map(|check| {
            let error_code = check.error_code.map(|code| code.id);
            let (mode, finding) = fix_mode_for_check(error_code, check.name, unreadable);
            if mode == FixMode::AutoRepair {
                return None;
            }
            Some(UnresolvedCoreCheck {
                name: check.name,
                severity: check.severity.as_str(),
                error_code,
                repair: check.repair,
                fix_mode: mode.as_str(),
                fix_finding: finding,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_core_checks_retain_undispatched_and_guidance_failures_only() {
        use crate::core::doctor::{CheckResult, CheckSeverity};
        use crate::models::error_codes::{CASS_DEGRADED, INDEX_NOT_FOUND, MIGRATION_REQUIRED};

        let mut uncoded = CheckResult::ok("runtime", "PRIVATE-CHECK-MESSAGE");
        uncoded.severity = CheckSeverity::Error;
        let checks = [
            CheckResult::ok("configuration", "ready"),
            CheckResult::warning("search_index", "rebuildable", INDEX_NOT_FOUND),
            CheckResult::warning("cass", "optional", CASS_DEGRADED).advisory(),
            CheckResult::error("database", "PRIVATE-MIGRATION-MESSAGE", MIGRATION_REQUIRED),
            uncoded,
        ];
        let pending = unresolved_core_checks(&checks);
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].name, "database");
        assert_eq!(pending[0].fix_mode, "auto_guidance");
        assert_eq!(pending[0].fix_finding, Some("schema_migration_pending"));
        assert_eq!(pending[1].name, "runtime");
        assert_eq!(pending[1].fix_mode, "manual");
        assert_eq!(pending[1].error_code, None);
        assert_eq!(pending[1].repair, None);
        let json = serde_json::to_string(&pending).expect("serializable pending checks");
        assert!(!json.contains("PRIVATE"));
        assert_eq!(pending, unresolved_core_checks(&checks));
    }

    #[test]
    fn unresolved_core_checks_keep_suppressed_repairs_without_dispatching_them() {
        use crate::core::doctor::CheckResult;
        use crate::models::error_codes::{DATABASE_CORRUPTED, INDEX_NOT_FOUND, MIGRATION_REQUIRED};

        let pending = unresolved_core_checks(&[
            CheckResult::error("database", "damaged store", DATABASE_CORRUPTED),
            CheckResult::warning("search_index", "no safe source", INDEX_NOT_FOUND),
            CheckResult::error("migration", "no safe source", MIGRATION_REQUIRED),
        ]);
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].fix_finding, Some("database_corrupted"));
        for check in &pending[1..] {
            assert_eq!(check.fix_mode, "manual");
            assert_eq!(check.fix_finding, None);
        }
    }

    #[test]
    fn unresolved_core_checks_follow_health_tiers_not_severity_alone() {
        use crate::core::doctor::CheckResult;
        use crate::models::error_codes::RUNTIME_UNAVAILABLE;

        let core = CheckResult::warning("runtime", "required", RUNTIME_UNAVAILABLE);
        assert_eq!(unresolved_core_checks(&[core]).len(), 1);
        let advisory = CheckResult::error("runtime", "optional", RUNTIME_UNAVAILABLE).advisory();
        assert!(unresolved_core_checks(&[advisory]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn search_index_repair_commands_preserve_the_selected_workspace_as_one_argument() {
        for workspace in [
            "/tmp/doctor workspace",
            "/tmp/agent's store",
            "/tmp/$(exit 99); literal",
        ] {
            for dispatch in [
                fix_search_index_missing(Path::new(workspace)),
                fix_search_index_stale(Path::new(workspace)),
            ] {
                let Op::RunIndexRebuild { steps } = dispatch.op else {
                    panic!("expected a real index rebuild");
                };
                for (step, verb) in [(&steps[0], "rebuild"), (&steps[1], "status")] {
                    // Parse the quoted command with the real shell without
                    // executing ee or any interpolated workspace content.
                    let output = std::process::Command::new("sh")
                        .args(["-c", &format!("set -- {step}; printf '%s\\0' \"$@\"")])
                        .output()
                        .expect("shell argument probe");
                    assert!(output.status.success());
                    let args: Vec<_> = output
                        .stdout
                        .split(|byte| *byte == 0)
                        .filter(|arg| !arg.is_empty())
                        .collect();
                    let expected = ["ee", "index", verb, "--workspace", workspace, "--json"];
                    assert_eq!(args, expected.map(str::as_bytes));
                }
                assert!(!steps.join(" ").contains("manifest.last_built_at"));
            }
        }
    }

    #[test]
    fn every_fix_finding_constructs_a_dispatch_with_that_code() {
        let checks = [
            (Some("EE-E300"), "search_index", false),
            (Some("EE-E301"), "search_index", false),
            (Some("EE-E700"), "database", false),
            (Some("EE-E507"), "cass", false),
            (Some("EE-E999"), "search_index", false),
            (Some("EE-E206"), "database", true),
            (Some("EE-E202"), "database", true),
        ];
        for (code, name, unreadable) in checks {
            let finding =
                fix_finding_for_check(code, name, unreadable).expect("dispatched finding");
            let dispatch = fix_dispatch_for_finding(&root(), finding).expect("dispatch");
            assert_eq!(dispatch.finding_code, finding);
        }
        assert_eq!(
            fix_finding_for_check(Some("EE-E102"), "shard_fanout", false),
            None
        );
        assert_eq!(fix_finding_for_check(None, "database", false), None);
        assert!(fix_dispatch_for_finding(&root(), "graph_snapshot_stale").is_none());
    }

    /// Exhaustive over the error-code registry: the dispatch table can only
    /// return listed findings, and every listed finding is reachable.
    #[test]
    fn fix_dispatched_findings_is_exactly_what_the_table_returns() {
        let codes = std::iter::once(None).chain(
            crate::models::error_codes::ALL_ERROR_CODES
                .iter()
                .map(|code| Some(code.id)),
        );
        let mut reached = std::collections::BTreeSet::new();
        for code in codes {
            for name in ["database", "search_index", "cass", "runtime"] {
                for unreadable in [false, true] {
                    if let Some(finding) = fix_finding_for_check(code, name, unreadable) {
                        assert!(
                            FIX_DISPATCHED_FINDINGS.contains(&finding),
                            "{finding} ({code:?}, {name}) is dispatched but not listed"
                        );
                        reached.insert(finding);
                    }
                }
            }
        }
        let listed = FIX_DISPATCHED_FINDINGS
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(reached, listed, "every listed finding must be reachable");
        for finding in FIX_DISPATCHED_FINDINGS {
            assert!(
                fix_dispatch_for_finding(&root(), finding).is_some(),
                "{finding}"
            );
        }
    }

    #[test]
    fn unreadable_store_skips_the_repairs_that_read_it() {
        assert!(store_unreadable([
            ("search_index", Some("EE-E300")),
            ("database", Some("EE-E202")),
        ]));
        assert!(store_unreadable([("database", Some("EE-E206"))]));
        assert!(!store_unreadable([("database", Some("EE-E700"))]));
        assert!(!store_unreadable([("search_index", Some("EE-E202"))]));
        for (code, name) in [
            (Some("EE-E300"), "search_index"),
            (Some("EE-E301"), "search_index"),
            (Some("EE-E999"), "search_index"),
            (Some("EE-E700"), "database"),
        ] {
            assert_eq!(
                fix_finding_for_check(code, name, true),
                None,
                "{code:?} {name}"
            );
        }
        // CASS drift does not read the store, so it is still dispatched.
        assert_eq!(
            fix_finding_for_check(Some("EE-E507"), "cass", true),
            Some("cass_integration_drift")
        );
    }

    #[test]
    fn fix_mode_separates_repairs_from_guidance_and_manual_steps() {
        assert_eq!(
            fix_mode_for_check(Some("EE-E300"), "search_index", false),
            (FixMode::AutoRepair, Some("search_index_missing"))
        );
        assert_eq!(
            fix_mode_for_check(Some("EE-E301"), "search_index", false),
            (FixMode::AutoRepair, Some("search_index_stale"))
        );
        // Op::RunMigration is advisory: --fix records `ee migrate run` as
        // guidance and migrates nothing, so a pending migration is NOT fixable.
        assert_eq!(
            fix_mode_for_check(Some("EE-E700"), "database", false),
            (FixMode::AutoGuidance, Some("schema_migration_pending"))
        );
        // cass_integration_drift is an Op::Manual dispatch: guidance only.
        assert_eq!(
            fix_mode_for_check(Some("EE-E507"), "cass", false),
            (FixMode::AutoGuidance, Some("cass_integration_drift"))
        );
        // bd-xa6ud: an unopenable store gets guidance, never a repair, and
        // the index it would feed is left alone.
        assert_eq!(
            fix_mode_for_check(Some("EE-E202"), "database", true),
            (FixMode::AutoGuidance, Some("database_corrupted"))
        );
        assert_eq!(
            fix_mode_for_check(Some("EE-E300"), "search_index", true),
            (FixMode::Manual, None)
        );
        assert_eq!(
            fix_mode_for_check(Some("EE-E102"), "shard_fanout", false),
            (FixMode::Manual, None)
        );
    }

    fn root() -> PathBuf {
        PathBuf::from("/tmp/doctor-fixers-test")
    }

    // The registered set itself is pinned by
    // `fixer_codes_match_every_fixer_and_every_dispatch`, which derives it from
    // the source; a hard-coded count here is what let two fixers go
    // unregistered (bd-ynfuu).
    #[test]
    fn fixer_codes_are_unique() {
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
        let Op::RunIndexRebuild { steps } = &dispatch.op else {
            panic!("expected RunIndexRebuild");
        };
        assert_eq!(
            steps[0],
            "ee index rebuild --workspace '/tmp/doctor-fixers-test' --json"
        );
        assert_eq!(dispatch.op.kind_str(), "run_index_rebuild");
        assert!(!dispatch.op.is_advisory());
        assert!(dispatch.op.is_writing());
    }

    #[test]
    fn search_index_fixers_target_the_directory_the_detector_inspects() {
        // bd-pbyay: the fixer used to target `.ee/indexes`, a path nothing
        // else in the crate reads; the index lives at `.ee/index`.
        let workspace = root();
        let expected = crate::config::workspace::resolve_store_index_dir(&workspace, None, None);
        assert!(expected.ends_with(".ee/index"), "{}", expected.display());
        for dispatch in [
            fix_search_index_stale(&workspace),
            fix_search_index_missing(&workspace),
        ] {
            assert_eq!(dispatch.path, expected, "{}", dispatch.finding_code);
        }
    }

    #[test]
    fn search_index_missing_is_its_own_finding() {
        let dispatch = fix_search_index_missing(&root());
        assert_eq!(dispatch.finding_code, "search_index_missing");
        let Op::RunIndexRebuild { steps } = &dispatch.op else {
            panic!("expected RunIndexRebuild");
        };
        assert_eq!(
            steps[0],
            "ee index rebuild --workspace '/tmp/doctor-fixers-test' --json"
        );
        assert_eq!(
            steps[1],
            "ee index status --workspace '/tmp/doctor-fixers-test' --json"
        );
        assert!(!dispatch.op.is_advisory());
        assert!(dispatch.op.is_writing());
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
        let Op::RunWalCheckpoint {
            ref mode,
            ref steps,
        } = dispatch.op
        else {
            panic!("expected RunWalCheckpoint");
        };
        assert_eq!(mode, "truncate");
        assert_eq!(
            steps[0],
            "ee maintenance wal-checkpoint --workspace . --mode truncate --json"
        );
        assert_eq!(dispatch.op.kind_str(), "run_wal_checkpoint");
    }

    #[test]
    fn schema_migration_pending_dispatches_run_migration() {
        let dispatch = fix_schema_migration_pending(&root(), "V099");
        let Op::RunMigration {
            ref target_version,
            ref steps,
        } = dispatch.op
        else {
            panic!("expected RunMigration");
        };
        assert_eq!(target_version, "V099");
        assert_eq!(steps[0], "ee migrate run --workspace . --json");
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
        let Op::Manual { steps } = &dispatch.op else {
            panic!("expected Manual");
        };
        assert_eq!(
            steps,
            &[
                "Run `ee import cass --dry-run --json` to preview the affected workspace import.",
                "Run `ee import cass --json` to refresh imported CASS evidence from the source corpus.",
            ]
        );
        assert!(
            steps.iter().all(|step| !step.contains("ee cass")),
            "manual CASS repair steps must use the shipped `ee import cass` surface"
        );
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
        let dispatch = fix_snapshot_backup_owed(&root(), "doctor pre ' migration");
        let Op::SnapshotBackup { ref label, .. } = dispatch.op else {
            panic!("expected SnapshotBackup");
        };
        assert_eq!(label, "doctor pre ' migration");
    }

    #[test]
    fn snapshot_backup_owed_uses_workspace_json_command_and_quotes_label() {
        let dispatch = fix_snapshot_backup_owed(&root(), "doctor pre ' migration");
        let Op::SnapshotBackup { ref steps, .. } = dispatch.op else {
            panic!("expected SnapshotBackup");
        };
        assert_eq!(
            steps[0],
            "ee backup create --workspace . --label 'doctor pre '\\'' migration' --json"
        );
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
            fix_search_index_missing(&workspace).finding_code,
            fix_database_empty(&workspace).finding_code,
            fix_database_corrupted(&workspace).finding_code,
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

    /// Every `pub fn fix_*` in the non-test part of a fixer module's source,
    /// mapped to the finding code it emits: the `finding_code: "..."` field,
    /// or the first argument of `FixerDispatch::manual(`. A `fix_*` function
    /// that builds no dispatch itself (the dispatch-table helpers) maps to
    /// `None`. A line belongs to the most recent function definition only.
    fn fixer_codes_in_source(source: &str) -> std::collections::BTreeMap<String, Option<String>> {
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        let mut codes = std::collections::BTreeMap::new();
        let mut current: Option<String> = None;
        let mut awaiting_manual_code = false;
        for line in production.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("pub fn fix_") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let name = format!("fix_{name}");
                codes.insert(name.clone(), None);
                current = Some(name);
                awaiting_manual_code = false;
                continue;
            }
            if trimmed.starts_with("fn ")
                || (trimmed.starts_with("pub") && trimmed.contains(" fn "))
            {
                current = None;
                continue;
            }
            let Some(name) = current.clone() else {
                continue;
            };
            if codes.get(&name).is_some_and(Option::is_some) {
                continue;
            }
            let literal = |text: &str| text.split('"').nth(1).map(str::to_owned);
            if let Some(rest) = trimmed.strip_prefix("finding_code: ") {
                if let Some(code) = literal(rest) {
                    codes.insert(name, Some(code));
                }
            } else if trimmed.starts_with("FixerDispatch::manual(") {
                awaiting_manual_code = true;
            } else if awaiting_manual_code && trimmed.starts_with('"') {
                if let Some(code) = literal(trimmed) {
                    codes.insert(name, Some(code));
                }
                awaiting_manual_code = false;
            }
        }
        codes
    }

    /// Every `fix_*` function called in the body of the top-level function
    /// whose definition line starts with `header` (up to its closing `}` in
    /// column 0).
    fn dispatched_fixers(source: &str, header: &str) -> std::collections::BTreeSet<String> {
        let Some(start) = source.find(&format!("\n{header}")) else {
            return std::collections::BTreeSet::new();
        };
        let body = &source[start + 1 + header.len()..];
        let body = body.find("\n}").map_or(body, |end| &body[..end]);
        let mut called = std::collections::BTreeSet::new();
        let mut rest = body;
        while let Some(at) = rest.find("fix_") {
            let preceded_by_ident = rest[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            let tail = &rest[at..];
            let name: String = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !preceded_by_ident && tail[name.len()..].starts_with('(') {
                called.insert(name.clone());
            }
            rest = &tail[name.len().max(1)..];
        }
        called
    }

    #[test]
    fn fixer_source_scanners_see_planted_fixers_and_dispatches() {
        let module = r#"
pub fn fix_planted_field(root: &Path) -> FixerDispatch {
    FixerDispatch {
        finding_code: "planted_field",
    }
}
pub fn fix_planted_manual(root: &Path) -> FixerDispatch {
    FixerDispatch::manual(
        "planted_manual",
        "error",
    )
}
pub fn fix_planted_table(root: &Path, finding: &str) -> Option<FixerDispatch> {
    match finding {
        "planted_field" => Some(fix_planted_field(root)),
        _ => None,
    }
}
pub fn not_a_fixer() -> FixerDispatch {
    FixerDispatch {
        finding_code: "stray",
    }
}
#[cfg(test)]
pub fn fix_test_only() -> FixerDispatch { finding_code: "test_only" }
"#;
        let codes = fixer_codes_in_source(module);
        let code = |name: &str| codes.get(name).cloned();
        assert_eq!(
            code("fix_planted_field"),
            Some(Some("planted_field".to_owned()))
        );
        assert_eq!(
            code("fix_planted_manual"),
            Some(Some("planted_manual".to_owned()))
        );
        // A table helper is a fix_* function with no code of its own, and the
        // next function's code is not attributed to it.
        assert_eq!(code("fix_planted_table"), Some(None), "{codes:?}");
        assert!(!codes.contains_key("not_a_fixer"), "{codes:?}");
        assert!(!codes.contains_key("fix_test_only"), "{codes:?}");
        let table = dispatched_fixers(module, "pub fn fix_planted_table(");
        assert_eq!(
            table.into_iter().collect::<Vec<_>>(),
            vec!["fix_planted_field".to_owned()]
        );
        let cli = "\nfn other() { fix_elsewhere(x) }\nfn doctor_fix_json(w: &Path) {\n    Some(fix_planted_field(w));\n    prefix_fix_not_a_call(w);\n}\nfn after() { fix_after(x) }\n";
        let called = dispatched_fixers(cli, "fn doctor_fix_json(");
        assert_eq!(
            called.into_iter().collect::<Vec<_>>(),
            vec!["fix_planted_field".to_owned()]
        );
        assert!(dispatched_fixers(cli, "fn absent(").is_empty());
    }

    #[test]
    fn fixer_codes_match_every_fixer_and_every_dispatch() {
        let module = include_str!("doctor_fixers.rs");
        let fixers = fixer_codes_in_source(module);
        let derived: std::collections::BTreeSet<&str> =
            fixers.values().flatten().map(String::as_str).collect();
        let registered: std::collections::BTreeSet<&str> =
            FIXER_FINDING_CODES.iter().copied().collect();
        assert_eq!(
            derived, registered,
            "FIXER_FINDING_CODES must equal the codes the fix_* functions emit"
        );
        let dispatched_unregistered: Vec<&str> = FIX_DISPATCHED_FINDINGS
            .iter()
            .copied()
            .filter(|finding| !registered.contains(finding))
            .collect();
        assert!(
            dispatched_unregistered.is_empty(),
            "findings --fix dispatches that are not in FIXER_FINDING_CODES: {dispatched_unregistered:?}"
        );

        // The --fix dispatch table (bd-223vl M3): every fixer it calls emits a
        // registered code.
        let table = dispatched_fixers(module, "pub fn fix_dispatch_for_finding(");
        assert!(
            !table.is_empty(),
            "fix_dispatch_for_finding must be found and dispatch at least one fixer"
        );
        let table_violations: Vec<(&String, Option<&Option<String>>)> = table
            .iter()
            .map(|name| (name, fixers.get(name)))
            .filter(
                |(_, code)| !matches!(code, Some(Some(code)) if registered.contains(code.as_str())),
            )
            .collect();
        assert!(
            table_violations.is_empty(),
            "fix_dispatch_for_finding calls fixers with no registered code: {table_violations:?}"
        );

        // doctor_fix_json dispatches through that table, and any fixer it
        // calls directly also emits a registered code.
        let cli = dispatched_fixers(include_str!("../cli/mod.rs"), "fn doctor_fix_json(");
        assert!(
            cli.contains("fix_dispatch_for_finding"),
            "doctor_fix_json must dispatch through fix_dispatch_for_finding; it calls {cli:?}"
        );
        let cli_violations: Vec<(&String, Option<&Option<String>>)> = cli
            .iter()
            .map(|name| (name, fixers.get(name)))
            .filter(|(_, code)| match code {
                None => true,
                Some(None) => false,
                Some(Some(code)) => !registered.contains(code.as_str()),
            })
            .collect();
        assert!(
            cli_violations.is_empty(),
            "doctor_fix_json calls fix_* functions that are unknown here or unregistered: {cli_violations:?}"
        );
    }
}
