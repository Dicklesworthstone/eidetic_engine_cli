//! Conformance matrix for the handoff/export/backup artifact family.
//!
//! The covered surfaces are side-path artifacts with stable JSON contracts:
//! handoff preview/create/inspect/resume, export JSONL records, and backup
//! create/inspect/verify. These tests intentionally use in-process APIs so the
//! contract checks stay focused on schema and artifact behavior rather than
//! process wiring.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ee::core::backup::{
    BackupCreateOptions, BackupInspectOptions, BackupVerifyOptions, create_backup, inspect_backup,
    verify_backup,
};
use ee::core::handoff::{
    CapsuleProfile, CreateOptions as HandoffCreateOptions, HANDOFF_CAPSULE_SCHEMA_V1,
    HANDOFF_CREATE_SCHEMA_V1, HANDOFF_INSPECT_SCHEMA_V1, HANDOFF_PREVIEW_SCHEMA_V1,
    HANDOFF_RESUME_SCHEMA_V1, InspectOptions as HandoffInspectOptions,
    PreviewOptions as HandoffPreviewOptions, ResumeOptions as HandoffResumeOptions, create_handoff,
    inspect_handoff, preview_handoff, resume_handoff,
};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;
use ee::models::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_VERIFY_SCHEMA_V1,
    EXPORT_AUDIT_SCHEMA_V1, EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1, EXPORT_WORKSPACE_SCHEMA_V1, RedactionLevel,
};
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

struct Fixture {
    _dir: TempDir,
    workspace: PathBuf,
    database: PathBuf,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn build_fixture() -> Result<Fixture, String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let database = workspace.join(".ee").join("ee.db");
    fs::create_dir_all(database.parent().expect("database parent"))
        .map_err(|error| format!("create .ee: {error}"))?;
    let conn = DbConnection::open_file(&database).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate db: {error}"))?;
    drop(conn);

    for (content, level, kind, tags, allow_secret_mention) in [
        (
            "Run cargo fmt --check before release.",
            "procedural",
            "rule",
            Some("release,formatting"),
            false,
        ),
        (
            "Adopt asupersync for runtime supervision.",
            "semantic",
            "decision",
            Some("runtime,adr"),
            false,
        ),
        (
            "Secret export conformance token=sk-ant-api03-conformanceconformanceconformanceconformance",
            "episodic",
            "failure",
            Some("redaction,export"),
            true,
        ),
    ] {
        remember_memory(&RememberMemoryOptions {
            workspace_path: &workspace,
            database_path: Some(&database),
            content,
            workflow_id: None,
            level,
            kind,
            tags,
            confidence: 0.9,
            source: None,
            allow_secret_mention,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
        })
        .map_err(|error| format!("remember `{content}`: {error:?}"))?;
    }

    Ok(Fixture {
        _dir: dir,
        workspace,
        database,
    })
}

fn create_capsule(fixture: &Fixture, name: &str) -> Result<(PathBuf, Value), String> {
    let output = fixture.workspace.join(format!("{name}.handoff.json"));
    let report = create_handoff(&HandoffCreateOptions {
        workspace: fixture.workspace.clone(),
        output: output.clone(),
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
        bind_to_machine: false,
        machine_salt_path: None,
        redaction_level: RedactionLevel::Standard,
    })
    .map_err(|error| format!("create handoff {name}: {error:?}"))?;
    ensure(
        report.schema == HANDOFF_CREATE_SCHEMA_V1,
        format!("create schema drifted: {}", report.schema),
    )?;

    let body = fs::read_to_string(&output).map_err(|error| format!("read capsule: {error}"))?;
    let capsule =
        serde_json::from_str(&body).map_err(|error| format!("parse capsule: {error}\n{body}"))?;
    Ok((output, capsule))
}

fn create_backup_fixture(fixture: &Fixture, level: RedactionLevel) -> Result<Value, String> {
    let backup_root = fixture.workspace.join(format!("backup-{}", level.as_str()));
    let report = create_backup(&BackupCreateOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: Some(fixture.database.clone()),
        output_dir: Some(backup_root),
        label: Some("conformance".to_owned()),
        redaction_level: level,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| format!("create backup: {error:?}"))?;
    Ok(report.data_json())
}

fn read_records(path: &Path) -> Result<Vec<Value>, String> {
    let raw = fs::read_to_string(path).map_err(|error| format!("read records: {error}"))?;
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .map_err(|error| format!("parse records line {}: {error}", index + 1))
        })
        .collect()
}

fn file_blake3(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

#[test]
fn schema_family_covers_handoff_export_and_backup_surfaces() -> TestResult {
    let fixture = build_fixture()?;

    let preview = preview_handoff(&HandoffPreviewOptions {
        workspace: fixture.workspace.clone(),
        profile: CapsuleProfile::Resume,
        since: None,
        include_estimates: true,
        task_frame_id: None,
    })
    .map_err(|error| format!("preview handoff: {error:?}"))?;
    ensure(
        preview.schema == HANDOFF_PREVIEW_SCHEMA_V1,
        format!("preview schema drifted: {}", preview.schema),
    )?;

    let (capsule_path, capsule) = create_capsule(&fixture, "schema-family")?;
    ensure(
        capsule.get("schema").and_then(Value::as_str) == Some(HANDOFF_CAPSULE_SCHEMA_V1),
        "capsule schema drifted",
    )?;

    let inspected = inspect_handoff(&HandoffInspectOptions {
        path: capsule_path.clone(),
        verify_hash: false,
        check_evidence: true,
    })
    .map_err(|error| format!("inspect handoff: {error:?}"))?;
    ensure(
        inspected.schema == HANDOFF_INSPECT_SCHEMA_V1,
        format!("inspect schema drifted: {}", inspected.schema),
    )?;

    let resumed = resume_handoff(&HandoffResumeOptions {
        path: capsule_path,
        workspace: fixture.workspace.clone(),
        ..HandoffResumeOptions::default()
    })
    .map_err(|error| format!("resume handoff: {error:?}"))?;
    ensure(
        resumed.schema == HANDOFF_RESUME_SCHEMA_V1,
        format!("resume schema drifted: {}", resumed.schema),
    )?;

    let backup = create_backup_fixture(&fixture, RedactionLevel::None)?;
    ensure(
        backup.get("schema").and_then(Value::as_str) == Some(BACKUP_CREATE_SCHEMA_V1),
        "backup create schema drifted",
    )?;
    let backup_path = backup
        .get("backupPath")
        .and_then(Value::as_str)
        .ok_or_else(|| "backupPath missing".to_owned())?;
    let inspected_backup = inspect_backup(&BackupInspectOptions {
        backup_path: PathBuf::from(backup_path),
    })
    .map_err(|error| format!("inspect backup: {error:?}"))?;
    ensure(
        inspected_backup.schema == BACKUP_INSPECT_SCHEMA_V1,
        format!("backup inspect schema drifted: {}", inspected_backup.schema),
    )?;
    let verified_backup = verify_backup(&BackupVerifyOptions {
        backup_path: PathBuf::from(backup_path),
    })
    .map_err(|error| format!("verify backup: {error:?}"))?;
    ensure(
        verified_backup.schema == BACKUP_VERIFY_SCHEMA_V1,
        format!("backup verify schema drifted: {}", verified_backup.schema),
    )?;

    let records_path = backup
        .get("recordsPath")
        .and_then(Value::as_str)
        .ok_or_else(|| "recordsPath missing".to_owned())?;
    let schemas = read_records(Path::new(records_path))?
        .into_iter()
        .filter_map(|record| {
            record
                .get("schema")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<BTreeSet<_>>();
    for expected in [
        EXPORT_HEADER_SCHEMA_V1,
        EXPORT_WORKSPACE_SCHEMA_V1,
        EXPORT_MEMORY_SCHEMA_V1,
        EXPORT_TAG_SCHEMA_V1,
        EXPORT_AUDIT_SCHEMA_V1,
        EXPORT_FOOTER_SCHEMA_V1,
    ] {
        ensure(
            schemas.contains(expected),
            format!("export records missing schema `{expected}`; got {schemas:?}"),
        )?;
    }

    Ok(())
}

#[test]
fn handoff_hmac_and_canonical_hashes_are_stable_for_repeated_reads() -> TestResult {
    let fixture = build_fixture()?;
    let (first_path, first_capsule) = create_capsule(&fixture, "first")?;
    let (second_path, _second_capsule) = create_capsule(&fixture, "second")?;

    let first_report = inspect_handoff(&HandoffInspectOptions {
        path: first_path.clone(),
        verify_hash: false,
        check_evidence: true,
    })
    .map_err(|error| format!("inspect first handoff: {error:?}"))?;
    let second_report = inspect_handoff(&HandoffInspectOptions {
        path: second_path,
        verify_hash: false,
        check_evidence: true,
    })
    .map_err(|error| format!("inspect second handoff: {error:?}"))?;
    ensure(
        first_report.capsule_schema == HANDOFF_CAPSULE_SCHEMA_V1
            && second_report.capsule_schema == HANDOFF_CAPSULE_SCHEMA_V1,
        "inspect must preserve capsule schema identity",
    )?;

    let integrity = first_capsule
        .get("integrity")
        .and_then(Value::as_object)
        .ok_or_else(|| "capsule integrity block missing".to_owned())?;
    let hmac = integrity
        .get("hmac")
        .and_then(Value::as_str)
        .ok_or_else(|| "integrity.hmac missing".to_owned())?;
    let hmac_prefix = integrity
        .get("hmacPrefix")
        .and_then(Value::as_str)
        .ok_or_else(|| "integrity.hmacPrefix missing".to_owned())?;
    let encoded = hmac
        .strip_prefix("base64url:")
        .ok_or_else(|| format!("hmac must be base64url-prefixed: {hmac}"))?;
    ensure(
        encoded.starts_with(hmac_prefix),
        format!("hmacPrefix `{hmac_prefix}` must match full hmac `{hmac}`"),
    )?;

    let before_resume_hmac = hmac.to_owned();
    for run in 0..2 {
        let resumed = resume_handoff(&HandoffResumeOptions {
            path: first_path.clone(),
            workspace: fixture.workspace.clone(),
            ..HandoffResumeOptions::default()
        })
        .map_err(|error| format!("resume run {run}: {error:?}"))?;
        ensure(
            resumed.capsule_id
                == first_capsule
                    .get("capsule_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            format!("resume run {run} capsule id drifted"),
        )?;
        ensure(
            !resumed
                .degradations
                .iter()
                .any(|degradation| degradation.code == "handoff_hmac_skipped"),
            format!("resume run {run} skipped HMAC verification"),
        )?;
    }
    let after_resume = fs::read_to_string(&first_path)
        .map_err(|error| format!("read capsule after resume: {error}"))?;
    let after_resume_capsule: Value = serde_json::from_str(&after_resume)
        .map_err(|error| format!("parse capsule after resume: {error}"))?;
    let after_resume_hmac = after_resume_capsule
        .pointer("/integrity/hmac")
        .and_then(Value::as_str)
        .ok_or_else(|| "integrity.hmac missing after resume".to_owned())?;
    ensure(
        after_resume_hmac == before_resume_hmac,
        "resume must be read-only and preserve the HMAC signature",
    )?;

    Ok(())
}

#[test]
fn redaction_and_content_hash_contracts_are_explicit() -> TestResult {
    let fixture = build_fixture()?;
    let output = fixture.workspace.join("redaction-summary.handoff.json");
    let handoff_report = create_handoff(&HandoffCreateOptions {
        workspace: fixture.workspace.clone(),
        output,
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
        bind_to_machine: false,
        machine_salt_path: None,
        redaction_level: RedactionLevel::Standard,
    })
    .map_err(|error| format!("create handoff for redaction summary: {error:?}"))?;
    let handoff_json: Value = serde_json::from_str(&handoff_report.to_json())
        .map_err(|error| format!("parse handoff report JSON: {error}"))?;
    for field in [
        "/redaction_summary/secrets_redacted",
        "/redaction_summary/paths_redacted",
        "/redaction_summary/ids_redacted",
    ] {
        ensure(
            handoff_json.pointer(field).and_then(Value::as_u64).is_some(),
            format!("handoff redaction summary missing numeric field `{field}`"),
        )?;
    }
    ensure(
        handoff_json
            .pointer("/redaction_summary/categories")
            .and_then(Value::as_array)
            .is_some(),
        "handoff redaction summary missing categories array",
    )?;

    let backup = create_backup_fixture(&fixture, RedactionLevel::Strict)?;
    let records_path = PathBuf::from(
        backup
            .get("recordsPath")
            .and_then(Value::as_str)
            .ok_or_else(|| "recordsPath missing".to_owned())?,
    );
    let manifest_path = PathBuf::from(
        backup
            .get("manifestPath")
            .and_then(Value::as_str)
            .ok_or_else(|| "manifestPath missing".to_owned())?,
    );
    ensure(
        backup.get("recordsHash").and_then(Value::as_str)
            == Some(file_blake3(&records_path)?.as_str()),
        "backup recordsHash must match records.jsonl bytes",
    )?;
    ensure(
        backup.get("manifestHash").and_then(Value::as_str)
            == Some(file_blake3(&manifest_path)?.as_str()),
        "backup manifestHash must match manifest.json bytes",
    )?;

    let records_text =
        fs::read_to_string(&records_path).map_err(|error| format!("read records: {error}"))?;
    ensure(
        !records_text.contains("sk-ant-api03-conformance"),
        "strict export records must not leak the raw secret-like token",
    )?;
    let records = read_records(&records_path)?;
    let redacted_memory = records
        .iter()
        .find(|record| {
            record.get("schema").and_then(Value::as_str) == Some(EXPORT_MEMORY_SCHEMA_V1)
                && record.get("redacted").and_then(Value::as_bool) == Some(true)
                && record.get("content").and_then(Value::as_str) == Some("[REDACTED]")
        })
        .ok_or_else(|| "missing redacted export memory record".to_owned())?;
    ensure(
        redacted_memory
            .get("content_hash")
            .and_then(Value::as_str)
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "strict redacted memory export must carry original content_hash",
    )?;
    ensure(
        redacted_memory
            .get("redaction_reason")
            .and_then(Value::as_str)
            == Some("redaction_level:strict"),
        "redacted memory export must explain redaction level",
    )?;

    Ok(())
}
