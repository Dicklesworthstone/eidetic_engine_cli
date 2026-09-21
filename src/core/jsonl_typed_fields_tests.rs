//! Real-import admission, duplicate handling and typed sidecar persistence.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::models::{ExportFooter, ExportMemoryRecord, ExportScope, WorkspaceId};
use crate::output::jsonl_export::JsonlExporter;

fn record(n: u128, kind: &str, fields: Option<JsonValue>) -> ExportMemoryRecord {
    let mut record = ExportMemoryRecord::builder()
        .memory_id(MemoryId::from_uuid(Uuid::from_u128(n)).to_string())
        .workspace_id(WorkspaceId::from_uuid(Uuid::from_u128(31)).to_string())
        .level("semantic")
        .kind(kind)
        .content(format!("Typed recovery memory number {n}."))
        .created_at("2026-09-01T00:00:00+00:00")
        .updated_at("2026-09-01T00:00:00+00:00")
        .trust_class("agent_validated")
        .build()
        .unwrap();
    record.typed_fields = fields;
    record
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
                .export_id("typed-field-test")
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
                .export_id("typed-field-test")
                .completed_at("2026-09-01T00:01:00Z")
                .build()
                .unwrap(),
        )
        .unwrap();
    String::from_utf8(bytes).unwrap()
}

fn options(root: &Path, text: &str) -> JsonlImportOptions {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let source_path = root.join("source.jsonl");
    fs::write(&source_path, text).unwrap();
    JsonlImportOptions {
        workspace_path: workspace,
        database_path: Some(root.join("destination/store.db")),
        source_path,
        dry_run: false,
    }
}

#[test]
fn import_preserves_typed_fields_before_tombstones_and_keeps_legacy_absence() {
    let root = tempfile::tempdir().unwrap();
    let mut tombstone = record(
        1,
        "rule",
        Some(json!({"condition":"release","action":"verify","exceptions":["docs"]})),
    );
    tombstone.tombstoned_at = Some("2026-09-02T00:00:00+00:00".to_owned());
    let plain = record(2, "rule", None);
    let options = options(
        root.path(),
        &source(&[tombstone.clone(), plain.clone()], RedactionLevel::None),
    );
    let report = import_jsonl_records(&options).unwrap();
    assert_eq!(report.status, "completed", "{:?}", report.issues);
    assert_eq!(report.memories_imported, 2);
    let db = DbConnection::open_file(database_path(&options)).unwrap();
    let typed = db
        .get_memory_typed_fields_json(&tombstone.memory_id)
        .unwrap()
        .unwrap();
    let expected = crate::models::memory::canonicalize_typed_memory_fields_json(
        &MemoryKind::Rule,
        &tombstone.typed_fields.unwrap().to_string(),
    )
    .unwrap();
    assert_eq!(typed, expected);
    let stored = db.get_memory(&tombstone.memory_id).unwrap().unwrap();
    assert_eq!(stored.tombstoned_at, tombstone.tombstoned_at);
    assert_eq!(stored.updated_at, tombstone.updated_at.unwrap());
    assert!(
        db.get_memory_typed_fields_json(&plain.memory_id)
            .unwrap()
            .is_none()
    );
    db.close().unwrap();
}

#[test]
fn import_rejects_bad_or_secret_sidecars_before_destination_creation() {
    let baseline = source(&[record(1, "decision", None)], RedactionLevel::None);
    for bad in [
        json!({"chosen":42}),
        json!({"unknown":"TYPED_PRIVATE_CANARY"}),
        json!({"schema":"ee.memory.typed_fields.v2","kind":"rule","fields":{"action":"wrong"}}),
        json!({"chosen":"api_key=TYPED_PRIVATE_CANARY"}),
        json!({"revisit_by":"not-a-date"}),
        json!({"options":vec!["x";9]}),
        json!({"chosen":"x".repeat(4097)}),
        json!("not-an-object"),
    ] {
        let mut rows = baseline
            .lines()
            .map(|line| serde_json::from_str::<JsonValue>(line).unwrap())
            .collect::<Vec<_>>();
        rows[1]["typed_fields"] = bad;
        let text = rows
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for dry in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut opts = options(root.path(), &text);
            opts.dry_run = dry;
            let report = import_jsonl_records(&opts).unwrap();
            assert_eq!(report.status, "rejected");
            assert!(
                report
                    .issues
                    .iter()
                    .any(|issue| issue.code == "invalid_memory_typed_fields"
                        || issue.code == "memory_typed_fields_contain_secret")
            );
            assert!(!format!("{:?}", report.issues).contains("TYPED_PRIVATE_CANARY"));
            assert!(!root.path().join("destination").exists());
            assert_eq!(fs::read_to_string(opts.source_path).unwrap(), text);
        }
    }
}

#[test]
fn reimport_reports_typed_only_conflicts_without_overwriting_or_publishing() {
    let root = tempfile::tempdir().unwrap();
    let original = record(1, "rule", Some(json!({"action":"first"})));
    let opts = options(
        root.path(),
        &source(std::slice::from_ref(&original), RedactionLevel::None),
    );
    assert_eq!(import_jsonl_records(&opts).unwrap().memories_imported, 1);
    let db = DbConnection::open_file(database_path(&opts)).unwrap();
    let before = db
        .get_memory_typed_fields_json(&original.memory_id)
        .unwrap();
    let jobs = db.count_table_rows("search_index_jobs").unwrap();
    db.close().unwrap();
    // Same body, same timestamps, different typed sidecar (and then absent).
    for fields in [Some(json!({"action":"second"})), None] {
        let mut changed = original.clone();
        changed.typed_fields = fields;
        fs::write(&opts.source_path, source(&[changed], RedactionLevel::None)).unwrap();
        let report = import_jsonl_records(&opts).unwrap();
        assert_eq!(report.memories_imported, 0);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "reimport_divergent_existing_row"
                    && issue.message.contains("typed_fields"))
        );
        let db = DbConnection::open_file(database_path(&opts)).unwrap();
        assert_eq!(
            db.get_memory_typed_fields_json(&original.memory_id)
                .unwrap(),
            before
        );
        assert_eq!(db.count_table_rows("search_index_jobs").unwrap(), jobs);
        db.close().unwrap();
    }
}

#[test]
fn legacy_v1_typed_fields_and_identical_reimports_preserve_canonical_values() {
    let root = tempfile::tempdir().unwrap();
    let baseline = source(
        &[record(
            1,
            "failure",
            Some(json!({"cause":"compile","family":"parser"})),
        )],
        RedactionLevel::None,
    );
    // Bypass the writer's upgrade to exercise actual v1 wire admission.
    let text = baseline.replace("ee.memory.typed_fields.v2", "ee.memory.typed_fields.v1");
    let opts = options(root.path(), &text);
    assert_eq!(import_jsonl_records(&opts).unwrap().memories_imported, 1);
    let report = import_jsonl_records(&opts).unwrap();
    assert_eq!(report.memories_imported, 0);
    assert!(
        !report
            .issues
            .iter()
            .any(|issue| issue.code == "reimport_divergent_existing_row")
    );
    let db = DbConnection::open_file(database_path(&opts)).unwrap();
    let raw = db
        .get_memory_typed_fields_json(&record(1, "failure", None).memory_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<JsonValue>(&raw).unwrap()["fields"]["family"],
        "parser"
    );
    assert!(raw.contains("ee.memory.typed_fields.v2"));
    db.close().unwrap();
}

#[test]
fn redacted_decision_supersedes_points_to_the_same_restored_memory_identity() {
    let prior = record(1, "decision", Some(json!({"chosen":"prior"})));
    let head = record(
        2,
        "decision",
        Some(json!({"chosen":"current","supersedes":prior.memory_id})),
    );
    let text = source(&[prior, head], RedactionLevel::Standard);
    let parsed = parse_jsonl_source(&text);
    let expected = validate_memories(&parsed).unwrap();
    let prior = expected[0].id.clone();
    let head = expected[1].id.clone();
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path(), &text);
    assert_eq!(import_jsonl_records(&opts).unwrap().memories_imported, 2);
    let db = DbConnection::open_file(database_path(&opts)).unwrap();
    let raw = db.get_memory_typed_fields_json(&head).unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<JsonValue>(&raw).unwrap()["fields"]["supersedes"],
        prior
    );
    assert!(db.get_memory(&prior).unwrap().is_some());
    db.close().unwrap();
}
