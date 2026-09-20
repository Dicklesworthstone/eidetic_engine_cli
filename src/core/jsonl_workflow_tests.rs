//! Workflow membership must survive the real export/import path without
//! permitting duplicate imports to rewrite an existing workflow assignment.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::models::{ExportScope, WorkspaceId};
use crate::output::jsonl_export::{JsonlExporter, redact_memory_record, redact_recovery_identity};

fn record(n: u128, workflow: Option<&str>) -> ExportMemoryRecord {
    let mut builder = ExportMemoryRecord::builder()
        .memory_id(MemoryId::from_uuid(Uuid::from_u128(n)).to_string())
        .workspace_id(WorkspaceId::from_uuid(Uuid::from_u128(31)).to_string())
        .level("working")
        .kind("observation")
        .content(format!("Workflow recovery observation number {n}."))
        .created_at("2026-09-01T00:00:00+00:00")
        .updated_at("2026-09-01T00:00:00+00:00")
        .trust_class("agent_validated");
    if let Some(workflow) = workflow {
        builder = builder.workflow_id(workflow);
    }
    builder.build().unwrap()
}

fn source(records: &[ExportMemoryRecord], level: RedactionLevel) -> String {
    let mut bytes = Vec::new();
    let mut writer = JsonlExporter::new(&mut bytes, level, ExportScope::All);
    writer
        .write_header(
            ExportHeader::builder()
                .created_at("2026-09-01T00:00:00Z")
                .workspace_id(WorkspaceId::from_uuid(Uuid::from_u128(31)).to_string())
                .workspace_path("/source")
                .export_scope(ExportScope::All)
                .redaction_level(level)
                .ee_version(env!("CARGO_PKG_VERSION"))
                .export_id("workflow-roundtrip")
                .import_source(ImportSource::Native)
                .trust_level(TrustLevel::Validated)
                .build()
                .unwrap(),
        )
        .unwrap();
    for record in records {
        writer.write_memory(record.clone()).unwrap();
    }
    writer
        .write_footer(
            ExportFooter::builder()
                .export_id("workflow-roundtrip")
                .completed_at("2026-09-01T00:01:00Z")
                .build()
                .unwrap(),
        )
        .unwrap();
    String::from_utf8(bytes).unwrap()
}

fn options(root: &Path, text: &str) -> JsonlImportOptions {
    let workspace_path = root.join("workspace");
    fs::create_dir_all(&workspace_path).unwrap();
    let source_path = root.join("source.jsonl");
    fs::write(&source_path, text).unwrap();
    JsonlImportOptions {
        workspace_path,
        database_path: Some(root.join("destination/store.db")),
        source_path,
        dry_run: false,
    }
}

#[test]
fn legacy_absence_and_native_workflow_boundaries_are_compatible() {
    use crate::output::jsonl_export::is_recovery_identity_alias;
    let old = record(1, None);
    let value = serde_json::to_value(&old).unwrap();
    assert!(value.get("workflow_id").is_none());
    let decoded: ExportMemoryRecord = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, old);
    for workflow in ["release", "  release  ", &"é".repeat(64)] {
        let record = record(1, Some(workflow));
        let validated = validate_memory(&record, RedactionLevel::None).unwrap();
        assert_eq!(validated.workflow_id.as_deref(), Some(workflow.trim()));
    }
    let digest = blake3::hash(b"workflow alias boundary")
        .to_hex()
        .to_string();
    let canonical = format!("key_{digest}");
    assert!(is_recovery_identity_alias(&canonical));
    let wire = record(1, Some(&canonical));
    assert_eq!(
        validate_memory(&wire, RedactionLevel::None)
            .unwrap()
            .workflow_id
            .as_deref(),
        Some(canonical.as_str())
    );
    for malformed in [
        format!("key_{}", &digest[..63]),
        format!("{canonical}0"),
        format!("key_{}", digest.to_uppercase()),
        format!("key_{digest}=private"),
        "key_secret".to_owned(),
    ] {
        assert!(!is_recovery_identity_alias(&malformed));
    }
}

#[test]
fn real_import_restores_scoped_recall_without_claiming_unassigned_memories() {
    let root = tempfile::tempdir().unwrap();
    let records = [
        record(1, Some("release-a")),
        record(2, Some("release-a")),
        record(3, Some("release-b")),
        record(4, None),
    ];
    let opts = options(root.path(), &source(&records, RedactionLevel::None));
    let report = import_jsonl_records(&opts).unwrap();
    assert_eq!(report.status, "completed", "{:?}", report.issues);
    assert_eq!(report.memories_imported, 4);
    let db = DbConnection::open_file(database_path(&opts)).unwrap();
    for expected in &records {
        let stored = db.get_memory(&expected.memory_id).unwrap().unwrap();
        assert_eq!(stored.workflow_id, expected.workflow_id);
        assert_eq!(stored.trust_class, "agent_validated");
        assert_eq!(stored.updated_at, expected.updated_at.clone().unwrap());
    }
    let workspace = db
        .get_memory(&records[0].memory_id)
        .unwrap()
        .unwrap()
        .workspace_id;
    let related = db
        .list_recent_workflow_memories(&workspace, "release-a", &records[0].memory_id, 10)
        .unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].id, records[1].memory_id);
    assert!(
        db.list_recent_workflow_memories(&workspace, "release-b", &records[2].memory_id, 10)
            .unwrap()
            .is_empty()
    );
    db.close().unwrap();
}

#[test]
fn invalid_or_secret_workflow_ids_fail_before_destination_creation() {
    let baseline = source(&[record(1, None)], RedactionLevel::None);
    for (bad, code) in [
        (json!(""), "invalid_memory_workflow_id"),
        (json!(" \n\t "), "invalid_memory_workflow_id"),
        (json!("a".repeat(129)), "invalid_memory_workflow_id"),
        (json!("é".repeat(65)), "invalid_memory_workflow_id"),
        (
            json!(format!("sk-proj-{}", "a".repeat(44))),
            "memory_workflow_id_contains_secret",
        ),
        (
            json!("api_key=WORKFLOW_PRIVATE_CANARY"),
            "memory_workflow_id_contains_secret",
        ),
    ] {
        let mut rows = baseline
            .lines()
            .map(|line| serde_json::from_str::<JsonValue>(line).unwrap())
            .collect::<Vec<_>>();
        rows[1]["workflow_id"] = bad.clone();
        let text = rows
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for dry_run in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut opts = options(root.path(), &text);
            opts.dry_run = dry_run;
            let report = import_jsonl_records(&opts).unwrap();
            assert_eq!(report.status, "rejected");
            assert!(
                report.issues.iter().any(|issue| issue.code == code),
                "{:?}",
                report.issues
            );
            let diagnostics = format!("{:?}", report.issues);
            assert!(!diagnostics.contains("WORKFLOW_PRIVATE_CANARY"));
            assert!(!diagnostics.contains("sk-proj-"));
            assert!(!root.path().join("destination").exists());
            assert_eq!(fs::read_to_string(&opts.source_path).unwrap(), text);
        }
    }
}

#[test]
fn reimport_cannot_add_remove_or_reassign_workflows_or_publish_jobs() {
    for original_workflow in [Some("release-a"), None] {
        let root = tempfile::tempdir().unwrap();
        let original = record(1, original_workflow);
        let opts = options(
            root.path(),
            &source(std::slice::from_ref(&original), RedactionLevel::None),
        );
        assert_eq!(import_jsonl_records(&opts).unwrap().memories_imported, 1);
        let db = DbConnection::open_file(database_path(&opts)).unwrap();
        let before = db.get_memory(&original.memory_id).unwrap().unwrap();
        let jobs = db.count_table_rows("search_index_jobs").unwrap();
        db.close().unwrap();
        for workflow in [original_workflow, Some("release-b"), None] {
            let mut changed = original.clone();
            changed.workflow_id = workflow.map(str::to_owned);
            fs::write(&opts.source_path, source(&[changed], RedactionLevel::None)).unwrap();
            let report = import_jsonl_records(&opts).unwrap();
            assert_eq!(report.memories_imported, 0);
            let conflict = report.issues.iter().any(|issue| {
                issue.code == "reimport_divergent_existing_row"
                    && issue.message.contains("workflow_id")
            });
            assert_eq!(
                conflict,
                workflow != original_workflow,
                "{:?}",
                report.issues
            );
            let db = DbConnection::open_file(database_path(&opts)).unwrap();
            assert_eq!(db.get_memory(&original.memory_id).unwrap().unwrap(), before);
            assert_eq!(db.count_table_rows("search_index_jobs").unwrap(), jobs);
            db.close().unwrap();
        }
    }
}

#[test]
fn workflow_redaction_is_non_aliasing_and_stable_across_recovery_generations() {
    let first = "api_key=FIRST_WORKFLOW_PRIVATE_CANARY";
    let second = "api_key=SECOND_WORKFLOW_PRIVATE_CANARY";
    for level in [
        RedactionLevel::None,
        RedactionLevel::Minimal,
        RedactionLevel::Standard,
        RedactionLevel::Strict,
        RedactionLevel::Paranoid,
        RedactionLevel::Full,
    ] {
        let projected = redact_memory_record(record(1, Some(first)), level)
            .workflow_id
            .unwrap();
        let other = redact_memory_record(record(2, Some(second)), level)
            .workflow_id
            .unwrap();
        assert_ne!(projected, other);
        assert_eq!(redact_recovery_identity(&projected, level), projected);
        assert_eq!(
            redact_memory_record(record(1, Some(&projected)), level)
                .workflow_id
                .as_deref(),
            Some(projected.as_str())
        );
        if level == RedactionLevel::None {
            assert_eq!(projected, first);
        } else {
            assert!(!projected.contains("PRIVATE_CANARY"));
            assert!(projected.starts_with("key_"));
            assert_eq!(projected.len(), 68);
        }
    }
    assert_ne!(
        redact_recovery_identity("key_secret", RedactionLevel::Full),
        "key_secret"
    );
}

#[test]
fn redacted_workflows_import_and_remain_usable_for_scoped_recall() {
    let original = "api_key=WORKFLOW_PRIVATE_CANARY";
    for level in [
        RedactionLevel::Minimal,
        RedactionLevel::Standard,
        RedactionLevel::Strict,
        RedactionLevel::Paranoid,
        RedactionLevel::Full,
    ] {
        let text = source(&[record(1, Some(original))], level);
        assert!(!text.contains("WORKFLOW_PRIVATE_CANARY"));
        let root = tempfile::tempdir().unwrap();
        let opts = options(root.path(), &text);
        let report = import_jsonl_records(&opts).unwrap();
        assert_eq!(report.status, "completed", "{level:?}: {:?}", report.issues);
        assert_eq!(report.memories_imported, 1);
        let expected = redact_recovery_identity(original, level);
        let db = DbConnection::open_file(database_path(&opts)).unwrap();
        let parsed = parse_jsonl_source(&text);
        let restored_id = validate_memories(&parsed).unwrap()[0].id.clone();
        let stored = db.get_memory(&restored_id).unwrap().unwrap();
        assert_eq!(stored.workflow_id.as_deref(), Some(expected.as_str()));
        let related = db
            .list_recent_workflow_memories(&stored.workspace_id, &expected, "not-this-memory", 10)
            .unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].id, restored_id);
        db.close().unwrap();
    }
}
