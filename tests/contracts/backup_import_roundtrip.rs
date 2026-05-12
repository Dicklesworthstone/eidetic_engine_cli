//! L2 contract test (eidetic_engine_cli bd-17c65.12.3).
//!
//! Workspace round-trip determinism: a workspace exported via
//! `create_backup` and re-imported via `import_jsonl_records` must
//! produce a target workspace whose memory state is content-equivalent
//! to the source.
//!
//! The equivalence check uses a **workspace_state_hash** that hashes
//! the canonical content set of each workspace, stripping volatile
//! fields (timestamps, audit IDs, etc.) so the round-trip can be
//! validated even though re-import generates new audit row IDs and
//! workspace IDs.
//!
//! Two scenarios:
//! 1. `RedactionLevel::None` — full fidelity round-trip.
//! 2. `RedactionLevel::Standard` — redaction preserved on re-import.
//!
//! Both scenarios use the in-process API (create_backup +
//! import_jsonl_records), not the CLI binary, to keep the test fast
//! and to expose the underlying contract directly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use ee::core::backup::{BackupCreateOptions, create_backup};
use ee::core::jsonl_import::{JsonlImportOptions, import_jsonl_records};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use ee::models::{EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1, RedactionLevel};
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn single_workspace_id(conn: &DbConnection) -> Result<String, String> {
    let workspaces = conn
        .list_workspaces()
        .map_err(|error| format!("list_workspaces: {error}"))?;
    if workspaces.len() != 1 {
        return Err(format!(
            "expected exactly one workspace in fixture DB, got {}",
            workspaces.len()
        ));
    }
    Ok(workspaces[0].id.clone())
}

/// Compute a deterministic state hash over the memory content set of a
/// workspace. Skips volatile fields (timestamps, audit IDs, workspace_id,
/// row IDs assigned by SQLite) so the hash is byte-stable across export
/// → import cycles where re-import allocates fresh IDs.
fn workspace_state_hash(database_path: &Path) -> Result<String, String> {
    let conn = DbConnection::open_file(database_path)
        .map_err(|error| format!("open db {}: {error}", database_path.display()))?;
    let workspace_id = single_workspace_id(&conn)?;
    let memories = conn
        .list_memories(&workspace_id, None, true)
        .map_err(|error| format!("list_memories: {error}"))?;

    // Project each memory to (level, kind, content, sorted tags), drop
    // every volatile field, sort the projection set, then BLAKE3-hash
    // the resulting stable bytes.
    let mut projections: Vec<String> = memories
        .iter()
        .map(|m| {
            // tags() is volatile in row order between exports? Use the
            // stored ID-keyed lookup so order is from the same source.
            let tags = conn.get_memory_tags(&m.id).unwrap_or_default();
            let mut tags_sorted: Vec<String> = tags;
            tags_sorted.sort();
            format!(
                "level={};kind={};content={};tags={}",
                m.level,
                m.kind,
                m.content,
                tags_sorted.join(",")
            )
        })
        .collect();
    projections.sort();
    let joined = projections.join("\n");
    let digest = blake3::hash(joined.as_bytes()).to_hex();
    Ok(format!("blake3:{}", digest.as_str()))
}

fn canonicalized_records_jsonl(records_path: &Path) -> Result<Vec<Value>, String> {
    let raw = std::fs::read_to_string(records_path)
        .map_err(|error| format!("read records {}: {error}", records_path.display()))?;
    let mut records = Vec::new();
    for (line_index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "parse records {} line {}: {error}",
                records_path.display(),
                line_index + 1
            )
        })?;
        match value.get("schema").and_then(Value::as_str) {
            Some(EXPORT_HEADER_SCHEMA_V1) => {
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| "export header record is not a JSON object".to_string())?;
                object.insert(
                    "created_at".to_string(),
                    Value::String("[created_at]".to_string()),
                );
                object.insert(
                    "export_id".to_string(),
                    Value::String("[export_id]".to_string()),
                );
            }
            Some(EXPORT_FOOTER_SCHEMA_V1) => {
                let object = value
                    .as_object_mut()
                    .ok_or_else(|| "export footer record is not a JSON object".to_string())?;
                object.insert(
                    "completed_at".to_string(),
                    Value::String("[completed_at]".to_string()),
                );
                object.insert(
                    "export_id".to_string(),
                    Value::String("[export_id]".to_string()),
                );
            }
            _ => {}
        }
        records.push(value);
    }
    Ok(records)
}

struct RoundtripFixture {
    _src_dir: TempDir,
    _dst_dir: TempDir,
    _backup_dir: TempDir,
    src_db: PathBuf,
    dst_db: PathBuf,
    _dst_workspace: PathBuf,
    _backup_records_path: PathBuf,
    _memories_imported: u32,
}

fn build_source_workspace(workspace: &Path, database: &Path) -> Result<(), String> {
    std::fs::create_dir_all(database.parent().expect("db parent"))
        .map_err(|error| format!("create .ee: {error}"))?;
    let conn = DbConnection::open_file(database).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    let memories = [
        (
            "Run cargo fmt --check before cutting a release.",
            "procedural",
            "rule",
            Some("release,formatting"),
        ),
        (
            "Adopt asupersync as the runtime substrate.",
            "semantic",
            "decision",
            Some("runtime,adr"),
        ),
        (
            "Release blocked when cargo test was skipped before tagging.",
            "episodic",
            "failure",
            Some("release,incident"),
        ),
        (
            "Memory ranking uses BLAKE3 of canonical content for dedupe.",
            "semantic",
            "fact",
            Some("blake3,dedupe"),
        ),
    ];
    for (content, level, kind, tags) in &memories {
        remember_memory(&RememberMemoryOptions {
            workspace_path: workspace,
            database_path: Some(database),
            content,
            workflow_id: None,
            level,
            kind,
            tags: *tags,
            confidence: 0.85,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
        })
        .map_err(|error| format!("remember `{content}`: {error:?}"))?;
    }
    Ok(())
}

fn run_roundtrip(redaction_level: RedactionLevel) -> Result<RoundtripFixture, String> {
    // -- Source workspace --------------------------------------------------
    let src_dir = tempfile::tempdir().map_err(|error| format!("src tempdir: {error}"))?;
    let src_workspace = src_dir.path().to_path_buf();
    let src_db = src_workspace.join(".ee").join("ee.db");
    build_source_workspace(&src_workspace, &src_db)?;

    // -- Backup ------------------------------------------------------------
    let backup_dir = tempfile::tempdir().map_err(|error| format!("backup tempdir: {error}"))?;
    let backup_report = create_backup(&BackupCreateOptions {
        workspace_path: src_workspace.clone(),
        database_path: Some(src_db.clone()),
        output_dir: Some(backup_dir.path().to_path_buf()),
        label: Some("l2-roundtrip".to_owned()),
        redaction_level,
        dry_run: false,
    })
    .map_err(|error| format!("create_backup: {error:?}"))?;
    let backup_records_path = PathBuf::from(&backup_report.records_path);
    if !backup_records_path.exists() {
        return Err(format!(
            "backup records.jsonl not written at {}",
            backup_records_path.display()
        ));
    }

    // -- Destination workspace --------------------------------------------
    let dst_dir = tempfile::tempdir().map_err(|error| format!("dst tempdir: {error}"))?;
    let dst_workspace = dst_dir.path().to_path_buf();
    let dst_db = dst_workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(dst_db.parent().expect("dst db parent"))
        .map_err(|error| format!("dst .ee dir: {error}"))?;
    let dst_conn =
        DbConnection::open_file(&dst_db).map_err(|error| format!("dst db open: {error}"))?;
    dst_conn
        .migrate()
        .map_err(|error| format!("dst db migrate: {error}"))?;
    drop(dst_conn);

    let import_report = import_jsonl_records(&JsonlImportOptions {
        workspace_path: dst_workspace.clone(),
        database_path: Some(dst_db.clone()),
        source_path: backup_records_path.clone(),
        dry_run: false,
    })
    .map_err(|error| format!("import_jsonl_records: {error:?}"))?;
    if import_report.memories_imported == 0 {
        return Err(format!(
            "import_jsonl_records imported 0 memories: status={}, issues={:?}",
            import_report.status, import_report.issues
        ));
    }

    Ok(RoundtripFixture {
        _src_dir: src_dir,
        _dst_dir: dst_dir,
        _backup_dir: backup_dir,
        src_db,
        dst_db,
        _dst_workspace: dst_workspace,
        _backup_records_path: backup_records_path,
        _memories_imported: import_report.memories_imported,
    })
}

#[test]
fn backup_export_import_roundtrip_preserves_workspace_state_hash() -> TestResult {
    let fixture = run_roundtrip(RedactionLevel::None)?;

    let src_hash = workspace_state_hash(&fixture.src_db)?;
    let dst_hash = workspace_state_hash(&fixture.dst_db)?;

    if src_hash != dst_hash {
        return Err(format!(
            "round-trip workspace state hash mismatch (no redaction):\n\
             source: {src_hash}\n\
             dest:   {dst_hash}"
        ));
    }

    // Sanity: confirm the hash represents real content, not the empty
    // string. (Both empty workspaces would technically pass the above
    // assertion.)
    if src_hash.ends_with("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262") {
        return Err(
            "workspace_state_hash matched the empty-input BLAKE3 — fixture didn't seed".to_string(),
        );
    }
    Ok(())
}

#[test]
fn backup_export_import_roundtrip_with_standard_redaction_remains_deterministic() -> TestResult {
    // Standard redaction intentionally masks source identifiers in the
    // JSONL stream. Import regenerates stable local memory IDs from the
    // redacted records, so the content/tag state remains round-trippable
    // without exposing original identifiers.
    let fixture = run_roundtrip(RedactionLevel::Standard)?;
    let src_hash = workspace_state_hash(&fixture.src_db)?;
    let dst_hash = workspace_state_hash(&fixture.dst_db)?;
    if src_hash != dst_hash {
        return Err(format!(
            "standard-redaction round-trip workspace state hash mismatch:\n\
             source: {src_hash}\n\
             dest:   {dst_hash}"
        ));
    }

    let src_conn = DbConnection::open_file(&fixture.src_db)
        .map_err(|error| format!("src db open: {error}"))?;
    let dst_conn = DbConnection::open_file(&fixture.dst_db)
        .map_err(|error| format!("dst db open: {error}"))?;
    let src_workspace_id = single_workspace_id(&src_conn)?;
    let dst_workspace_id = single_workspace_id(&dst_conn)?;
    let src_count = src_conn
        .list_memories(&src_workspace_id, None, true)
        .map_err(|error| format!("src list_memories: {error}"))?
        .len();
    let dst_count = dst_conn
        .list_memories(&dst_workspace_id, None, true)
        .map_err(|error| format!("dst list_memories: {error}"))?
        .len();
    if src_count != dst_count {
        return Err(format!(
            "redacted round-trip changed memory count: src={src_count}, dst={dst_count}"
        ));
    }
    if dst_count == 0 {
        return Err("redacted round-trip imported 0 memories".to_string());
    }
    Ok(())
}

#[test]
fn backup_records_jsonl_is_deterministic_across_two_exports() -> TestResult {
    // Raw backup records include run-specific provenance fields
    // (backup_id and timestamps). The deterministic contract for the
    // exported data records is equality after normalizing those explicit
    // header/footer provenance fields.
    let src_dir = tempfile::tempdir().map_err(|error| format!("src tempdir: {error}"))?;
    let src_workspace = src_dir.path().to_path_buf();
    let src_db = src_workspace.join(".ee").join("ee.db");
    build_source_workspace(&src_workspace, &src_db)?;

    let backup_dir_a = tempfile::tempdir().map_err(|error| format!("dir a: {error}"))?;
    let backup_dir_b = tempfile::tempdir().map_err(|error| format!("dir b: {error}"))?;
    let report_a = create_backup(&BackupCreateOptions {
        workspace_path: src_workspace.clone(),
        database_path: Some(src_db.clone()),
        output_dir: Some(backup_dir_a.path().to_path_buf()),
        label: Some("a".to_owned()),
        redaction_level: RedactionLevel::None,
        dry_run: false,
    })
    .map_err(|error| format!("backup a: {error:?}"))?;
    let report_b = create_backup(&BackupCreateOptions {
        workspace_path: src_workspace.clone(),
        database_path: Some(src_db.clone()),
        output_dir: Some(backup_dir_b.path().to_path_buf()),
        label: Some("b".to_owned()),
        redaction_level: RedactionLevel::None,
        dry_run: false,
    })
    .map_err(|error| format!("backup b: {error:?}"))?;

    let records_a = canonicalized_records_jsonl(Path::new(&report_a.records_path))?;
    let records_b = canonicalized_records_jsonl(Path::new(&report_b.records_path))?;
    if records_a != records_b {
        return Err(format!(
            "canonical records.jsonl diverged across two exports of the same workspace:\n\
             a: {records_a:#?}\n\
             b: {records_b:#?}"
        ));
    }
    Ok(())
}
