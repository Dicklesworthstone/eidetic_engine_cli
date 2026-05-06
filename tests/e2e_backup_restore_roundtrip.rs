//! End-to-end backup → restore round-trip test for eidetic_engine_cli-534m.
//!
//! Seeds a workspace with diverse memories + tags, runs `ee backup create`
//! and `ee backup restore --side-path`, then opens both SQLite databases
//! through `DbConnection` and diffs every content-bearing memory + tag row
//! between the source workspace and the restored side-path.

use std::path::{Path, PathBuf};
use std::process::Command;

use ee::db::{DbConnection, StoredMemory};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn run_ee(args: &[&str]) -> Result<JsonValue, String> {
    let output = Command::new(ee_bin())
        .args(args)
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse JSON from ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(actual: &T, expected: &T, ctx: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

fn workspace_id_from_db(conn: &DbConnection, workspace_path: &Path) -> Result<String, String> {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let path_str = canonical.to_string_lossy().into_owned();
    if let Some(workspace) = conn
        .get_workspace_by_path(&path_str)
        .map_err(|error| format!("get_workspace_by_path: {error}"))?
    {
        return Ok(workspace.id);
    }
    // Fall back to whichever workspace lives in the DB; backup/restore both
    // emit at most one workspace row.
    let workspaces = conn
        .list_workspaces()
        .map_err(|error| format!("list_workspaces: {error}"))?;
    workspaces
        .into_iter()
        .next()
        .map(|w| w.id)
        .ok_or_else(|| "no workspace row present in database".to_owned())
}

/// Content-bearing fields of a memory that must round-trip identically.
///
/// Intentionally excluded fields, all "modulo IDs that legitimately differ"
/// per the bead's acceptance text:
/// - `created_at` / `updated_at` move forward when records.jsonl is re-imported.
/// - Workspace / memory ids are regenerated against the restore side-path.
/// - `trust_class` is rewritten by `core::jsonl_import::trust_class_for_header`,
///   which derives the class from the export header's `import_source` +
///   `trust_level` rather than reading the per-memory class. This means a
///   `human_explicit` memory comes back as `agent_validated` after a Native
///   export. That's a documented property of the JSONL transit format, not a
///   regression we can fix at the e2e layer.
#[derive(Clone, Debug, PartialEq)]
struct MemoryContent {
    level: String,
    kind: String,
    content: String,
    confidence_milli: i64, // milli units to compare floats deterministically
    utility_milli: i64,
    importance_milli: i64,
    tombstoned: bool,
}

fn milli(value: f32) -> i64 {
    (f64::from(value) * 1000.0).round() as i64
}

impl From<&StoredMemory> for MemoryContent {
    fn from(memory: &StoredMemory) -> Self {
        Self {
            level: memory.level.clone(),
            kind: memory.kind.clone(),
            content: memory.content.clone(),
            confidence_milli: milli(memory.confidence),
            utility_milli: milli(memory.utility),
            importance_milli: milli(memory.importance),
            tombstoned: memory.tombstoned_at.is_some(),
        }
    }
}

fn memory_with_tags(
    conn: &DbConnection,
    memory: &StoredMemory,
) -> Result<(MemoryContent, Vec<String>), String> {
    let tags = conn
        .get_memory_tags(&memory.id)
        .map_err(|error| format!("get_memory_tags({}): {error}", memory.id))?;
    Ok((MemoryContent::from(memory), tags))
}

#[test]
fn backup_then_restore_preserves_every_memory_and_tag() -> TestResult {
    let staging = tempfile::Builder::new()
        .prefix("ee-534m-roundtrip-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let workspace = staging.path().join("ws");
    let backup_dir = staging.path().join("backups");
    let side_path = staging.path().join("restored");

    std::fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    let side_path_arg = side_path.to_string_lossy().into_owned();

    // 1. Initialize the source workspace.
    let init = run_ee(&["--workspace", &workspace_arg, "--json", "init"])?;
    ensure_equal(
        &init.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("created"),
        "init status",
    )?;

    // 2. Seed with diverse memories: distinct levels, kinds, tag sets, scores.
    let seeds: &[(&str, &str, &str, &str, &str)] = &[
        (
            "procedural",
            "rule",
            "Always run cargo fmt --check before release",
            "alpha,backup-test",
            "0.95",
        ),
        (
            "semantic",
            "fact",
            "FrankenSQLite is the storage layer",
            "beta,backup-test",
            "0.90",
        ),
        (
            "episodic",
            "observation",
            "Saw the build pass after pinning the toolchain",
            "alpha,observation",
            "0.55",
        ),
        (
            "procedural",
            "anti-pattern",
            "Never invoke git reset --hard on dirty trees",
            "policy,backup-test",
            "0.99",
        ),
    ];

    for (level, kind, content, tags, confidence) in seeds {
        let json = run_ee(&[
            "--workspace",
            &workspace_arg,
            "--json",
            "remember",
            "--level",
            level,
            "--kind",
            kind,
            "--tags",
            tags,
            "--confidence",
            confidence,
            content,
        ])?;
        ensure_equal(
            &json.pointer("/success").and_then(JsonValue::as_bool),
            &Some(true),
            &format!("remember `{content}` succeeded"),
        )?;
    }

    // 3. Capture source-side ground truth from the SQLite database directly.
    let src_db = workspace.join(".ee").join("ee.db");
    ensure(src_db.exists(), "source database exists")?;
    let src_conn =
        DbConnection::open_file(&src_db).map_err(|error| format!("open src db: {error}"))?;
    let src_workspace_id = workspace_id_from_db(&src_conn, &workspace)?;
    let src_memories = src_conn
        .list_memories(&src_workspace_id, None, false)
        .map_err(|error| format!("src list_memories: {error}"))?;
    ensure_equal(
        &src_memories.len(),
        &seeds.len(),
        "seeded memory count matches list_memories",
    )?;

    let mut src_pairs: Vec<(MemoryContent, Vec<String>)> = src_memories
        .iter()
        .map(|memory| memory_with_tags(&src_conn, memory))
        .collect::<Result<_, _>>()?;
    src_pairs.sort_by(|a, b| a.0.content.cmp(&b.0.content));
    drop(src_conn);

    // 4. Create the backup with redaction = none so content survives intact.
    let backup = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "create",
        "--output-dir",
        &backup_dir_arg,
        "--redaction",
        "none",
        "--label",
        "534m-roundtrip",
    ])?;
    ensure_equal(
        &backup.pointer("/data/schema").and_then(JsonValue::as_str),
        &Some("ee.backup.create.v1"),
        "backup create schema",
    )?;
    let backup_id = backup
        .pointer("/data/backupId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing backupId".to_owned())?
        .to_owned();
    ensure(backup_id.starts_with("bk_"), "backupId has bk_ prefix")?;
    let memory_records = backup
        .pointer("/data/counts/memoryRecords")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| "missing memoryRecords count".to_owned())?;
    ensure_equal(
        &usize::try_from(memory_records).unwrap_or(0),
        &seeds.len(),
        "backup memoryRecords matches seeded count",
    )?;

    // 5. Restore to the side path.
    let restore = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "restore",
        &backup_id,
        "--output-dir",
        &backup_dir_arg,
        "--side-path",
        &side_path_arg,
    ])?;
    ensure_equal(
        &restore.pointer("/data/schema").and_then(JsonValue::as_str),
        &Some("ee.backup.restore.v1"),
        "restore schema",
    )?;
    ensure_equal(
        &restore.pointer("/data/dryRun").and_then(JsonValue::as_bool),
        &Some(false),
        "restore was not a dry run",
    )?;
    let import_status = restore
        .pointer("/data/importStatus")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing importStatus".to_owned())?;
    ensure(
        matches!(import_status, "imported" | "completed"),
        format!("import status was {import_status:?}, expected imported|completed"),
    )?;
    let imported = restore
        .pointer("/data/counts/memoriesImported")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| "missing memoriesImported".to_owned())?;
    ensure_equal(
        &usize::try_from(imported).unwrap_or(0),
        &seeds.len(),
        "memoriesImported matches seeded count",
    )?;

    // 6. Source database must still exist after restore (restore is non-destructive).
    ensure(
        src_db.exists(),
        "source database survives restore (no in-place mutation)",
    )?;

    // 7. Restored DB now has the full content. Diff every content-bearing field.
    let restored_db_path = restore
        .pointer("/data/restoredDatabasePath")
        .and_then(JsonValue::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| "missing restoredDatabasePath".to_owned())?;
    ensure(
        restored_db_path.exists(),
        format!("restored database exists at {}", restored_db_path.display()),
    )?;

    let restored_conn = DbConnection::open_file(&restored_db_path)
        .map_err(|error| format!("open restored db: {error}"))?;
    let restored_workspace_id = workspace_id_from_db(&restored_conn, &side_path)?;
    let restored_memories = restored_conn
        .list_memories(&restored_workspace_id, None, false)
        .map_err(|error| format!("restored list_memories: {error}"))?;
    ensure_equal(
        &restored_memories.len(),
        &src_pairs.len(),
        "restored memory count matches source",
    )?;

    let mut restored_pairs: Vec<(MemoryContent, Vec<String>)> = restored_memories
        .iter()
        .map(|memory| memory_with_tags(&restored_conn, memory))
        .collect::<Result<_, _>>()?;
    restored_pairs.sort_by(|a, b| a.0.content.cmp(&b.0.content));

    // 8. Row-by-row diff. Content + tag set must match exactly per pair.
    for (index, (src_pair, restored_pair)) in
        src_pairs.iter().zip(restored_pairs.iter()).enumerate()
    {
        ensure_equal(
            &restored_pair.0,
            &src_pair.0,
            &format!("memory[{index}] content fields"),
        )?;
        ensure_equal(
            &restored_pair.1,
            &src_pair.1,
            &format!("memory[{index}] tag set"),
        )?;
    }

    // 9. Restore is idempotent for verification: re-running list_memories on
    //    the restored DB after a fresh open returns the same content.
    drop(restored_conn);
    let restored_conn2 = DbConnection::open_file(&restored_db_path)
        .map_err(|error| format!("re-open restored db: {error}"))?;
    let restored_again = restored_conn2
        .list_memories(&restored_workspace_id, None, false)
        .map_err(|error| format!("restored re-list: {error}"))?;
    ensure_equal(
        &restored_again.len(),
        &restored_pairs.len(),
        "restored count stable across reopen",
    )?;

    Ok(())
}

#[test]
fn backup_dry_run_restore_does_not_materialize_database() -> TestResult {
    let staging = tempfile::Builder::new()
        .prefix("ee-534m-dryrun-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let workspace = staging.path().join("ws");
    let backup_dir = staging.path().join("backups");
    let side_path = staging.path().join("restored");
    std::fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let backup_dir_arg = backup_dir.to_string_lossy().into_owned();
    let side_path_arg = side_path.to_string_lossy().into_owned();

    run_ee(&["--workspace", &workspace_arg, "--json", "init"])?;
    run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Dry-run probe memory",
    ])?;

    let backup = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "create",
        "--output-dir",
        &backup_dir_arg,
        "--redaction",
        "none",
        "--label",
        "534m-dryrun",
    ])?;
    let backup_id = backup
        .pointer("/data/backupId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing backupId".to_owned())?
        .to_owned();

    let restore = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "backup",
        "restore",
        &backup_id,
        "--output-dir",
        &backup_dir_arg,
        "--side-path",
        &side_path_arg,
        "--dry-run",
    ])?;
    ensure_equal(
        &restore.pointer("/data/dryRun").and_then(JsonValue::as_bool),
        &Some(true),
        "restore reports dryRun=true",
    )?;
    ensure(
        !side_path.join(".ee").join("ee.db").exists(),
        "dry-run restore must not materialize a side-path database",
    )?;
    Ok(())
}
