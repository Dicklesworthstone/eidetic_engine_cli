//! Real-store regressions for recovery admission and revision headship.
//!
//! These tests use the production JSONL importer, migrations, typed storage
//! methods and the pre-publication verifier. No mock database or index is used.

use super::*;
use crate::db::CreateWorkspaceInput;
use crate::models::WorkspaceId;

type TestResult = Result<(), String>;

fn rows() -> Vec<JsonValue> {
    let workspace = WorkspaceId::from_uuid(Uuid::from_u128(31)).to_string();
    let first = MemoryId::from_uuid(Uuid::from_u128(1)).to_string();
    let second = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    let memory = |id: &str, content: &str| {
        json!({
            "schema": EXPORT_MEMORY_SCHEMA_V1,
            "memory_id": id, "workspace_id": workspace,
            "level": "procedural", "kind": "rule", "content": content,
            "confidence": 0.9, "utility": 0.7, "importance": 0.8,
            "trust_class": "human_explicit", "trust_subclass": "reviewed",
            "created_at": "2026-04-30T00:00:00Z", "updated_at": null,
            "provenance_uri": "ee-export://recovery-regression", "redacted": false
        })
    };
    vec![
        json!({
            "schema": EXPORT_HEADER_SCHEMA_V1, "format_version": 1,
            "created_at": "2026-04-30T00:00:00Z", "workspace_id": workspace,
            "workspace_path": "/source", "export_scope": "all",
            "redaction_level": "none", "record_count": 6,
            "ee_version": "0.15.2", "export_id": "recovery-regression",
            "import_source": "native", "trust_level": "validated"
        }),
        memory(&first, "Retain the original release evidence."),
        json!({"schema": EXPORT_TAG_SCHEMA_V1, "memory_id": first,
            "tag": "evidence", "created_at": "2026-04-30T00:00:00Z"}),
        memory(
            &second,
            "Use the current, explicitly reviewed release procedure.",
        ),
        json!({"schema": EXPORT_TAG_SCHEMA_V1, "memory_id": second,
            "tag": "release", "created_at": "2026-04-30T00:00:00Z"}),
        json!({
            "schema": EXPORT_LINK_SCHEMA_V1,
            "link_id": MemoryLinkId::from_uuid(Uuid::from_u128(3)).to_string(),
            "source_memory_id": first, "target_memory_id": second,
            "link_type": "supports", "weight": 0.75,
            "created_at": "2026-04-30T00:00:01Z",
            "metadata": {"source": "agent", "confidence": 0.5,
                "directed": false, "evidenceCount": 7}
        }),
        json!({
            "schema": EXPORT_FOOTER_SCHEMA_V1, "export_id": "recovery-regression",
            "completed_at": "2026-04-30T00:01:00Z", "total_records": 7,
            "memory_count": 2, "link_count": 1, "tag_count": 2,
            "audit_count": 0, "success": true
        }),
    ]
}

fn source_text(records: &[JsonValue]) -> String {
    records
        .iter()
        .map(JsonValue::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

struct Fixture {
    _root: tempfile::TempDir,
    options: JsonlImportOptions,
    workspace: String,
    db: DbConnection,
}

impl Fixture {
    fn new(records: &[JsonValue]) -> Result<Self, String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace_path = path.join("workspace");
        fs::create_dir_all(workspace_path.join(".ee")).map_err(|error| error.to_string())?;
        let options = JsonlImportOptions {
            workspace_path,
            database_path: None,
            source_path: path.join("records.jsonl"),
            dry_run: false,
        };
        let workspace = records[0]["workspace_id"]
            .as_str()
            .ok_or("fixture workspace identity is absent")?
            .to_owned();
        let db =
            DbConnection::open_file(database_path(&options)).map_err(|error| error.to_string())?;
        db.migrate().map_err(|error| error.to_string())?;
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: options.workspace_path.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .map_err(|error| error.to_string())?;
        db.close().map_err(|error| error.to_string())?;
        fs::write(&options.source_path, source_text(records)).map_err(|error| error.to_string())?;
        let report =
            import_verified_backup_jsonl_records(&options).map_err(|error| error.to_string())?;
        assert_eq!(report.status, "completed", "{:?}", report.issues);
        assert_eq!(
            report.memories_imported as usize,
            records
                .iter()
                .filter(|row| row["schema"] == EXPORT_MEMORY_SCHEMA_V1)
                .count()
        );
        assert_eq!(report.links_imported, 1);
        let db =
            DbConnection::open_file(database_path(&options)).map_err(|error| error.to_string())?;
        Ok(Self {
            _root: root,
            options,
            workspace,
            db,
        })
    }

    fn verify(&self) -> Result<(), crate::models::DomainError> {
        recovery::verify_backup_records(
            &database_path(&self.options),
            &self.options.source_path,
            &self.options.workspace_path,
            &self.workspace,
        )
    }
}

#[test]
fn explicit_head_expiry_cannot_become_supersession_under_clock_skew() -> TestResult {
    for reverse_reference in [false, true] {
        let mut records = rows();
        let prior = records[1]["memory_id"].as_str().ok_or("prior")?.to_owned();
        let head = records[3]["memory_id"].as_str().ok_or("head")?.to_owned();
        records[1]["created_at"] = json!("2026-05-03T00:00:00Z");
        records[1]["valid_to"] = json!("2028-12-01T00:00:00Z");
        records[3]["created_at"] = json!("2026-05-02T00:00:00Z");
        records[3]["valid_from"] = json!("2026-05-04T00:00:00Z");
        records[3]["valid_to"] = json!("2029-12-01T00:00:00Z");
        records[3]["logical_id"] = json!(prior);
        if reverse_reference {
            records[3]["supersedes"] = json!(prior);
        } else {
            records[1]["superseded_by"] = json!(head);
        }
        let fixture = Fixture::new(&records)?;
        assert_eq!(
            fixture
                .db
                .get_memory_superseded_at(&prior)
                .map_err(|e| e.to_string())?,
            Some("2026-05-04T00:00:00Z".to_owned())
        );
        assert_eq!(
            fixture
                .db
                .get_memory_superseded_at(&head)
                .map_err(|e| e.to_string())?,
            None,
            "an explicit head's expiry is not a supersession marker"
        );
        for (id, expiry) in [
            (&prior, "2028-12-01T00:00:00Z"),
            (&head, "2029-12-01T00:00:00Z"),
        ] {
            let memory = fixture
                .db
                .get_memory(id)
                .map_err(|e| e.to_string())?
                .ok_or("restored memory is absent")?;
            assert_eq!(memory.valid_to.as_deref(), Some(expiry));
            assert_eq!(memory.trust_class, "agent_validated");
        }
        assert_eq!(
            fixture
                .db
                .filter_current_memory_ids(&[prior.clone(), head.clone()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([head.clone()])
        );
        fixture.verify().map_err(|e| e.to_string())?;
        let repeated =
            import_verified_backup_jsonl_records(&fixture.options).map_err(|e| e.to_string())?;
        assert_eq!(repeated.status, "completed", "{:?}", repeated.issues);
        assert_eq!(repeated.memories_imported, 0);
        assert_eq!(repeated.memories_skipped_duplicate, 2);
        fixture.verify().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[test]
fn recovery_rejects_an_unexpected_supersession_even_with_identical_rows() -> TestResult {
    let records = rows();
    let fixture = Fixture::new(&records)?;
    fixture.verify().map_err(|e| e.to_string())?;
    let head = records[1]["memory_id"].as_str().ok_or("head")?;
    assert!(
        fixture
            .db
            .restore_imported_memory_supersession(head, "2026-05-04T00:00:00Z")
            .map_err(|e| e.to_string())?
    );
    assert_eq!(
        fixture
            .db
            .count_table_rows("memories")
            .map_err(|e| e.to_string())?,
        2
    );
    let error = fixture
        .verify()
        .expect_err("retiring a live head must block publication");
    assert!(error.message().contains("revision supersession differs"));
    assert!(!error.to_string().contains(head));
    Ok(())
}

#[test]
fn legacy_expiry_only_history_still_recovers_its_current_head() -> TestResult {
    for expiry_field in ["valid_to", "expires_at"] {
        let mut records = rows();
        let prior = records[1]["memory_id"].as_str().ok_or("prior")?.to_owned();
        let head = records[3]["memory_id"].as_str().ok_or("head")?.to_owned();
        records[1][expiry_field] = json!("2026-05-04T00:00:00Z");
        records[3]["logical_id"] = json!(prior);
        records[3]["created_at"] = json!("2026-05-04T00:00:00Z");
        records[3]["valid_from"] = json!("2026-05-04T00:00:00Z");
        let fixture = Fixture::new(&records)?;
        assert_eq!(
            fixture
                .db
                .get_memory_superseded_at(&prior)
                .map_err(|e| e.to_string())?,
            Some("2026-05-04T00:00:00Z".to_owned())
        );
        assert_eq!(
            fixture
                .db
                .filter_current_memory_ids(&[prior, head.clone()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([head])
        );
        fixture.verify().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[test]
fn mixed_era_history_keeps_legacy_ancestors_without_retiring_explicit_heads() -> TestResult {
    let mut records = rows();
    let prior = records[1]["memory_id"].as_str().ok_or("prior")?.to_owned();
    let middle = records[3]["memory_id"].as_str().ok_or("middle")?.to_owned();
    let head = MemoryId::from_uuid(Uuid::from_u128(4)).to_string();
    let mut newest = records[3].clone();
    newest["memory_id"] = json!(head);
    newest["logical_id"] = json!(prior);
    newest["supersedes"] = json!(middle);
    newest["created_at"] = json!("2026-05-03T00:00:00Z");
    newest["valid_from"] = json!("2026-05-04T00:00:00Z");
    newest["valid_to"] = json!("2029-12-01T00:00:00Z");
    records[1]["valid_to"] = json!("2026-05-02T00:00:00Z");
    records[3]["logical_id"] = json!(prior);
    records[3]["superseded_by"] = json!(head);
    records[3]["created_at"] = json!("2026-05-02T00:00:00Z");
    records[3]["valid_from"] = json!("2026-05-02T00:00:00Z");
    records[6]["total_records"] = json!(8);
    records[6]["memory_count"] = json!(3);
    records[0]["record_count"] = json!(7);
    records.insert(6, newest);
    let parsed = parse_jsonl_source(&source_text(&records));
    let validated = validate_memories(&parsed).map_err(|issues| format!("{issues:?}"))?;
    assert_eq!(
        revisions::legacy_supersession_ids(&validated),
        BTreeSet::from([prior.clone()])
    );
    let fixture = Fixture::new(&records)?;
    for (id, marker) in [
        (&prior, Some("2026-05-02T00:00:00Z")),
        (&middle, Some("2026-05-04T00:00:00Z")),
        (&head, None),
    ] {
        assert_eq!(
            fixture
                .db
                .get_memory_superseded_at(id)
                .map_err(|e| e.to_string())?
                .as_deref(),
            marker
        );
    }
    assert_eq!(
        fixture
            .db
            .filter_current_memory_ids(&[prior, middle, head.clone()])
            .map_err(|e| e.to_string())?,
        BTreeSet::from([head])
    );
    fixture.verify().map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn legacy_selector_returns_imported_ids_not_redacted_archive_aliases() -> TestResult {
    let mut records = rows();
    records[0]["redaction_level"] = json!("standard");
    records[1]["memory_id"] = json!("redacted-prior");
    records[1]["valid_to"] = json!("2026-05-04T00:00:00Z");
    records[2]["memory_id"] = json!("redacted-prior");
    records[3]["memory_id"] = json!("redacted-head");
    records[3]["logical_id"] = json!("redacted-prior");
    records[3]["created_at"] = json!("2026-05-04T00:00:00Z");
    records[4]["memory_id"] = json!("redacted-head");
    records[5]["source_memory_id"] = json!("redacted-prior");
    records[5]["target_memory_id"] = json!("redacted-head");
    let parsed = parse_jsonl_source(&source_text(&records));
    let validated = validate_memories(&parsed).map_err(|issues| format!("{issues:?}"))?;
    let eligible = revisions::legacy_supersession_ids(&validated);
    assert_eq!(eligible, BTreeSet::from([validated[0].id.clone()]));
    assert!(eligible.iter().all(|id| id.parse::<MemoryId>().is_ok()));
    assert!(!eligible.contains("redacted-prior"));
    // Adding an explicit edge protects both imported identities even though
    // references in the authenticated projection still use archive aliases.
    records[1]["superseded_by"] = json!("redacted-head");
    records[3]["valid_to"] = json!("2029-12-01T00:00:00Z");
    let parsed = parse_jsonl_source(&source_text(&records));
    let validated = validate_memories(&parsed).map_err(|issues| format!("{issues:?}"))?;
    assert!(revisions::legacy_supersession_ids(&validated).is_empty());
    Ok(())
}
