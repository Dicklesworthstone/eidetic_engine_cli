use ee::db::shard::{
    DbShardRouter, DbShardRouterError, DbShardRoutingMode, PRE_SHARD_FANOUT_FILE_NAME,
    SHARD_CATALOG_FILE_NAME, SHARD_FANOUT_CATALOG_MISSING_CODE, SHARD_FANOUT_SHARD_MISSING_CODE,
    ShardFanoutMigrationPlanInput, ShardFanoutMigrationWorkspaceInput, ShardFanoutPosture,
    ShardFanoutResolverInput, plan_shard_fanout_migration, resolve_shard_fanout_status,
    shard_file_path,
};

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

    let missing_catalog = DbShardRouter::resolve(ShardFanoutResolverInput {
        enabled: true,
        workspace_id: Some("wsp_missing_catalog".to_owned()),
        workspace_root: Some(workspace_root.clone()),
        shards_dir_override: Some(shard_root.clone()),
    })
    .expect_err("enabled mode must not route without catalog");

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

    let missing_shard = DbShardRouter::resolve(ShardFanoutResolverInput {
        enabled: true,
        workspace_id: Some("wsp_missing_shard".to_owned()),
        workspace_root: Some(workspace_root),
        shards_dir_override: Some(shard_root),
    })
    .expect_err("enabled mode must not route without required shard");

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
