use ee::db::shard::{
    DbShardRouter, DbShardRouterError, DbShardRoutingMode, PRE_SHARD_FANOUT_FILE_NAME,
    SHARD_CATALOG_FILE_NAME, SHARD_FANOUT_CATALOG_MISSING_CODE, SHARD_FANOUT_SHARD_MISSING_CODE,
    ShardFanoutMigrationPlanInput, ShardFanoutMigrationWorkspaceInput, ShardFanoutPosture,
    ShardFanoutResolverInput, plan_shard_fanout_migration, resolve_shard_fanout_status,
    shard_file_path,
};

use crate::isolated_ee;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

fn temp_root(label: &str) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(label)
        .tempdir()
        .map_err(|error| error.to_string())
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn off_switch_keeps_legacy_database_authoritative_even_when_shards_exist() -> TestResult {
    let temp = temp_root("ee-shard-off-switch")?;
    let workspace_root = temp.path().join("workspace");
    let data_root = temp.path().join("data");
    let shard_root = data_root.join("shards");
    std::fs::create_dir_all(&shard_root).map_err(|error| error.to_string())?;
    std::fs::write(data_root.join(SHARD_CATALOG_FILE_NAME), b"catalog")
        .map_err(|error| error.to_string())?;
    std::fs::write(shard_file_path(&shard_root, "wsp_offswitch"), b"shard")
        .map_err(|error| error.to_string())?;

    let router = DbShardRouter::resolve(ShardFanoutResolverInput {
        enabled: false,
        workspace_id: Some("wsp_offswitch".to_owned()),
        workspace_root: Some(workspace_root.clone()),
        shards_dir_override: Some(shard_root),
    })
    .map_err(|error| error.to_string())?;

    ensure(
        router.status().posture == ShardFanoutPosture::Disabled,
        "disabled off-switch must report disabled posture",
    )?;
    ensure(
        router.status().degraded.is_empty(),
        "disabled off-switch should not degrade because shard files exist",
    )?;
    ensure(
        router.handle().routing_mode == DbShardRoutingMode::Legacy,
        "disabled off-switch must keep legacy routing authoritative",
    )?;
    ensure(
        router.handle().database_path == workspace_root.join(".ee").join("ee.db"),
        "disabled off-switch must route to workspace .ee/ee.db",
    )
}

#[test]
fn enabled_mode_fails_closed_when_catalog_or_shard_is_missing() -> TestResult {
    let temp = temp_root("ee-shard-fail-closed")?;
    let workspace_root = temp.path().join("workspace");
    let data_root = temp.path().join("data");
    let shard_root = data_root.join("shards");

    let missing_catalog = match DbShardRouter::resolve(ShardFanoutResolverInput {
        enabled: true,
        workspace_id: Some("wsp_missing_catalog".to_owned()),
        workspace_root: Some(workspace_root.clone()),
        shards_dir_override: Some(shard_root.clone()),
    }) {
        Ok(_) => return Err("enabled mode must not route without catalog".to_owned()),
        Err(error) => error,
    };

    match missing_catalog {
        DbShardRouterError::ShardNotAuthoritative {
            posture,
            degraded_codes,
        } => {
            ensure(
                posture == ShardFanoutPosture::MigrationRequired,
                "missing catalog should require migration",
            )?;
            ensure(
                degraded_codes.contains(&SHARD_FANOUT_CATALOG_MISSING_CODE),
                "missing catalog degraded code should be present",
            )?;
        }
        other => return Err(format!("unexpected missing catalog error: {other}")),
    }

    std::fs::create_dir_all(&data_root).map_err(|error| error.to_string())?;
    std::fs::write(data_root.join(SHARD_CATALOG_FILE_NAME), b"catalog")
        .map_err(|error| error.to_string())?;

    let missing_shard = match DbShardRouter::resolve(ShardFanoutResolverInput {
        enabled: true,
        workspace_id: Some("wsp_missing_shard".to_owned()),
        workspace_root: Some(workspace_root),
        shards_dir_override: Some(shard_root),
    }) {
        Ok(_) => return Err("enabled mode must not route without required shard".to_owned()),
        Err(error) => error,
    };

    match missing_shard {
        DbShardRouterError::ShardNotAuthoritative {
            posture,
            degraded_codes,
        } => {
            ensure(
                posture == ShardFanoutPosture::MigrationRequired,
                "missing shard should require migration",
            )?;
            ensure(
                degraded_codes.contains(&SHARD_FANOUT_SHARD_MISSING_CODE),
                "missing shard degraded code should be present",
            )?;
        }
        other => return Err(format!("unexpected missing shard error: {other}")),
    }

    Ok(())
}

#[test]
fn migration_plan_exposes_preserved_rollback_path_without_writing_it() -> TestResult {
    let temp = temp_root("ee-shard-rollback-plan")?;
    let source_database_path = temp.path().join("workspace/.ee/ee.db");
    let shard_root = temp.path().join("data/shards");

    let plan = plan_shard_fanout_migration(ShardFanoutMigrationPlanInput {
        source_database_path: source_database_path.clone(),
        shards_dir_override: Some(shard_root),
        workspaces: vec![ShardFanoutMigrationWorkspaceInput {
            workspace_id: "wsp_rollback".to_owned(),
            workspace_root: temp.path().join("workspace"),
        }],
    });

    let expected_preserved = source_database_path
        .parent()
        .ok_or_else(|| "source database should have parent".to_owned())?
        .join(PRE_SHARD_FANOUT_FILE_NAME);

    ensure(
        plan.dry_run,
        "migration planning should be dry-run until the operator explicitly applies it",
    )?;
    ensure(
        plan.preserved_source_database_path == expected_preserved,
        "migration plan should expose .pre-shard-fanout.db rollback path",
    )?;
    ensure(
        !expected_preserved.exists(),
        "dry-run migration planning must not materialize preserved rollback file",
    )?;
    ensure(
        plan.expected_audit_rows.iter().any(|row| {
            row.event == "preserve_legacy_database"
                && row.source_path == source_database_path
                && row.target_path == expected_preserved
        }),
        "migration plan should include preserve_legacy_database audit evidence",
    )
}

#[test]
fn migration_required_status_uses_structured_recovery_action() -> TestResult {
    let temp = temp_root("ee-shard-recovery-action")?;
    let report = resolve_shard_fanout_status(ShardFanoutResolverInput {
        enabled: true,
        workspace_id: Some("wsp_recovery".to_owned()),
        workspace_root: Some(temp.path().join("workspace")),
        shards_dir_override: Some(temp.path().join("data/shards")),
    });

    ensure(
        report.posture == ShardFanoutPosture::MigrationRequired,
        "missing shard layout should require migration",
    )?;
    let action = report
        .recovery
        .first()
        .ok_or_else(|| "migration-required status should include recovery action".to_owned())?;
    ensure(action.priority == 1, "recovery priority should be stable")?;
    ensure(
        action.kind == "dry_run",
        "recovery kind should be structured",
    )?;
    ensure(
        action.command == "ee migrate shard-fanout --workspace . --dry-run --json",
        "recovery command should be the dry-run shard fanout migration",
    )
}

/// Run `ee` with its HOME/XDG dirs under `root` and no inherited shard root.
fn run_ee(root: &Path, args: &[&str], env: &[(&str, &Path)]) -> Result<serde_json::Value, String> {
    let mut command = isolated_ee::isolated_ee_command(root)?;
    command
        .env_remove("EE_SHARDS_DIR")
        .env_remove("EE_SHARD_FANOUT_ENABLED");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .args(args)
        .output()
        .map_err(|error| format!("spawn ee {args:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ee {args:?} exited {:?}: stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ee {args:?} stdout is not JSON: {error}"))
}

/// A fresh isolated root with one initialized workspace holding one memory.
/// Canonicalized because shard roots reject symlinked components.
fn seeded_shard_world(label: &str) -> Result<(tempfile::TempDir, PathBuf, PathBuf), String> {
    let temp = temp_root(label)?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let ws = workspace.to_str().ok_or("workspace path is not UTF-8")?;
    run_ee(
        &root,
        &["--workspace", ws, "init", "--skip-boilerplate", "--json"],
        &[],
    )?;
    run_ee(
        &root,
        &[
            "remember",
            "shard root probe",
            "--workspace",
            ws,
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
        &[],
    )?;
    Ok((temp, root, workspace))
}

fn shard_db_count(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "db"))
            .count()
    })
}

fn applied_shard_root(report: &serde_json::Value) -> String {
    report["data"]["apply"]["shardRoot"]
        .as_str()
        .unwrap_or("<missing>")
        .to_owned()
}

/// bd-qxc0b: without `--shards-dir`, `migrate shard-fanout` must resolve the
/// shard root from `EE_SHARDS_DIR`, as its help text says and as doctor, status
/// and backup already do, not fall back to the XDG default.
#[test]
fn migrate_shard_fanout_honours_exported_ee_shards_dir() -> TestResult {
    let (_temp, root, workspace) = seeded_shard_world("ee-shard-env-root")?;
    let env_dir = root.join("custom/shards");
    let default_dir = root.join("xdg-data/ee/shards");
    let ws = workspace.to_str().ok_or("workspace path is not UTF-8")?;
    let report = run_ee(
        &root,
        &["migrate", "shard-fanout", "--workspace", ws, "--json"],
        &[
            ("EE_SHARD_FANOUT_ENABLED", Path::new("1")),
            ("EE_SHARDS_DIR", &env_dir),
        ],
    )?;
    let shard_root = applied_shard_root(&report);
    ensure(
        shard_root == env_dir.display().to_string(),
        format!(
            "shardRoot {shard_root}, expected EE_SHARDS_DIR {}",
            env_dir.display()
        ),
    )?;
    ensure(
        shard_db_count(&env_dir) > 0,
        format!("no shard database written under {}", env_dir.display()),
    )?;
    ensure(
        shard_db_count(&default_dir) == 0,
        format!(
            "shard databases written to the XDG default {}",
            default_dir.display()
        ),
    )
}

/// bd-qxc0b control: an explicit `--shards-dir` still takes precedence over an
/// exported `EE_SHARDS_DIR`.
#[test]
fn migrate_shard_fanout_flag_overrides_exported_ee_shards_dir() -> TestResult {
    let (_temp, root, workspace) = seeded_shard_world("ee-shard-flag-root")?;
    let env_dir = root.join("env/shards");
    let flag_dir = root.join("flag/shards");
    let ws = workspace.to_str().ok_or("workspace path is not UTF-8")?;
    let flag = flag_dir.to_str().ok_or("flag path is not UTF-8")?;
    let report = run_ee(
        &root,
        &[
            "migrate",
            "shard-fanout",
            "--workspace",
            ws,
            "--shards-dir",
            flag,
            "--json",
        ],
        &[
            ("EE_SHARD_FANOUT_ENABLED", Path::new("1")),
            ("EE_SHARDS_DIR", &env_dir),
        ],
    )?;
    let shard_root = applied_shard_root(&report);
    ensure(
        shard_root == flag,
        format!("shardRoot {shard_root}, expected --shards-dir {flag}"),
    )?;
    ensure(
        shard_db_count(&flag_dir) > 0,
        format!("no shard database written under {flag}"),
    )?;
    ensure(
        shard_db_count(&env_dir) == 0,
        format!(
            "shard databases written to EE_SHARDS_DIR {} despite the flag",
            env_dir.display()
        ),
    )
}
