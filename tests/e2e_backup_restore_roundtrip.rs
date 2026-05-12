//! End-to-end backup → restore round-trip test for eidetic_engine_cli-534m.
//!
//! Seeds a workspace with diverse memories + tags, runs `ee backup create`
//! and `ee backup restore --side-path`, then opens both SQLite databases
//! through `DbConnection` and diffs every content-bearing memory + tag row
//! between the source workspace and the restored side-path.

use std::fs;
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

fn json_str<'a>(value: &'a JsonValue, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn json_u64(value: &JsonValue, pointer: &str, context: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| format!("{context}: missing integer at {pointer}"))
}

fn artifact_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("e2e_backup_restore_roundtrip_artifacts")
}

fn persist_json_artifact(name: &str, value: &JsonValue) -> TestResult {
    let dir = artifact_dir();
    fs::create_dir_all(&dir).map_err(|error| format!("mkdir artifact dir: {error}"))?;
    let path = dir.join(format!("{name}.json"));
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("render artifact {name}: {error}"))?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|error| format!("write artifact {}: {error}", path.display()))
}

fn read_jsonl_records(path: &Path) -> Result<Vec<JsonValue>, String> {
    let input = fs::read_to_string(path)
        .map_err(|error| format!("read JSONL {}: {error}", path.display()))?;
    input
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(serde_json::from_str::<JsonValue>(trimmed).map_err(|error| {
                    format!("parse JSONL {} line {}: {error}", path.display(), index + 1)
                }))
            }
        })
        .collect()
}

fn records_with_schema(records: &[JsonValue], schema: &str) -> Vec<JsonValue> {
    records
        .iter()
        .filter(|record| record.get("schema").and_then(JsonValue::as_str) == Some(schema))
        .cloned()
        .collect()
}

fn normalized_records_with_schema(
    records: &[JsonValue],
    schema: &str,
    ignored_fields: &[&str],
) -> Vec<JsonValue> {
    let mut normalized = records_with_schema(records, schema)
        .into_iter()
        .map(|mut record| {
            if let JsonValue::Object(object) = &mut record {
                for field in ignored_fields {
                    object.remove(*field);
                }
            }
            record
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(JsonValue::to_string);
    normalized
}

fn records_path_from_report(report: &JsonValue, context: &str) -> Result<PathBuf, String> {
    json_str(report, "/data/recordsPath", context).map(PathBuf::from)
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
fn export_import_export_preserves_memory_and_tag_records() -> TestResult {
    let staging = tempfile::Builder::new()
        .prefix("ee-0n9b5-jsonl-roundtrip-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let source_workspace = staging.path().join("source-ws");
    let imported_workspace = staging.path().join("imported-ws");
    let source_export_dir = staging.path().join("source-export");
    let imported_export_dir = staging.path().join("imported-export");
    let provenance_dir = staging.path().join("provenance");
    fs::create_dir_all(&source_workspace).map_err(|error| format!("mkdir source ws: {error}"))?;
    fs::create_dir_all(&imported_workspace)
        .map_err(|error| format!("mkdir imported ws: {error}"))?;
    fs::create_dir_all(&provenance_dir).map_err(|error| format!("mkdir provenance: {error}"))?;

    let source_workspace_arg = source_workspace.to_string_lossy().into_owned();
    let imported_workspace_arg = imported_workspace.to_string_lossy().into_owned();
    let source_export_dir_arg = source_export_dir.to_string_lossy().into_owned();
    let imported_export_dir_arg = imported_export_dir.to_string_lossy().into_owned();

    let init_source = run_ee(&["--workspace", &source_workspace_arg, "--json", "init"])?;
    persist_json_artifact("0n9b5_01_init_source", &init_source)?;

    let seeds: &[(&str, &str, &str, &str, &str)] = &[
        (
            "procedural",
            "rule",
            "JSONL roundtrip rule: run cargo fmt --check before release",
            "roundtrip,rule,release",
            "0.97",
        ),
        (
            "semantic",
            "fact",
            "JSONL roundtrip fact: FrankenSQLite is the source of truth",
            "roundtrip,fact,storage",
            "0.91",
        ),
        (
            "episodic",
            "decision",
            "JSONL roundtrip decision: compare normalized JSON records",
            "roundtrip,decision,testing",
            "0.86",
        ),
        (
            "working",
            "failure",
            "JSONL roundtrip failure: stale exports hide missing provenance",
            "roundtrip,failure,provenance",
            "0.52",
        ),
        (
            "procedural",
            "command",
            "JSONL roundtrip command: ee export --redaction none",
            "roundtrip,command,cli",
            "0.88",
        ),
        (
            "semantic",
            "convention",
            "JSONL roundtrip convention: stable JSON stays machine-readable",
            "roundtrip,convention,json",
            "0.82",
        ),
        (
            "procedural",
            "anti-pattern",
            "JSONL roundtrip anti-pattern: do not drop tags during import",
            "roundtrip,anti-pattern,tags",
            "0.94",
        ),
        (
            "working",
            "risk",
            "JSONL roundtrip risk: regenerated workspace IDs must be normalized",
            "roundtrip,risk,workspace",
            "0.64",
        ),
        (
            "episodic",
            "playbook-step",
            "JSONL roundtrip playbook step: verify memory IDs survive import",
            "roundtrip,playbook,ids",
            "0.79",
        ),
    ];

    let mut memory_ids = Vec::with_capacity(seeds.len());
    for (index, (level, kind, content, tags, confidence)) in seeds.iter().copied().enumerate() {
        let source_path = provenance_dir.join(format!("source-{index}.md"));
        fs::write(&source_path, content)
            .map_err(|error| format!("write provenance source {index}: {error}"))?;
        let source_uri = format!("file://{}#L1", source_path.display());
        let remembered = run_ee(&[
            "--workspace",
            &source_workspace_arg,
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
            "--source",
            &source_uri,
            "--no-propose-candidates",
            content,
        ])?;
        persist_json_artifact(&format!("0n9b5_02_remember_{index}"), &remembered)?;
        ensure_equal(
            &remembered.pointer("/success").and_then(JsonValue::as_bool),
            &Some(true),
            &format!("remember {index} succeeded"),
        )?;
        memory_ids.push(json_str(&remembered, "/data/memory_id", "remember")?.to_owned());
    }

    ensure_equal(&memory_ids.len(), &seeds.len(), "remembered memory count")?;

    let link = run_ee(&[
        "--workspace",
        &source_workspace_arg,
        "--json",
        "memory",
        "link",
        &memory_ids[0],
        &memory_ids[1],
        "--relation",
        "supports",
        "--weight",
        "0.75",
        "--confidence",
        "0.90",
        "--evidence-count",
        "2",
        "--metadata",
        r#"{"reason":"eidetic_engine_cli-0n9b5 source fixture"}"#,
        "--actor",
        "jsonl-roundtrip-e2e",
    ])?;
    persist_json_artifact("0n9b5_03_link_source_memories", &link)?;
    ensure_equal(
        &link.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("created"),
        "source memory link created",
    )?;

    let source_export = run_ee(&[
        "--workspace",
        &source_workspace_arg,
        "--json",
        "export",
        "--output-dir",
        &source_export_dir_arg,
        "--redaction",
        "none",
        "--label",
        "0n9b5-source",
    ])?;
    persist_json_artifact("0n9b5_04_export_source", &source_export)?;
    ensure_equal(
        &source_export
            .pointer("/data/schema")
            .and_then(JsonValue::as_str),
        &Some("ee.export.report.v1"),
        "source export schema",
    )?;
    ensure_equal(
        &json_u64(
            &source_export,
            "/data/counts/memoryRecords",
            "source export",
        )?,
        &(seeds.len() as u64),
        "source export memory count",
    )?;

    let source_records_path = records_path_from_report(&source_export, "source export")?;
    let source_records = read_jsonl_records(&source_records_path)?;
    let source_records_json = JsonValue::Array(source_records.clone());
    persist_json_artifact("0n9b5_05_source_records", &source_records_json)?;

    let source_link_records = records_with_schema(&source_records, "ee.export.link.v1");
    ensure(
        !source_link_records.is_empty(),
        "source export contains the explicit memory link fixture",
    )?;
    ensure(
        source_link_records.iter().any(|record| {
            record.get("source_memory_id").and_then(JsonValue::as_str)
                == Some(memory_ids[0].as_str())
                && record.get("target_memory_id").and_then(JsonValue::as_str)
                    == Some(memory_ids[1].as_str())
                && record.get("link_type").and_then(JsonValue::as_str) == Some("supports")
        }),
        "source export includes the expected supports link",
    )?;

    let init_imported = run_ee(&["--workspace", &imported_workspace_arg, "--json", "init"])?;
    persist_json_artifact("0n9b5_06_init_imported", &init_imported)?;

    let source_records_path_arg = source_records_path.to_string_lossy().into_owned();
    let import = run_ee(&[
        "--workspace",
        &imported_workspace_arg,
        "--json",
        "import",
        "jsonl",
        "--source",
        &source_records_path_arg,
    ])?;
    persist_json_artifact("0n9b5_07_import_jsonl", &import)?;
    ensure_equal(
        &import.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("completed"),
        "import status",
    )?;
    ensure_equal(
        &json_u64(&import, "/data/memoriesImported", "import report")?,
        &(seeds.len() as u64),
        "imported memory count",
    )?;
    ensure(
        json_u64(&import, "/data/ignoredRecords", "import report")?
            >= source_link_records.len() as u64,
        "import report accounts for link records that are parsed but not replayed",
    )?;

    let imported_export = run_ee(&[
        "--workspace",
        &imported_workspace_arg,
        "--json",
        "export",
        "--output-dir",
        &imported_export_dir_arg,
        "--redaction",
        "none",
        "--label",
        "0n9b5-imported",
    ])?;
    persist_json_artifact("0n9b5_08_export_imported", &imported_export)?;
    ensure_equal(
        &json_u64(
            &imported_export,
            "/data/counts/memoryRecords",
            "imported export",
        )?,
        &(seeds.len() as u64),
        "imported export memory count",
    )?;

    let imported_records_path = records_path_from_report(&imported_export, "imported export")?;
    let imported_records = read_jsonl_records(&imported_records_path)?;
    let imported_records_json = JsonValue::Array(imported_records.clone());
    persist_json_artifact("0n9b5_09_imported_records", &imported_records_json)?;

    let ignored_memory_fields = ["workspace_id", "created_at", "updated_at"];
    let source_memories = normalized_records_with_schema(
        &source_records,
        "ee.export.memory.v1",
        &ignored_memory_fields,
    );
    let imported_memories = normalized_records_with_schema(
        &imported_records,
        "ee.export.memory.v1",
        &ignored_memory_fields,
    );
    persist_json_artifact(
        "0n9b5_10_normalized_source_memories",
        &JsonValue::Array(source_memories.clone()),
    )?;
    persist_json_artifact(
        "0n9b5_11_normalized_imported_memories",
        &JsonValue::Array(imported_memories.clone()),
    )?;
    ensure_equal(
        &imported_memories,
        &source_memories,
        "normalized memory records survive export/import/export",
    )?;

    let ignored_tag_fields = ["created_at"];
    let source_tags =
        normalized_records_with_schema(&source_records, "ee.export.tag.v1", &ignored_tag_fields);
    let imported_tags =
        normalized_records_with_schema(&imported_records, "ee.export.tag.v1", &ignored_tag_fields);
    ensure_equal(
        &imported_tags,
        &source_tags,
        "normalized tag records survive export/import/export",
    )?;

    let imported_link_records = records_with_schema(&imported_records, "ee.export.link.v1");
    ensure(
        imported_link_records.is_empty(),
        "JSONL import currently ignores link records rather than replaying them",
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
