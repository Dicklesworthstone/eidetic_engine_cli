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

use std::fs;
use std::path::{Path, PathBuf};

use ee::core::backup::{BackupCreateOptions, create_backup};
use ee::core::jsonl_import::{JsonlImportOptions, import_jsonl_records};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use ee::models::{
    EXPORT_AUDIT_SCHEMA_V1, EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1, EXPORT_WORKSPACE_SCHEMA_V1, RedactionLevel,
};
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

fn file_blake3_hash(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn file_len(path: &Path) -> Result<u64, String> {
    Ok(fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .len())
}

fn json_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("record missing string field `{key}`: {value}"))
}

fn json_u64(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("record missing integer field `{key}`: {value}"))
}

fn json_bool(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("record missing bool field `{key}`: {value}"))
}

fn records_contract_projection(records_path: &Path) -> Result<Vec<String>, String> {
    canonicalized_records_jsonl(records_path)?
        .iter()
        .map(|record| {
            let schema = json_str(record, "schema")?;
            match schema {
                EXPORT_HEADER_SCHEMA_V1 => Ok(format!(
                    "header|schema={schema}|formatVersion={}|scope={}|redaction={}|importSource={}|trustLevel={}",
                    json_u64(record, "format_version")?,
                    json_str(record, "export_scope")?,
                    json_str(record, "redaction_level")?,
                    json_str(record, "import_source")?,
                    json_str(record, "trust_level")?,
                )),
                EXPORT_WORKSPACE_SCHEMA_V1 => Ok(format!(
                    "workspace|schema={schema}|hasName={}",
                    record.get("name").and_then(Value::as_str).is_some(),
                )),
                EXPORT_MEMORY_SCHEMA_V1 => Ok(format!(
                    "memory|level={}|kind={}|content={}|trustClass={}|redacted={}",
                    json_str(record, "level")?,
                    json_str(record, "kind")?,
                    json_str(record, "content")?,
                    json_str(record, "trust_class")?,
                    json_bool(record, "redacted")?,
                )),
                EXPORT_TAG_SCHEMA_V1 => Ok(format!("tag|tag={}", json_str(record, "tag")?)),
                EXPORT_AUDIT_SCHEMA_V1 => Ok(format!(
                    "audit|operation={}|targetType={}",
                    json_str(record, "operation")?,
                    json_str(record, "target_type")?,
                )),
                EXPORT_FOOTER_SCHEMA_V1 => Ok(format!(
                    "footer|totalRecords={}|memoryRecords={}|linkRecords={}|tagRecords={}|auditRecords={}|success={}",
                    json_u64(record, "total_records")?,
                    json_u64(record, "memory_count")?,
                    json_u64(record, "link_count")?,
                    json_u64(record, "tag_count")?,
                    json_u64(record, "audit_count")?,
                    json_bool(record, "success")?,
                )),
                _ => Ok(format!("unknown|schema={schema}")),
            }
        })
        .collect()
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
            allow_secret_mention: false,
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
        include_derived: false,
        include_graph_cache: false,
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
fn backup_export_import_roundtrip_imports_all_redaction_levels() -> TestResult {
    for level in RedactionLevel::all() {
        let fixture = run_roundtrip(*level)
            .map_err(|error| format!("round-trip for redaction level `{level}`: {error}"))?;
        if fixture._memories_imported == 0 {
            return Err(format!(
                "round-trip for redaction level `{level}` imported 0 memories"
            ));
        }

        let dst_conn = DbConnection::open_file(&fixture.dst_db)
            .map_err(|error| format!("dst db open for `{level}`: {error}"))?;
        let dst_workspace_id = single_workspace_id(&dst_conn)?;
        let dst_count = dst_conn
            .list_memories(&dst_workspace_id, None, true)
            .map_err(|error| format!("dst list_memories for `{level}`: {error}"))?
            .len();
        if dst_count != fixture._memories_imported as usize {
            return Err(format!(
                "round-trip for redaction level `{level}` changed imported memory count: \
                 report={}, db={dst_count}",
                fixture._memories_imported
            ));
        }
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
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| format!("backup a: {error:?}"))?;
    let report_b = create_backup(&BackupCreateOptions {
        workspace_path: src_workspace.clone(),
        database_path: Some(src_db.clone()),
        output_dir: Some(backup_dir_b.path().to_path_buf()),
        label: Some("b".to_owned()),
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
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

#[test]
fn backup_report_artifacts_match_written_files() -> TestResult {
    let src_dir = tempfile::tempdir().map_err(|error| format!("src tempdir: {error}"))?;
    let src_workspace = src_dir.path().to_path_buf();
    let src_db = src_workspace.join(".ee").join("ee.db");
    build_source_workspace(&src_workspace, &src_db)?;

    let backup_dir = tempfile::tempdir().map_err(|error| format!("backup tempdir: {error}"))?;
    let report = create_backup(&BackupCreateOptions {
        workspace_path: src_workspace,
        database_path: Some(src_db),
        output_dir: Some(backup_dir.path().to_path_buf()),
        label: Some("artifact-hashes".to_owned()),
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| format!("backup: {error:?}"))?;

    let records_path = Path::new(&report.records_path);
    let manifest_path = Path::new(&report.manifest_path);
    let expected_records_hash = file_blake3_hash(records_path)?;
    let expected_manifest_hash = file_blake3_hash(manifest_path)?;

    if report.records_hash.as_deref() != Some(expected_records_hash.as_str()) {
        return Err(format!(
            "records hash mismatch: report={:?}, actual={expected_records_hash}",
            report.records_hash
        ));
    }
    if report.manifest_hash.as_deref() != Some(expected_manifest_hash.as_str()) {
        return Err(format!(
            "manifest hash mismatch: report={:?}, actual={expected_manifest_hash}",
            report.manifest_hash
        ));
    }

    let mut artifact_projection = report
        .artifacts
        .iter()
        .map(|artifact| {
            format!(
                "{}|{}|{}|{}|{}",
                artifact.path,
                artifact.kind,
                artifact.hash.as_deref().unwrap_or("[missing-hash]"),
                artifact.size_bytes.unwrap_or(0),
                artifact.required
            )
        })
        .collect::<Vec<_>>();
    artifact_projection.sort();

    let expected = vec![
        format!(
            "manifest.json|manifest|{}|{}|true",
            expected_manifest_hash,
            file_len(manifest_path)?
        ),
        format!(
            "records.jsonl|jsonl_export|{}|{}|true",
            expected_records_hash,
            file_len(records_path)?
        ),
    ];
    if artifact_projection != expected {
        return Err(format!(
            "backup artifact projection drifted:\nexpected: {expected:#?}\nactual:   {artifact_projection:#?}"
        ));
    }

    Ok(())
}

#[test]
fn backup_records_jsonl_matches_db_domain_golden_shape() -> TestResult {
    let src_dir = tempfile::tempdir().map_err(|error| format!("src tempdir: {error}"))?;
    let src_workspace = src_dir.path().to_path_buf();
    let src_db = src_workspace.join(".ee").join("ee.db");
    build_source_workspace(&src_workspace, &src_db)?;

    let backup_dir = tempfile::tempdir().map_err(|error| format!("backup tempdir: {error}"))?;
    let report = create_backup(&BackupCreateOptions {
        workspace_path: src_workspace,
        database_path: Some(src_db),
        output_dir: Some(backup_dir.path().to_path_buf()),
        label: Some("golden-shape".to_owned()),
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| format!("backup: {error:?}"))?;

    let actual = records_contract_projection(Path::new(&report.records_path))?;
    if report.total_records != actual.len() as u64 {
        return Err(format!(
            "backup report total_records disagrees with records.jsonl line count: report={}, lines={}",
            report.total_records,
            actual.len()
        ));
    }
    if (
        report.memory_count,
        report.link_count,
        report.tag_count,
        report.audit_count,
    ) != (4, 0, 8, 4)
    {
        return Err(format!(
            "backup report counts drifted: memories={}, links={}, tags={}, audits={}",
            report.memory_count, report.link_count, report.tag_count, report.audit_count
        ));
    }
    let expected = vec![
        "header|schema=ee.export.header.v1|formatVersion=1|scope=all|redaction=none|importSource=native|trustLevel=validated",
        "workspace|schema=ee.export.workspace.v1|hasName=true",
        "memory|level=procedural|kind=rule|content=Run cargo fmt --check before cutting a release.|trustClass=human_explicit|redacted=false",
        "tag|tag=formatting",
        "tag|tag=release",
        "memory|level=semantic|kind=decision|content=Adopt asupersync as the runtime substrate.|trustClass=human_explicit|redacted=false",
        "tag|tag=adr",
        "tag|tag=runtime",
        "memory|level=episodic|kind=failure|content=Release blocked when cargo test was skipped before tagging.|trustClass=human_explicit|redacted=false",
        "tag|tag=incident",
        "tag|tag=release",
        "memory|level=semantic|kind=fact|content=Memory ranking uses BLAKE3 of canonical content for dedupe.|trustClass=human_explicit|redacted=false",
        "tag|tag=blake3",
        "tag|tag=dedupe",
        "audit|operation=memory.create|targetType=memory",
        "audit|operation=memory.create|targetType=memory",
        "audit|operation=memory.create|targetType=memory",
        "audit|operation=memory.create|targetType=memory",
        "footer|totalRecords=18|memoryRecords=4|linkRecords=0|tagRecords=8|auditRecords=4|success=true",
    ];
    if actual != expected {
        return Err(format!(
            "backup records.jsonl golden projection drifted:\nexpected: {expected:#?}\nactual:   {actual:#?}"
        ));
    }
    Ok(())
}
