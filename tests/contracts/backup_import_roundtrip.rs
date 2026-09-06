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

use ee::core::backup::{
    BackupCreateOptions, BackupInspectOptions, BackupVerifyOptions, create_backup, inspect_backup,
    verify_backup,
};
use ee::core::handoff::{
    CapsuleProfile, CreateOptions as HandoffCreateOptions, HANDOFF_CAPSULE_SCHEMA_V1,
    HANDOFF_CREATE_SCHEMA_V1, HANDOFF_INSPECT_SCHEMA_V1, HANDOFF_RESUME_SCHEMA_V1,
    InspectOptions as HandoffInspectOptions, ResumeOptions as HandoffResumeOptions, create_handoff,
    inspect_handoff, resume_handoff,
};
use ee::core::jsonl_import::{JsonlImportOptions, import_jsonl_records};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use ee::models::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_VERIFY_SCHEMA_V1,
    EXPORT_AUDIT_SCHEMA_V1, EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1,
    EXPORT_LINK_SCHEMA_V1, EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1,
    EXPORT_WORKSPACE_SCHEMA_V1, RedactionLevel,
};
use ee::output::jsonl_export::REDACTED_PATH_PLACEHOLDER;
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
                EXPORT_LINK_SCHEMA_V1 => Ok(format!(
                    "link|linkType={}",
                    json_str(record, "link_type")?,
                )),
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

fn expected_memory_projection(
    redaction_level: RedactionLevel,
    level: &str,
    kind: &str,
    content: &str,
) -> String {
    let exported_content = if redaction_level == RedactionLevel::Paranoid {
        "[REDACTED]"
    } else {
        content
    };
    format!(
        "memory|level={level}|kind={kind}|content={exported_content}|trustClass=human_explicit|redacted={}",
        redaction_level != RedactionLevel::None
    )
}

fn expected_tag_projection(redaction_level: RedactionLevel, tag: &str) -> String {
    let exported_tag = if redaction_level == RedactionLevel::Paranoid {
        let digest = blake3::hash(tag.as_bytes()).to_hex().to_string();
        format!("tag_{}", &digest[..16])
    } else {
        tag.to_string()
    };
    format!("tag|tag={exported_tag}")
}

fn expected_db_domain_records_projection(redaction_level: RedactionLevel) -> Vec<String> {
    vec![
        format!(
            "header|schema=ee.export.header.v1|formatVersion=1|scope=all|redaction={}|importSource=native|trustLevel=validated",
            redaction_level.as_str()
        ),
        "workspace|schema=ee.export.workspace.v1|hasName=true".to_string(),
        expected_memory_projection(
            redaction_level,
            "procedural",
            "rule",
            "Run cargo fmt --check before cutting a release.",
        ),
        expected_tag_projection(redaction_level, "formatting"),
        expected_tag_projection(redaction_level, "release"),
        expected_memory_projection(
            redaction_level,
            "semantic",
            "decision",
            "Adopt asupersync as the runtime substrate.",
        ),
        expected_tag_projection(redaction_level, "adr"),
        expected_tag_projection(redaction_level, "runtime"),
        expected_memory_projection(
            redaction_level,
            "episodic",
            "failure",
            "Release blocked when cargo test was skipped before tagging.",
        ),
        expected_tag_projection(redaction_level, "incident"),
        expected_tag_projection(redaction_level, "release"),
        expected_memory_projection(
            redaction_level,
            "semantic",
            "fact",
            "Memory ranking uses BLAKE3 of canonical content for dedupe.",
        ),
        expected_tag_projection(redaction_level, "blake3"),
        expected_tag_projection(redaction_level, "dedupe"),
        // The fixture remembers with auto_link enabled, and the release-themed
        // memories are similar enough that one deterministic auto-link (plus
        // its audit row) is part of the canonical shape.
        "link|linkType=related".to_string(),
        "audit|operation=memory.create|targetType=memory".to_string(),
        "audit|operation=memory.link.create|targetType=memory_link".to_string(),
        "audit|operation=memory.create|targetType=memory".to_string(),
        "audit|operation=memory.create|targetType=memory".to_string(),
        "audit|operation=memory.create|targetType=memory".to_string(),
        "footer|totalRecords=21|memoryRecords=4|linkRecords=1|tagRecords=8|auditRecords=5|success=true".to_string(),
    ]
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
fn handoff_create_inspect_resume_preserve_integrity_contract() -> TestResult {
    let src_dir = tempfile::tempdir().map_err(|error| format!("src tempdir: {error}"))?;
    let src_workspace = src_dir.path().to_path_buf();
    let src_db = src_workspace.join(".ee").join("ee.db");
    build_source_workspace(&src_workspace, &src_db)?;

    let handoff_dir = tempfile::tempdir().map_err(|error| format!("handoff tempdir: {error}"))?;
    let capsule_path = handoff_dir.path().join("capsule.json");
    let create_report = create_handoff(&HandoffCreateOptions {
        workspace: src_workspace.clone(),
        output: capsule_path.clone(),
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
        bind_to_machine: false,
        machine_salt_path: None,
        redaction_level: RedactionLevel::Standard,
    })
    .map_err(|error| format!("handoff create: {error:?}"))?;
    if create_report.schema != HANDOFF_CREATE_SCHEMA_V1 {
        return Err(format!(
            "handoff create schema drifted: {}",
            create_report.schema
        ));
    }
    if create_report.canonical_content_hash.len() != 16
        || !create_report
            .canonical_content_hash
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    {
        return Err(format!(
            "handoff canonical hash must be a 16-char hex prefix, got {}",
            create_report.canonical_content_hash
        ));
    }

    let create_json: Value =
        serde_json::from_str(&create_report.to_json()).map_err(|error| error.to_string())?;
    for field in [
        "secrets_redacted",
        "paths_redacted",
        "ids_redacted",
        "categories",
    ] {
        if create_json
            .pointer(&format!("/redaction_summary/{field}"))
            .is_none()
        {
            return Err(format!(
                "handoff create redaction_summary missing `{field}`"
            ));
        }
    }

    let capsule_text =
        fs::read_to_string(&capsule_path).map_err(|error| format!("read capsule: {error}"))?;
    let capsule: Value =
        serde_json::from_str(&capsule_text).map_err(|error| format!("parse capsule: {error}"))?;
    if capsule.get("schema").and_then(Value::as_str) != Some(HANDOFF_CAPSULE_SCHEMA_V1) {
        return Err(format!("handoff capsule schema drifted: {capsule}"));
    }
    let integrity = capsule
        .get("integrity")
        .ok_or_else(|| "handoff capsule missing integrity block".to_owned())?;
    let hmac = json_str(integrity, "hmac")?.to_owned();
    let hmac_prefix = json_str(integrity, "hmacPrefix")?;
    if !hmac
        .strip_prefix("base64url:")
        .is_some_and(|encoded| encoded.starts_with(hmac_prefix))
    {
        return Err(format!(
            "handoff hmacPrefix must prefix encoded hmac: prefix={hmac_prefix}, hmac={hmac}"
        ));
    }

    let inspect_report = inspect_handoff(&HandoffInspectOptions {
        path: capsule_path.clone(),
        verify_hash: true,
        check_evidence: true,
    })
    .map_err(|error| format!("handoff inspect: {error:?}"))?;
    if inspect_report.schema != HANDOFF_INSPECT_SCHEMA_V1 || !inspect_report.hash_valid {
        return Err(format!(
            "handoff inspect drifted: schema={}, hash_valid={}",
            inspect_report.schema, inspect_report.hash_valid
        ));
    }

    for run in 0..2 {
        let resume_report = resume_handoff(&HandoffResumeOptions {
            path: capsule_path.clone(),
            use_latest: false,
            workspace: src_workspace.clone(),
            max_sections: None,
            task_frame_id: None,
            bound_workspace_id: None,
            bound_workspace_identity: None,
            include_prompt_fragment: true,
            require_fresh: false,
            insecure_skip_hmac: false,
            machine_salt_path: None,
        })
        .map_err(|error| format!("handoff resume run {run}: {error:?}"))?;
        if resume_report.schema != HANDOFF_RESUME_SCHEMA_V1 {
            return Err(format!(
                "handoff resume schema drifted: {}",
                resume_report.schema
            ));
        }
        if resume_report
            .degradations
            .iter()
            .any(|degradation| degradation.code == "handoff_hmac_skipped")
        {
            return Err("handoff resume unexpectedly skipped HMAC verification".to_owned());
        }
        let capsule_after_resume: Value = serde_json::from_str(
            &fs::read_to_string(&capsule_path)
                .map_err(|error| format!("read capsule after resume {run}: {error}"))?,
        )
        .map_err(|error| format!("parse capsule after resume {run}: {error}"))?;
        let after_hmac = capsule_after_resume
            .pointer("/integrity/hmac")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("capsule missing hmac after resume run {run}"))?;
        if after_hmac != hmac.as_str() {
            return Err(format!(
                "handoff hmac changed across resume run {run}: before={hmac}, after={after_hmac}"
            ));
        }
    }
    Ok(())
}

#[test]
fn backup_inspect_and_verify_reports_are_content_addressed() -> TestResult {
    let src_dir = tempfile::tempdir().map_err(|error| format!("src tempdir: {error}"))?;
    let src_workspace = src_dir.path().to_path_buf();
    let src_db = src_workspace.join(".ee").join("ee.db");
    build_source_workspace(&src_workspace, &src_db)?;
    let sensitive_path = "/data/projects/private/customer-release-plan.md";
    let redaction_fixture = format!("Never export raw customer path {sensitive_path}.");
    remember_memory(&RememberMemoryOptions {
        workspace_path: &src_workspace,
        database_path: Some(&src_db),
        content: &redaction_fixture,
        workflow_id: None,
        level: "semantic",
        kind: "fact",
        tags: Some("privacy,export"),
        confidence: 0.9,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    })
    .map_err(|error| format!("remember redaction fixture: {error:?}"))?;

    let backup_dir = tempfile::tempdir().map_err(|error| format!("backup tempdir: {error}"))?;
    let create_report = create_backup(&BackupCreateOptions {
        workspace_path: src_workspace,
        database_path: Some(src_db),
        output_dir: Some(backup_dir.path().to_path_buf()),
        label: Some("inspect-verify-content-addressed".to_owned()),
        redaction_level: RedactionLevel::Standard,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| format!("backup: {error:?}"))?;
    if create_report.schema != BACKUP_CREATE_SCHEMA_V1 {
        return Err(format!(
            "backup create schema drifted: {}",
            create_report.schema
        ));
    }

    let records_path = Path::new(&create_report.records_path);
    let manifest_path = Path::new(&create_report.manifest_path);
    let expected_records_hash = file_blake3_hash(records_path)?;
    let expected_records_len = file_len(records_path)?;
    let expected_manifest_hash = file_blake3_hash(manifest_path)?;
    let expected_manifest_len = file_len(manifest_path)?;
    let backup_path = PathBuf::from(&create_report.backup_path);
    let records_text = fs::read_to_string(records_path)
        .map_err(|error| format!("read records {}: {error}", records_path.display()))?;
    if records_text.contains(sensitive_path) {
        return Err("backup export leaked a raw sensitive path".to_owned());
    }
    if !records_text.contains(REDACTED_PATH_PLACEHOLDER) {
        return Err("backup export did not keep an explicit path redaction marker".to_owned());
    }
    let record_schemas = canonicalized_records_jsonl(records_path)?
        .iter()
        .map(|record| json_str(record, "schema").map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    for expected_schema in [
        EXPORT_HEADER_SCHEMA_V1,
        EXPORT_MEMORY_SCHEMA_V1,
        EXPORT_TAG_SCHEMA_V1,
        EXPORT_AUDIT_SCHEMA_V1,
        EXPORT_FOOTER_SCHEMA_V1,
    ] {
        if !record_schemas
            .iter()
            .any(|schema| schema == expected_schema)
        {
            return Err(format!("backup export records missing {expected_schema}"));
        }
    }

    let inspect_report = inspect_backup(&BackupInspectOptions {
        backup_path: backup_path.clone(),
    })
    .map_err(|error| format!("inspect: {error:?}"))?;
    if inspect_report.schema != BACKUP_INSPECT_SCHEMA_V1 {
        return Err(format!(
            "backup inspect schema drifted: {}",
            inspect_report.schema
        ));
    }
    if inspect_report.backup_id != create_report.backup_id {
        return Err(format!(
            "inspect backupId drifted: create={}, inspect={}",
            create_report.backup_id, inspect_report.backup_id
        ));
    }
    if inspect_report.manifest_hash != expected_manifest_hash {
        return Err(format!(
            "inspect manifest hash mismatch: expected={expected_manifest_hash}, got={}",
            inspect_report.manifest_hash
        ));
    }
    if inspect_report.redaction_level.as_deref() != Some(RedactionLevel::Standard.as_str()) {
        return Err(format!(
            "inspect redaction level drifted: {:?}",
            inspect_report.redaction_level
        ));
    }
    if inspect_report.counts.memory_count != create_report.memory_count
        || inspect_report.counts.tag_count != create_report.tag_count
        || inspect_report.counts.audit_count != create_report.audit_count
    {
        return Err(format!(
            "inspect count summary drifted: inspect={:?}, create=(memories={}, tags={}, audits={})",
            inspect_report.counts,
            create_report.memory_count,
            create_report.tag_count,
            create_report.audit_count
        ));
    }

    let verify_report = verify_backup(&BackupVerifyOptions {
        workspace_path: src_workspace,
        backup_path,
    })
    .map_err(|error| format!("verify: {error:?}"))?;
    if verify_report.schema != BACKUP_VERIFY_SCHEMA_V1 {
        return Err(format!(
            "backup verify schema drifted: {}",
            verify_report.schema
        ));
    }
    if verify_report.status != "verified" {
        return Err(format!(
            "verify status drifted: status={}, issues={:?}",
            verify_report.status, verify_report.issues
        ));
    }
    if verify_report.manifest_hash != expected_manifest_hash {
        return Err(format!(
            "verify manifest hash mismatch: expected={expected_manifest_hash}, got={}",
            verify_report.manifest_hash
        ));
    }

    let mut checked_artifacts = verify_report
        .checked_artifacts
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
    checked_artifacts.sort();
    let expected = vec![
        format!(
            "manifest.json|manifest|{}|{}|true",
            expected_manifest_hash, expected_manifest_len
        ),
        format!(
            "records.jsonl|jsonl_export|{}|{}|true",
            expected_records_hash, expected_records_len
        ),
    ];
    if checked_artifacts != expected {
        return Err(format!(
            "verify checked artifact projection drifted:\nexpected: {expected:#?}\nactual:   {checked_artifacts:#?}"
        ));
    }
    Ok(())
}

#[test]
fn backup_records_jsonl_golden_shape_covers_all_redaction_levels() -> TestResult {
    let src_dir = tempfile::tempdir().map_err(|error| format!("src tempdir: {error}"))?;
    let src_workspace = src_dir.path().to_path_buf();
    let src_db = src_workspace.join(".ee").join("ee.db");
    build_source_workspace(&src_workspace, &src_db)?;

    for redaction_level in RedactionLevel::all() {
        let backup_dir = tempfile::tempdir()
            .map_err(|error| format!("backup tempdir for `{redaction_level}`: {error}"))?;
        let report = create_backup(&BackupCreateOptions {
            workspace_path: src_workspace.clone(),
            database_path: Some(src_db.clone()),
            output_dir: Some(backup_dir.path().to_path_buf()),
            label: Some(format!("golden-shape-{redaction_level}")),
            redaction_level: *redaction_level,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| format!("backup for `{redaction_level}`: {error:?}"))?;

        let actual = records_contract_projection(Path::new(&report.records_path))?;
        let expected = expected_db_domain_records_projection(*redaction_level);
        if actual != expected {
            return Err(format!(
                "backup records.jsonl golden projection drifted for `{redaction_level}`:\nexpected: {expected:#?}\nactual:   {actual:#?}"
            ));
        }
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
    ) != (4, 1, 8, 5)
    {
        return Err(format!(
            "backup report counts drifted: memories={}, links={}, tags={}, audits={}",
            report.memory_count, report.link_count, report.tag_count, report.audit_count
        ));
    }
    let expected = expected_db_domain_records_projection(RedactionLevel::None);
    if actual != expected {
        return Err(format!(
            "backup records.jsonl golden projection drifted:\nexpected: {expected:#?}\nactual:   {actual:#?}"
        ));
    }
    Ok(())
}
