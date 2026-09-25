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
fn verified_restore_preserves_missing_provenance_even_with_a_source_agent() -> TestResult {
    let mut records = rows();
    records[1]["provenance_uri"] = JsonValue::Null;
    records[1]["source_agent"] = json!("RecoveryAgent");
    let fixture = Fixture::new(&records)?;
    for record in [&records[1], &records[3]] {
        let id = record["memory_id"].as_str().ok_or("memory identity")?;
        let memory = fixture
            .db
            .get_memory(id)
            .map_err(|error| error.to_string())?
            .ok_or("restored memory")?;
        assert_eq!(
            memory.provenance_uri.as_deref(),
            record["provenance_uri"].as_str()
        );
    }
    fixture.verify().map_err(|error| error.to_string())?;
    // A verifier sharing import preparation must still reject a writer that
    // manufactures evidence after admission, including for absent provenance.
    fixture
        .db
        .execute_raw("UPDATE memories SET provenance_uri = 'jsonl-import://unknown' WHERE provenance_uri IS NULL")
        .map_err(|error| error.to_string())?;
    assert!(fixture.verify().is_err());
    Ok(())
}

#[test]
fn ordinary_jsonl_import_retains_origin_markers_for_missing_provenance() -> TestResult {
    for source_agent in [None, Some("ExternalAgent")] {
        let mut records = rows();
        records[1]["provenance_uri"] = JsonValue::Null;
        records[1]["source_agent"] = json!(source_agent);
        records[1]["trust_class"] = json!("agent_validated");
        records[3]["trust_class"] = json!("agent_validated");
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let options = JsonlImportOptions {
            workspace_path: workspace,
            database_path: None,
            source_path: root.path().join("records.jsonl"),
            dry_run: false,
        };
        fs::write(&options.source_path, source_text(&records))
            .map_err(|error| error.to_string())?;
        let report = import_jsonl_records(&options).map_err(|error| error.to_string())?;
        assert_eq!(report.status, "completed", "{:?}", report.issues);
        assert_eq!(report.memories_imported, 2);
        let db = DbConnection::open_file(database_path(&options))
            .map_err(|error| error.to_string())?;
        for (record, expected) in [
            (
                &records[1],
                format!("jsonl-import://{}", source_agent.unwrap_or("unknown")),
            ),
            (&records[3], "ee-export://recovery-regression".to_owned()),
        ] {
            let id = record["memory_id"].as_str().ok_or("memory identity")?;
            let memory = db
                .get_memory(id)
                .map_err(|error| error.to_string())?
                .ok_or("imported memory")?;
            assert_eq!(memory.provenance_uri.as_deref(), Some(expected.as_str()));
        }
        db.close().map_err(|error| error.to_string())?;
    }
    Ok(())
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
    let audits = fixture
        .db
        .list_audit_entries(Some(&fixture.workspace), None)
        .map_err(|e| e.to_string())?;
    let repeated =
        import_verified_backup_jsonl_records(&fixture.options).map_err(|e| e.to_string())?;
    assert_eq!(repeated.status, "rejected", "{:?}", repeated.issues);
    assert!(
        repeated
            .issues
            .iter()
            .any(|issue| issue.code == "reimport_divergent_revision_chain")
    );
    assert_eq!(
        (repeated.memories_imported, repeated.links_imported),
        (0, 0)
    );
    assert_eq!(
        fixture
            .db
            .list_audit_entries(Some(&fixture.workspace), None)
            .map_err(|e| e.to_string())?,
        audits
    );
    assert_eq!(
        fixture
            .db
            .get_memory_superseded_at(head)
            .map_err(|e| e.to_string())?
            .as_deref(),
        Some("2026-05-04T00:00:00Z")
    );
    Ok(())
}

#[test]
fn reimport_rejects_corrupted_edge_declared_head_before_any_write() -> TestResult {
    for reverse_reference in [false, true] {
        let mut records = rows();
        let prior = records[1]["memory_id"].clone();
        let head = records[3]["memory_id"].as_str().ok_or("head")?.to_owned();
        records[1]["valid_to"] = json!("2028-12-01T00:00:00Z");
        records[3]["valid_to"] = json!("2029-12-01T00:00:00Z");
        records[3]["logical_id"] = prior.clone();
        if reverse_reference {
            records[3]["supersedes"] = prior;
        } else {
            records[1]["superseded_by"] = json!(head);
        }
        let fixture = Fixture::new(&records)?;
        fixture.verify().map_err(|e| e.to_string())?;
        let before = fixture
            .db
            .list_memories(&fixture.workspace, None, true)
            .map_err(|e| e.to_string())?;
        let audits = fixture
            .db
            .list_audit_entries(Some(&fixture.workspace), None)
            .map_err(|e| e.to_string())?;
        assert!(
            fixture
                .db
                .restore_imported_memory_supersession(&head, "2026-05-04T00:00:00Z")
                .map_err(|e| e.to_string())?
        );
        let repeated =
            import_verified_backup_jsonl_records(&fixture.options).map_err(|e| e.to_string())?;
        assert_eq!(repeated.status, "rejected", "{:?}", repeated.issues);
        assert!(
            repeated
                .issues
                .iter()
                .any(|issue| issue.code == "reimport_divergent_revision_chain")
        );
        assert_eq!(
            (repeated.memories_imported, repeated.links_imported),
            (0, 0)
        );
        assert_eq!(
            fixture
                .db
                .get_memory_superseded_at(&head)
                .map_err(|e| e.to_string())?
                .as_deref(),
            Some("2026-05-04T00:00:00Z")
        );
        assert_eq!(
            fixture
                .db
                .list_memories(&fixture.workspace, None, true)
                .map_err(|e| e.to_string())?,
            before
        );
        assert_eq!(
            fixture
                .db
                .list_audit_entries(Some(&fixture.workspace), None)
                .map_err(|e| e.to_string())?,
            audits
        );
        assert!(fixture.verify().is_err());
    }
    Ok(())
}

#[test]
fn expiring_edge_head_cannot_hide_a_disconnected_current_revision() -> TestResult {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    for reverse_reference in [false, true] {
        for explicit_null in [false, true] {
            let mut records = rows();
            let prior = records[1]["memory_id"].clone();
            records[1]["valid_to"] = json!("2028-12-01T00:00:00Z");
            records[3]["logical_id"] = prior.clone();
            records[3]["expires_at"] = json!("2029-12-01T00:00:00Z");
            let mut disconnected = records[3].clone();
            disconnected["memory_id"] = json!(MemoryId::from_uuid(Uuid::from_u128(44)).to_string());
            if explicit_null {
                disconnected["superseded_at"] = JsonValue::Null;
            } else {
                disconnected
                    .as_object_mut()
                    .ok_or("memory record")?
                    .remove("expires_at");
            }
            if reverse_reference {
                records[3]["supersedes"] = prior;
            } else {
                records[1]["superseded_by"] = records[3]["memory_id"].clone();
            }
            records.insert(5, disconnected);
            records[0]["record_count"] = json!(7);
            records[7]["total_records"] = json!(8);
            records[7]["memory_count"] = json!(3);
            let source = source_text(&records);
            assert!(!parse_jsonl_source(&source).has_errors());
            for dry_run in [false, true] {
                for verified_backup in [false, true] {
                    let case =
                        format!("{reverse_reference}-{explicit_null}-{dry_run}-{verified_backup}");
                    let options = JsonlImportOptions {
                        workspace_path: root.path().join(format!("workspace-{case}")),
                        database_path: Some(root.path().join(format!("database-{case}/ee.db"))),
                        source_path: root.path().join(format!("source-{case}.jsonl")),
                        dry_run,
                    };
                    fs::write(&options.source_path, &source).map_err(|e| e.to_string())?;
                    let report = if verified_backup {
                        import_verified_backup_jsonl_records(&options)
                    } else {
                        import_jsonl_records(&options)
                    }
                    .map_err(|e| e.to_string())?;
                    assert_eq!(report.status, "rejected", "{case}: {:?}", report.issues);
                    assert!(
                        report
                            .issues
                            .iter()
                            .any(|issue| issue.code == "invalid_memory_lineage"),
                        "{case}: {:?}",
                        report.issues
                    );
                    assert_eq!((report.memories_imported, report.links_imported), (0, 0));
                    assert!(!options.workspace_path.exists(), "{case}");
                    assert!(
                        !database_path(&options)
                            .parent()
                            .ok_or("database parent")?
                            .exists(),
                        "{case}"
                    );
                    assert_eq!(
                        fs::read_to_string(&options.source_path).map_err(|e| e.to_string())?,
                        source
                    );
                }
            }
        }
    }
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

fn expiring_null_head_with_tombstoned_sibling(expiry_field: &str) -> Vec<JsonValue> {
    let mut records = rows();
    records[1]["superseded_at"] = JsonValue::Null;
    records[1][expiry_field] = json!("2099-01-01T00:00:00Z");
    records[3]["logical_id"] = records[1]["memory_id"].clone();
    records[3]["superseded_at"] = JsonValue::Null;
    records[3]["created_at"] = json!("2026-05-02T00:00:00Z");
    records[3]["tombstoned_at"] = json!("2026-05-03T00:00:00Z");
    records
}

#[test]
fn explicit_null_keeps_expiring_head_while_omission_retains_legacy_inference() -> TestResult {
    for expiry_field in ["valid_to", "expires_at"] {
        for explicit_null in [false, true] {
            let mut records = expiring_null_head_with_tombstoned_sibling(expiry_field);
            if !explicit_null {
                for index in [1, 3] {
                    records[index]
                        .as_object_mut()
                        .ok_or("memory record")?
                        .remove("superseded_at");
                }
            }
            let head = records[1]["memory_id"].as_str().ok_or("head")?.to_owned();
            let sibling = records[3]["memory_id"]
                .as_str()
                .ok_or("sibling")?
                .to_owned();
            let fixture = Fixture::new(&records)?;
            assert_eq!(
                fixture
                    .db
                    .get_memory_superseded_at(&head)
                    .map_err(|e| e.to_string())?
                    .as_deref(),
                if explicit_null {
                    None
                } else {
                    Some("2099-01-01T00:00:00Z")
                },
                "{expiry_field}, explicit_null={explicit_null}"
            );
            assert!(
                fixture
                    .db
                    .get_memory_superseded_at(&sibling)
                    .map_err(|e| e.to_string())?
                    .is_none()
            );
            assert_eq!(
                fixture
                    .db
                    .get_memory(&head)
                    .map_err(|e| e.to_string())?
                    .ok_or("restored head")?
                    .valid_to
                    .as_deref(),
                Some("2099-01-01T00:00:00Z")
            );
            assert_eq!(
                fixture
                    .db
                    .filter_current_memory_ids(&[head.clone(), sibling])
                    .map_err(|e| e.to_string())?,
                if explicit_null {
                    BTreeSet::from([head])
                } else {
                    BTreeSet::new()
                },
                "expiry must not retire a modern head because a later tombstoned sibling exists"
            );
            fixture.verify().map_err(|e| e.to_string())?;
            let repeated = import_verified_backup_jsonl_records(&fixture.options)
                .map_err(|e| e.to_string())?;
            assert_eq!(repeated.status, "completed", "{:?}", repeated.issues);
            assert_eq!(repeated.memories_imported, 0);
            assert_eq!(repeated.memories_skipped_duplicate, 2);
            fixture.verify().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[test]
fn explicit_null_expiry_rejects_corrupt_headship_in_verification_and_reimport() -> TestResult {
    let records = expiring_null_head_with_tombstoned_sibling("valid_to");
    let fixture = Fixture::new(&records)?;
    fixture.verify().map_err(|e| e.to_string())?;
    let head = records[1]["memory_id"].as_str().ok_or("head")?;
    let before = fixture
        .db
        .list_memories(&fixture.workspace, None, true)
        .map_err(|e| e.to_string())?;
    let audits = fixture
        .db
        .list_audit_entries(Some(&fixture.workspace), None)
        .map_err(|e| e.to_string())?;
    assert!(
        fixture
            .db
            .restore_imported_memory_supersession(head, "2026-05-04T00:00:00Z")
            .map_err(|e| e.to_string())?
    );
    assert_eq!(
        fixture
            .db
            .list_memories(&fixture.workspace, None, true)
            .map_err(|e| e.to_string())?,
        before
    );
    let error = fixture.verify().expect_err(
        "a modern null marker is an exact recovery obligation even when the row expires",
    );
    assert!(error.message().contains("revision supersession differs"));
    assert!(!error.message().contains(head));
    let repeated =
        import_verified_backup_jsonl_records(&fixture.options).map_err(|e| e.to_string())?;
    assert_eq!(repeated.status, "rejected", "{:?}", repeated.issues);
    assert_eq!(
        (repeated.memories_imported, repeated.links_imported),
        (0, 0)
    );
    assert!(
        repeated
            .issues
            .iter()
            .any(|issue| issue.code == "reimport_divergent_revision_chain")
    );
    assert_eq!(
        fixture
            .db
            .get_memory_superseded_at(head)
            .map_err(|e| e.to_string())?
            .as_deref(),
        Some("2026-05-04T00:00:00Z")
    );
    assert_eq!(
        fixture
            .db
            .list_audit_entries(Some(&fixture.workspace), None)
            .map_err(|e| e.to_string())?,
        audits
    );
    assert_eq!(
        fixture
            .db
            .list_memories(&fixture.workspace, None, true)
            .map_err(|e| e.to_string())?,
        before
    );
    Ok(())
}

#[test]
fn explicit_null_conflicts_are_rejected_before_destination_creation() -> TestResult {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    for dry_run in [false, true] {
        for case in ["multiple_heads", "successor", "predecessor"] {
            let mut records = rows();
            records[1]["superseded_at"] = JsonValue::Null;
            records[1]["valid_to"] = json!("2026-04-30T00:00:00Z");
            records[3]["logical_id"] = records[1]["memory_id"].clone();
            records[3]["superseded_at"] = JsonValue::Null;
            records[3]["expires_at"] = json!("2026-04-30T00:00:00Z");
            match case {
                "successor" => records[1]["superseded_by"] = records[3]["memory_id"].clone(),
                "predecessor" => records[3]["supersedes"] = records[1]["memory_id"].clone(),
                _ => {}
            }
            let options = JsonlImportOptions {
                workspace_path: root.path().join(format!("workspace-{dry_run}-{case}")),
                database_path: Some(root.path().join(format!("database-{dry_run}-{case}/ee.db"))),
                source_path: root.path().join(format!("source-{dry_run}-{case}.jsonl")),
                dry_run,
            };
            let source = source_text(&records);
            fs::write(&options.source_path, &source).map_err(|e| e.to_string())?;
            let report =
                import_verified_backup_jsonl_records(&options).map_err(|e| e.to_string())?;
            assert_eq!(
                report.status, "rejected",
                "{case}, dry_run={dry_run}: {:?}",
                report.issues
            );
            let expected_code = if case == "multiple_heads" {
                "invalid_memory_lineage"
            } else {
                "invalid_memory_supersession"
            };
            assert!(
                report
                    .issues
                    .iter()
                    .any(|issue| issue.code == expected_code),
                "{case}: {:?}",
                report.issues
            );
            assert_eq!((report.memories_imported, report.links_imported), (0, 0));
            assert!(!options.workspace_path.exists());
            assert!(
                !database_path(&options)
                    .parent()
                    .ok_or("database parent")?
                    .exists()
            );
            assert_eq!(
                fs::read_to_string(&options.source_path).map_err(|e| e.to_string())?,
                source
            );
        }
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

#[test]
fn incomplete_backup_streams_are_rejected_before_creating_any_destination() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    for dry_run in [false, true] {
        for case in 0..10 {
            let mut records = rows();
            match case {
                0 => records[6]["success"] = json!(false),
                1 => records[6]["total_records"] = json!(8),
                2 => records[6]["memory_count"] = json!(3),
                3 => records[6]["tag_count"] = json!(3),
                4 => records[6]["link_count"] = json!(2),
                5 => records[6]["artifact_count"] = json!(1),
                6 => records[0]["workspace_id"] = json!(null),
                7 => records[0]["workspace_id"] = json!("  "),
                8 => records[1]["workspace_id"] = json!("PRIVATE-WORKSPACE-CANARY"),
                _ => {
                    // A dropped relationship must not become a successful
                    // partial restore merely because the remaining rows parse.
                    records.remove(5);
                }
            }
            let options = JsonlImportOptions {
                workspace_path: path.join(format!("absent-{dry_run}-{case}")),
                database_path: Some(path.join(format!("absent-db-{dry_run}-{case}/ee.db"))),
                source_path: path.join(format!("source-{dry_run}-{case}.jsonl")),
                dry_run,
            };
            let source = source_text(&records);
            fs::write(&options.source_path, &source).map_err(|e| e.to_string())?;
            let report =
                import_verified_backup_jsonl_records(&options).map_err(|e| e.to_string())?;
            assert_eq!(report.status, "rejected", "case {case}, dry_run={dry_run}");
            let issue = report
                .issues
                .iter()
                .find(|issue| issue.code == "invalid_backup_record_stream")
                .ok_or_else(|| {
                    format!(
                        "missing strict admission issue for case {case}: {:?}",
                        report.issues
                    )
                })?;
            assert_eq!(issue.severity, JsonlImportIssueSeverity::Error);
            assert!(!issue.message.contains("PRIVATE-WORKSPACE-CANARY"));
            assert!(
                !options.workspace_path.exists(),
                "case {case}, dry_run={dry_run}"
            );
            let database = database_path(&options);
            assert!(!database.exists());
            assert!(!database.parent().ok_or("database parent")?.exists());
            assert_eq!(
                fs::read_to_string(&options.source_path).map_err(|e| e.to_string())?,
                source
            );
        }
    }
    Ok(())
}

#[test]
fn ordinary_jsonl_preview_retains_its_warning_only_count_contract() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut records = rows();
    records[6]["success"] = json!(false);
    records[6]["memory_count"] = json!(99);
    let options = JsonlImportOptions {
        workspace_path: path.join("absent"),
        database_path: None,
        source_path: path.join("ordinary.jsonl"),
        dry_run: true,
    };
    fs::write(&options.source_path, source_text(&records)).map_err(|e| e.to_string())?;
    let report = import_jsonl_records(&options).map_err(|e| e.to_string())?;
    assert_ne!(report.status, "rejected", "{:?}", report.issues);
    for code in ["source_export_incomplete", "footer_memory_count_mismatch"] {
        assert!(
            report.issues.iter().any(|issue| {
                issue.code == code && issue.severity == JsonlImportIssueSeverity::Warning
            }),
            "{code}"
        );
    }
    assert!(
        !report
            .issues
            .iter()
            .any(|issue| issue.code == "invalid_backup_record_stream")
    );
    assert!(!options.workspace_path.exists());
    Ok(())
}

#[test]
fn early_backup_admission_and_final_recovery_verification_use_the_same_counts() -> TestResult {
    let mut records = rows();
    let fixture = Fixture::new(&records)?;
    fixture.verify().map_err(|e| e.to_string())?;
    records[6]["artifact_count"] = json!(1);
    let parsed = parse_jsonl_source(&source_text(&records));
    assert!(recovery::validate_backup_source(&parsed).is_err());
    fs::write(&fixture.options.source_path, source_text(&records)).map_err(|e| e.to_string())?;
    let error = fixture
        .verify()
        .expect_err("an omitted artifact must fail the final fence too");
    assert!(error.message().contains("counts disagree"));
    assert!(!error.to_string().contains(&fixture.workspace));
    assert_eq!(
        fixture
            .db
            .count_table_rows("memories")
            .map_err(|e| e.to_string())?,
        2
    );
    Ok(())
}
