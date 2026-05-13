//! L0 phase 1 contract test (eidetic_engine_cli bd-17c65.12.1).
//!
//! Documents the top-level `ee migrate` surface added in this bead:
//!
//! - `ee migrate status` emits an ee.response.v1 envelope with
//!   schemaVersion / latestCompiledSchemaVersion / pendingCount /
//!   needsMigration / upToDate. Exit code is 0 when up-to-date and
//!   8 (MigrationRequired) when pending migrations exist.
//! - `ee migrate run` applies any unapplied migrations idempotently
//!   and emits an envelope listing applied + skipped version numbers.
//! - `ee migrate run --dry-run` reports what WOULD be applied without
//!   touching the database.
//!
//! The test invokes the real binary against a real workspace so the
//! exit codes and the JSON envelope are exercised end-to-end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ee::db::{DbConnection, MIGRATIONS, MigrationRecord, audit_actions};
use serde_json::Value;

type TestResult = Result<(), String>;

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(ee_binary())
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_stdout(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout not JSON: {error}\nstdout: {stdout}"))
}

fn init_workspace(workspace: &Path) -> Result<(), String> {
    let init = run_ee(&["--workspace", workspace.to_str().unwrap(), "init", "--json"])?;
    if init.status.code() != Some(0) {
        return Err(format!(
            "init failed: exit {:?}, stderr {}",
            init.status.code(),
            String::from_utf8_lossy(&init.stderr)
        ));
    }
    Ok(())
}

fn persistent_temp_workspace(prefix: &str) -> Result<PathBuf, String> {
    let workspace = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("system clock before unix epoch: {error}"))?
            .as_nanos()
    ));
    fs::create_dir_all(&workspace).map_err(|error| format!("create workspace: {error}"))?;
    Ok(workspace)
}

fn seed_legacy_v011_database(workspace: &Path) -> Result<String, String> {
    let ee_dir = workspace.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee dir: {error}"))?;
    let database_path = ee_dir.join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    connection
        .ensure_migration_table()
        .map_err(|error| error.to_string())?;
    for migration in MIGRATIONS.iter().take(11) {
        connection
            .execute_raw(migration.sql())
            .map_err(|error| error.to_string())?;
        let record = MigrationRecord::new(
            migration.version(),
            migration.name(),
            migration.checksum_label(),
            "2026-05-01T00:00:00Z",
        )
        .map_err(|error| error.to_string())?;
        connection
            .record_migration(&record)
            .map_err(|error| error.to_string())?;
    }

    let workspace_path = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    let escaped_path = workspace_path.to_string_lossy().replace('\'', "''");
    let workspace_id = "wsp_01234567890123456789012345";
    let memory_id = "mem_01234567890123456789012345";
    connection
        .execute_raw(&format!(
            "INSERT INTO workspaces (id, path, created_at, updated_at) VALUES ('{workspace_id}', '{escaped_path}', '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z')",
        ))
        .map_err(|error| error.to_string())?;
    connection
        .execute_raw(&format!(
            "INSERT INTO memories (id, workspace_id, level, kind, content, confidence, utility, importance, provenance_uri, created_at, updated_at, trust_class, trust_subclass) VALUES ('{memory_id}', '{workspace_id}', 'procedural', 'rule', 'Legacy schema memory for migration index rebuild.', 0.8, 0.7, 0.6, 'test://legacy-v011', '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z', 'human_explicit', 'test')",
        ))
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;
    Ok(memory_id.to_owned())
}

#[test]
fn migrate_status_on_fresh_workspace_reports_up_to_date() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    init_workspace(tmpdir.path())?;

    let status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    if status.status.code() != Some(0) {
        return Err(format!(
            "migrate status should exit 0 on fresh workspace, got {:?}; stderr {}",
            status.status.code(),
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    let json = parse_stdout(&status, "migrate status")?;
    let schema = json
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing schema".to_string())?;
    if schema != "ee.response.v1" {
        return Err(format!("schema mismatch: got {schema}"));
    }
    let up_to_date = json
        .pointer("/data/upToDate")
        .and_then(Value::as_bool)
        .ok_or_else(|| "missing data.upToDate".to_string())?;
    if !up_to_date {
        return Err("fresh workspace should report upToDate=true".to_string());
    }
    let pending = json
        .pointer("/data/pendingCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing pendingCount".to_string())?;
    if pending != 0 {
        return Err(format!(
            "fresh workspace should have 0 pending, got {pending}"
        ));
    }
    Ok(())
}

#[test]
fn migrate_run_dry_run_does_not_mutate_database() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    init_workspace(tmpdir.path())?;

    let pre_status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    let pre_json = parse_stdout(&pre_status, "pre status")?;
    let pre_schema_version = pre_json.pointer("/data/schemaVersion").cloned();

    let dry = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "run",
        "--dry-run",
        "--json",
    ])?;
    if dry.status.code() != Some(0) {
        return Err(format!(
            "migrate run --dry-run should exit 0, got {:?}",
            dry.status.code()
        ));
    }
    let dry_json = parse_stdout(&dry, "migrate run --dry-run")?;
    let dry_run_flag = dry_json
        .pointer("/data/dryRun")
        .and_then(Value::as_bool)
        .ok_or_else(|| "missing data.dryRun".to_string())?;
    if !dry_run_flag {
        return Err("data.dryRun must be true for --dry-run".to_string());
    }

    // Schema version unchanged after --dry-run.
    let post_status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    let post_json = parse_stdout(&post_status, "post status")?;
    let post_schema_version = post_json.pointer("/data/schemaVersion").cloned();
    if pre_schema_version != post_schema_version {
        return Err(format!(
            "schemaVersion changed across --dry-run: {pre_schema_version:?} → {post_schema_version:?}"
        ));
    }
    Ok(())
}

#[test]
fn migrate_run_is_idempotent_on_up_to_date_workspace() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    init_workspace(tmpdir.path())?;

    let first = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "run",
        "--json",
    ])?;
    if first.status.code() != Some(0) {
        return Err(format!("first migrate run exit {:?}", first.status.code()));
    }
    let first_json = parse_stdout(&first, "first run")?;
    let applied = first_json
        .pointer("/data/appliedCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing appliedCount".to_string())?;
    if applied != 0 {
        return Err(format!(
            "fresh workspace should have no migrations to apply, got {applied}"
        ));
    }

    // Second run: must also exit 0 and apply nothing.
    let second = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "run",
        "--json",
    ])?;
    if second.status.code() != Some(0) {
        return Err(format!(
            "second migrate run exit {:?}",
            second.status.code()
        ));
    }
    let second_json = parse_stdout(&second, "second run")?;
    let second_applied = second_json
        .pointer("/data/appliedCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing appliedCount on second run".to_string())?;
    if second_applied != 0 {
        return Err(format!(
            "second migrate run on up-to-date workspace should apply nothing, got {second_applied}"
        ));
    }
    Ok(())
}

#[test]
fn migrate_run_rebuilds_index_and_audits_when_schema_changes_apply() -> TestResult {
    let workspace = persistent_temp_workspace("ee-migrate-index-rebuild")?;
    let memory_id = seed_legacy_v011_database(&workspace)?;

    let migrated = run_ee(&[
        "--workspace",
        workspace.to_str().unwrap(),
        "migrate",
        "run",
        "--json",
    ])?;
    if migrated.status.code() != Some(0) {
        return Err(format!(
            "migrate run should exit 0, got {:?}; stderr {}",
            migrated.status.code(),
            String::from_utf8_lossy(&migrated.stderr)
        ));
    }
    let json = parse_stdout(&migrated, "migrate run legacy v011")?;
    let applied = json
        .pointer("/data/appliedCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing data.appliedCount".to_string())?;
    if applied == 0 {
        return Err("legacy schema should apply pending migrations".to_string());
    }

    let rebuild = json
        .pointer("/data/postMigrationIndexRebuild")
        .ok_or_else(|| "missing data.postMigrationIndexRebuild".to_string())?;
    let required = rebuild
        .get("required")
        .and_then(Value::as_bool)
        .ok_or_else(|| "missing rebuild.required".to_string())?;
    if !required {
        return Err("index rebuild should be required after schema changes".to_string());
    }
    let status = rebuild
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing rebuild.status".to_string())?;
    if status != "success" {
        return Err(format!("expected successful index rebuild, got {status}"));
    }
    let memories_indexed = rebuild
        .get("memoriesIndexed")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing rebuild.memoriesIndexed".to_string())?;
    if memories_indexed == 0 {
        return Err(format!(
            "post-migration rebuild should index seeded memory {memory_id}"
        ));
    }
    let audit_id = rebuild
        .get("auditId")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing rebuild.auditId".to_string())?;
    if !audit_id.starts_with("audit_") {
        return Err(format!("unexpected audit id: {audit_id}"));
    }

    let conn =
        DbConnection::open_file(workspace.join(".ee").join("ee.db")).map_err(|e| e.to_string())?;
    let audit_entries = conn
        .list_audit_by_action(audit_actions::MIGRATION_INDEX_REBUILD, None)
        .map_err(|e| e.to_string())?;
    if audit_entries.len() != 1 {
        return Err(format!(
            "expected one migration index rebuild audit row, got {}",
            audit_entries.len()
        ));
    }
    let audit = &audit_entries[0];
    if audit.id != audit_id {
        return Err(format!(
            "audit id in output ({audit_id}) did not match audit row ({})",
            audit.id
        ));
    }
    let details = audit
        .details
        .as_deref()
        .ok_or_else(|| "audit details missing".to_string())?;
    if !details.contains("v0_2_010_index_rebuild") {
        return Err(format!("audit details missing step id: {details}"));
    }

    let capabilities = run_ee(&[
        "--workspace",
        workspace.to_str().unwrap(),
        "capabilities",
        "--json",
    ])?;
    if capabilities.status.code() != Some(0) {
        return Err(format!(
            "capabilities should exit 0 after migration, got {:?}; stderr {}",
            capabilities.status.code(),
            String::from_utf8_lossy(&capabilities.stderr)
        ));
    }
    let caps_json = parse_stdout(&capabilities, "capabilities after migration")?;
    let last_rebuild = caps_json
        .pointer("/data/index/last_full_rebuild_at")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing capabilities data.index.last_full_rebuild_at".to_string())?;
    if last_rebuild.trim().is_empty() {
        return Err("capabilities last_full_rebuild_at must not be empty".to_string());
    }
    Ok(())
}

#[test]
fn migrate_status_on_missing_database_returns_storage_error() -> TestResult {
    let tmpdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    // Do NOT init — workspace has no DB file.
    let status = run_ee(&[
        "--workspace",
        tmpdir.path().to_str().unwrap(),
        "migrate",
        "status",
        "--json",
    ])?;
    // Storage error exit code is 3 per AGENTS.md.
    let code = status.status.code();
    if code == Some(0) {
        return Err(format!(
            "migrate status on missing DB should not exit 0, got {code:?}"
        ));
    }
    let stdout = String::from_utf8_lossy(&status.stdout);
    if !stdout.contains("\"schema\":\"ee.error.v2\"") {
        return Err(format!(
            "expected ee.error.v2 envelope on missing DB, got: {stdout}"
        ));
    }
    Ok(())
}
