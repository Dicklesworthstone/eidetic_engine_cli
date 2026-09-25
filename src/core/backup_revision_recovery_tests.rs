//! No-mock recovery through create, authenticate, restore and index publication.

use super::*;
use crate::db::{CreateMemoryInput, CreateMemoryLinkInput, MemoryLinkRelation, MemoryLinkSource};
use crate::models::{MemoryId, MemoryLinkId, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), String>;

#[path = "backup_primary_publication_tests.rs"]
mod publication;

fn source() -> Result<(tempfile::TempDir, PathBuf, PathBuf, String, String, String), String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = root
        .path()
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join("workspace");
    fs::create_dir_all(workspace.join(WORKSPACE_MARKER)).map_err(|e| e.to_string())?;
    let database = workspace.join(WORKSPACE_MARKER).join(DEFAULT_DB_FILE);
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    db.migrate().map_err(|e| e.to_string())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(11)).to_string();
    let prior = MemoryId::from_uuid(Uuid::from_u128(21)).to_string();
    let head = MemoryId::from_uuid(Uuid::from_u128(22)).to_string();
    db.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: workspace.to_string_lossy().into_owned(),
            name: Some("Recovery test".to_owned()),
        },
    )
    .map_err(|e| e.to_string())?;
    for (id, body, created, expiry) in [
        (
            &prior,
            "Café release builds used the legacy manifest.",
            "2026-05-01T00:00:00+00:00",
            Some("2099-01-01T00:00:00Z"),
        ),
        (
            &head,
            "Café release builds use the canonical manifest.",
            "2026-06-01T00:00:00+00:00",
            None,
        ),
    ] {
        db.insert_memory_with_timestamps(
            id,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: body.to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.7,
                importance: 0.8,
                provenance_uri: Some("ee-export://recovery".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: vec!["release".to_owned()],
                valid_from: Some(
                    if id == &prior {
                        "2026-05-01T00:00:00Z"
                    } else {
                        "2026-06-01T00:00:00Z"
                    }
                    .to_owned(),
                ),
                valid_to: expiry.map(str::to_owned),
            },
            created,
            created,
            &prior,
        )
        .map_err(|e| e.to_string())?;
    }
    db.restore_imported_memory_supersession(&prior, "2026-06-01T00:00:00Z")
        .map_err(|e| e.to_string())?;
    db.insert_memory_link_at(
        &MemoryLinkId::from_uuid(Uuid::from_u128(23)).to_string(),
        &CreateMemoryLinkInput {
            src_memory_id: prior.clone(),
            dst_memory_id: head.clone(),
            relation: MemoryLinkRelation::Supports,
            weight: 0.75,
            confidence: 0.5,
            directed: true,
            evidence_count: 2,
            last_reinforced_at: None,
            source: MemoryLinkSource::Agent,
            created_by: Some("recovery-test".to_owned()),
            metadata_json: None,
        },
        "2026-06-01T00:00:00+00:00",
    )
    .map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
    Ok((root, workspace, database, workspace_id, prior, head))
}

#[test]
fn backup_restore_and_rebackup_preserve_absent_and_explicit_provenance() -> TestResult {
    for redaction in [RedactionLevel::Minimal, RedactionLevel::Standard] {
        let (root, workspace, database, workspace_id, prior, _head) = source()?;
        let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        db.execute_raw(&format!(
            "UPDATE memories SET provenance_uri = NULL WHERE id = '{prior}'"
        ))
        .map_err(|error| error.to_string())?;
        db.restore_imported_memory_tombstone(&prior, "2026-06-02T00:00:00+00:00")
            .map_err(|error| error.to_string())?;
        let source_provenance = db
            .list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|memory| (memory.content, memory.provenance_uri))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(source_provenance.len(), 2);
        assert!(source_provenance.values().any(Option::is_none));
        assert!(source_provenance.values().any(Option::is_some));
        db.close().map_err(|error| error.to_string())?;

        let create = |workspace: &Path, database: &Path| {
            create_backup(&BackupCreateOptions {
                workspace_path: workspace.to_owned(),
                database_path: Some(database.to_owned()),
                output_dir: None,
                label: None,
                redaction_level: redaction,
                include_derived: false,
                include_graph_cache: false,
                dry_run: false,
            })
            .map_err(|error| error.to_string())
        };
        let exported = |backup: &Path| -> Result<BTreeMap<String, Option<String>>, String> {
            let text =
                fs::read_to_string(backup.join(RECORDS_FILE)).map_err(|error| error.to_string())?;
            text.lines()
                .map(serde_json::from_str::<JsonValue>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|row| row["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
                .map(|row| {
                    serde_json::from_value::<ExportMemoryRecord>(row)
                        .map(|record| (record.content, record.provenance_uri))
                        .map_err(|error| error.to_string())
                })
                .collect()
        };
        let created = create(&workspace, &database)?;
        assert_eq!(
            exported(Path::new(&created.backup_path))?,
            source_provenance
        );
        let side_path = root.path().join("restored");
        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;
        let restored_database = PathBuf::from(&restored.restored_database_path);
        let db = DbConnection::open_file(&restored_database).map_err(|error| error.to_string())?;
        let restored_provenance = db
            .list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|memory| (memory.content, memory.provenance_uri))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(restored_provenance, source_provenance);
        db.close().map_err(|error| error.to_string())?;
        let rebackup = create(&side_path, &restored_database)?;
        assert_eq!(
            exported(Path::new(&rebackup.backup_path))?,
            source_provenance
        );
    }
    Ok(())
}

#[test]
fn same_lineage_restore_preserves_native_trust_and_records_root() -> TestResult {
    for (redaction, rotate) in [
        (RedactionLevel::Minimal, false),
        (RedactionLevel::Minimal, true),
        (RedactionLevel::Standard, false),
    ] {
        let (root, workspace, database, workspace_id, prior, _) = source()?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.execute_raw("UPDATE memories SET trust_class = 'human_explicit'")
            .map_err(|e| e.to_string())?;
        db.execute_raw(&format!(
            "UPDATE memories SET provenance_uri = NULL WHERE id = '{prior}'"
        ))
        .map_err(|e| e.to_string())?;
        let expected = db
            .list_memories(&workspace_id, None, true)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|memory| (memory.content.clone(), memory))
            .collect::<BTreeMap<_, _>>();
        db.close().map_err(|e| e.to_string())?;
        let create = |workspace: &Path, database: &Path| {
            create_backup(&BackupCreateOptions {
                workspace_path: workspace.to_owned(),
                database_path: Some(database.to_owned()),
                output_dir: None,
                label: None,
                redaction_level: redaction,
                include_derived: false,
                include_graph_cache: false,
                dry_run: false,
            })
            .map_err(|e| e.to_string())
        };
        let created = create(&workspace, &database)?;
        if rotate {
            let mut keys =
                StoreAuthRoot::open(workspace_keys_dir(&workspace)).map_err(|e| e.message())?;
            let original_key = keys.current_key_id();
            keys.rotate().map_err(|e| e.message())?;
            assert_ne!(keys.current_key_id(), original_key);
            assert!(keys.window_key_ids().contains(&original_key));
        }
        // The same JSONL file cannot elevate an unrelated ordinary import.
        let ordinary_workspace = root.path().join("ordinary-import");
        let ordinary = crate::core::jsonl_import::import_jsonl_records(&JsonlImportOptions {
            workspace_path: ordinary_workspace.clone(),
            database_path: None,
            source_path: PathBuf::from(&created.records_path),
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        assert_eq!(ordinary.status, "rejected", "{:?}", ordinary.issues);
        assert_eq!(ordinary.memories_imported, 0);

        let side_path = root.path().join("restored-native");
        assert!(!workspace_keys_dir(&side_path).exists());
        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&created.backup_path),
            side_path: side_path.clone(),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        assert_eq!(restored.status, "completed", "{:?}", restored.degraded);
        assert_eq!(restored.imported_memory_count, 2);
        assert!(restored.degraded.is_empty());
        // Restoration carries verified rows, never the source's private keys.
        assert!(!workspace_keys_dir(&side_path).exists());
        let restored_database = PathBuf::from(&restored.restored_database_path);
        let db =
            DbConnection::open_file_read_only(&restored_database).map_err(|e| e.to_string())?;
        let memories = db
            .list_memories(&workspace_id, None, true)
            .map_err(|e| e.to_string())?;
        assert_eq!(memories.len(), expected.len());
        for memory in memories {
            let original = expected
                .get(&memory.content)
                .ok_or("unexpected restored body")?;
            assert_eq!(memory.trust_class, "human_explicit");
            assert_eq!(memory.trust_subclass, original.trust_subclass);
            assert_eq!(memory.provenance_uri, original.provenance_uri);
            assert_eq!(memory.created_at, original.created_at);
            assert_eq!(memory.updated_at, original.updated_at);
            assert_eq!(memory.valid_from, original.valid_from);
            assert_eq!(memory.valid_to, original.valid_to);
        }
        db.close().map_err(|e| e.to_string())?;
        let rebackup = create(&side_path, &restored_database)?;
        if redaction == RedactionLevel::Minimal {
            let authenticated_records = |path: &str| -> Result<(String, Vec<String>), String> {
                let source = fs::read_to_string(path).map_err(|e| e.to_string())?;
                let mut root = None;
                let mut records = Vec::new();
                for line in source.lines() {
                    let row: JsonValue = serde_json::from_str(line).map_err(|e| e.to_string())?;
                    match row["schema"].as_str() {
                        Some(
                            crate::models::EXPORT_MEMORY_SCHEMA_V1
                            | crate::models::EXPORT_TAG_SCHEMA_V1
                            | crate::models::EXPORT_LINK_SCHEMA_V1,
                        ) => records.push(line.to_owned()),
                        Some(crate::models::EXPORT_FOOTER_SCHEMA_V1) => {
                            root = row
                                .pointer("/authentication/recordsRoot")
                                .and_then(JsonValue::as_str)
                                .map(str::to_owned);
                        }
                        _ => {}
                    }
                }
                Ok((
                    root.ok_or("backup has no authenticated records root")?,
                    records,
                ))
            };
            let original = authenticated_records(&created.records_path)?;
            assert!(!original.1.is_empty());
            assert_eq!(
                authenticated_records(&rebackup.records_path)?,
                original,
                "same-lineage recovery preserves every authenticated memory, tag and link byte"
            );
        }
    }
    Ok(())
}

#[test]
fn recovery_fences_reject_loss_of_authenticated_native_trust() -> TestResult {
    for final_fence in [false, true] {
        let (root, workspace, database, workspace_id, _, _) = source()?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.execute_raw("UPDATE memories SET trust_class = 'human_explicit'")
            .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: None,
            label: None,
            redaction_level: RedactionLevel::Minimal,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        let mutate = |path: &Path| -> Result<(), DomainError> {
            let db = DbConnection::open_file(path).map_err(work_history_error)?;
            let before = db
                .list_memories(&workspace_id, None, true)
                .map_err(work_history_error)?;
            assert_eq!(before.len(), 2);
            assert!(
                before
                    .iter()
                    .all(|memory| memory.trust_class == "human_explicit")
            );
            db.execute_raw("UPDATE memories SET trust_class = 'agent_validated'")
                .map_err(work_history_error)?;
            db.close().map_err(work_history_error)?;
            Ok(())
        };
        let side_path = root.path().join("refused-trust-loss");
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &BackupRestoreOptions {
                workspace_path: workspace,
                backup_path: PathBuf::from(&created.backup_path),
                side_path: side_path.clone(),
                restore_graph_cache: false,
                dry_run: false,
            },
            |path| if final_fence { Ok(()) } else { mutate(path) },
            |path| if final_fence { mutate(path) } else { Ok(()) },
        )
        .err()
        .ok_or("recovery published a trust downgrade after authenticated admission")?;
        assert!(
            error.message().contains("Restored memory graph"),
            "{}",
            error.message()
        );
        assert!(!side_path.join(WORKSPACE_MARKER).exists());
        let db = DbConnection::open_file_read_only(database).map_err(|e| e.to_string())?;
        assert!(
            db.list_memories(&workspace_id, None, true)
                .map_err(|e| e.to_string())?
                .iter()
                .all(|memory| memory.trust_class == "human_explicit")
        );
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[test]
fn verified_manifest_without_record_authentication_reports_trust_downgrade() -> TestResult {
    let (root, workspace, database, workspace_id, _, _) = source()?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    db.execute_raw("UPDATE memories SET trust_class = 'human_explicit'")
        .map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
    let created = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database),
        output_dir: None,
        label: None,
        redaction_level: RedactionLevel::Minimal,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.to_string())?;
    let original = fs::read_to_string(&created.records_path).map_err(|e| e.to_string())?;
    let mut rows = original.lines().map(str::to_owned).collect::<Vec<_>>();
    let footer_line = rows.last_mut().ok_or("backup footer")?;
    let mut footer: JsonValue = serde_json::from_str(footer_line).map_err(|e| e.to_string())?;
    assert_eq!(footer["schema"], crate::models::EXPORT_FOOTER_SCHEMA_V1);
    assert!(footer["authentication"].is_object());
    footer["authentication"] = JsonValue::Null;
    *footer_line = footer.to_string();
    let changed = rows.join("\n") + "\n";
    fs::write(&created.records_path, &changed).map_err(|e| e.to_string())?;
    let (_, mut manifest) =
        read_backup_manifest(Path::new(&created.backup_path)).map_err(|e| e.to_string())?;
    let artifact = manifest["artifacts"]
        .as_array_mut()
        .ok_or("manifest artifacts")?
        .iter_mut()
        .find(|artifact| artifact["path"] == RECORDS_FILE)
        .ok_or("records artifact")?;
    artifact["hash"] = serde_json::json!(hash_bytes(changed.as_bytes()));
    artifact["sizeBytes"] = serde_json::json!(changed.len());
    let source_auth =
        StoreAuthRoot::open(workspace_keys_dir(&workspace)).map_err(|e| e.message())?;
    authenticate_backup_manifest(&mut manifest, &source_auth).map_err(|e| e.to_string())?;
    fs::write(
        &created.manifest_path,
        serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: workspace.clone(),
        backup_path: PathBuf::from(&created.backup_path),
    })
    .map_err(|e| e.to_string())?;
    assert_eq!(verified.status, "verified", "{:?}", verified.issues);
    // The outer signature proves archive integrity, but cannot replace the
    // native records MAC. This intentional defence-in-depth fallback is visible.
    let restored = restore_backup_to_side_path(&BackupRestoreOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(&created.backup_path),
        side_path: root.path().join("restored-capped"),
        restore_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.to_string())?;
    assert_eq!(restored.status, "degraded");
    assert_eq!(restored.import_status, "completed");
    assert_eq!(restored.imported_memory_count, 2);
    let downgrade = restored
        .degraded
        .iter()
        .find(|entry| {
            entry.code == crate::core::jsonl_import::VERIFIED_BACKUP_TRUST_DOWNGRADED_CODE
        })
        .ok_or("restore hid its native trust downgrade")?;
    assert_eq!(downgrade.severity, "warning");
    assert!(downgrade.message.starts_with("2 restored memory record(s)"));
    assert!(downgrade.next_action.contains("ee backup keys import"));
    let db = DbConnection::open_file_read_only(&restored.restored_database_path)
        .map_err(|e| e.to_string())?;
    let memories = db
        .list_memories(&workspace_id, None, true)
        .map_err(|e| e.to_string())?;
    assert_eq!(memories.len(), 2);
    assert!(
        memories
            .iter()
            .all(|memory| memory.trust_class == "agent_validated")
    );
    db.close().map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn backup_successor_hints_use_durable_headship_and_timestamp_instants() -> TestResult {
    let (_root, _workspace, database, workspace_id, prior, head) = source()?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let mut memories = db
        .list_memories(&workspace_id, None, true)
        .map_err(|e| e.to_string())?;
    let mut first = memories
        .iter()
        .find(|memory| memory.id == prior)
        .ok_or("prior revision")?
        .clone();
    first.id = MemoryId::from_uuid(Uuid::from_u128(20)).to_string();
    let first_id = first.id.clone();
    memories.push(first);
    db.close().map_err(|e| e.to_string())?;
    let logical_ids = memories
        .iter()
        .map(|memory| (memory.id.clone(), prior.clone()))
        .collect::<BTreeMap<_, _>>();
    let markers = BTreeMap::from([
        (first_id.clone(), "2026-06-01T00:00:00Z".to_owned()),
        (prior.clone(), "2026-06-01T00:00:00Z".to_owned()),
    ]);
    for (first_at, prior_at, head_at) in [
        (
            "2026-06-01T00:00:00.000+00:00",
            "2026-06-01T00:00:00+00:00",
            "2026-06-01T00:00:00Z",
        ),
        (
            "2026-06-01T00:00:00.000Z",
            "2026-06-01T00:00:00+00:00",
            "2026-06-01T00:00:00.000+00:00",
        ),
        (
            "2026-06-01T01:00:00+01:00",
            "2026-06-01T00:00:00Z",
            "2026-06-01T00:00:00+00:00",
        ),
        (
            "2026-06-02T00:00:00Z",
            "2026-06-03T00:00:00Z",
            "2026-06-01T00:00:00Z",
        ),
    ] {
        for memory in &mut memories {
            memory.created_at = if memory.id == first_id {
                first_at
            } else if memory.id == prior {
                prior_at
            } else {
                head_at
            }
            .to_owned();
        }
        assert_eq!(
            superseded_by_within_export(&memories, &logical_ids, &markers)
                .map_err(|e| e.to_string())?,
            BTreeMap::from([
                (first_id.clone(), prior.clone()),
                (prior.clone(), head.clone()),
            ]),
            "{first_at}, {prior_at}, {head_at}"
        );
    }
    // A guessed ordering must never manufacture headship for an ambiguous
    // family, or invent a terminal after every stored row was superseded.
    let mut ambiguous = markers.clone();
    ambiguous.remove(&prior);
    assert!(
        superseded_by_within_export(&memories, &logical_ids, &ambiguous)
            .map_err(|e| e.to_string())?
            .is_empty()
    );
    let mut malformed = memories.clone();
    malformed[0].created_at = "PRIVATE_INVALID_TIMESTAMP".to_owned();
    let error = superseded_by_within_export(&malformed, &logical_ids, &markers)
        .expect_err("a malformed timestamp must not produce an arbitrary successor ordering");
    assert!(
        error
            .message()
            .contains("invalid durable revision timestamp")
    );
    assert!(!error.message().contains("PRIVATE_INVALID_TIMESTAMP"));
    let mut no_terminal = markers;
    no_terminal.insert(head, "2026-06-04T00:00:00Z".to_owned());
    assert!(
        superseded_by_within_export(&memories, &logical_ids, &no_terminal)
            .map_err(|e| e.to_string())?
            .is_empty()
    );
    Ok(())
}

#[test]
fn backup_revision_recovery_keeps_expiring_head_with_inverted_timestamp_spellings() -> TestResult {
    for redaction in [RedactionLevel::None, RedactionLevel::Standard] {
        let (root, workspace, database, _workspace_id, prior, head) = source()?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        // These are the same instant, but lexical ordering puts the head
        // first. The current revision also has a future author expiry, which
        // must not be mistaken for the old supersession encoding.
        db.execute_raw(&format!(
            "UPDATE memories SET created_at = '2026-06-01T01:00:00+01:00' \
             WHERE id = '{prior}'"
        ))
        .map_err(|e| e.to_string())?;
        db.execute_raw(&format!(
            "UPDATE memories SET created_at = '2026-06-01T00:00:00.000Z', \
             valid_to = '2099-01-01T00:00:00Z' WHERE id = '{head}'"
        ))
        .map_err(|e| e.to_string())?;
        assert_eq!(
            db.filter_current_memory_ids(&[prior.clone(), head.clone()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([head.clone()])
        );
        db.close().map_err(|e| e.to_string())?;
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: None,
            label: None,
            redaction_level: redaction,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        let backup = PathBuf::from(&created.backup_path);
        let lines = fs::read_to_string(backup.join(RECORDS_FILE)).map_err(|e| e.to_string())?;
        let memories = lines
            .lines()
            .map(serde_json::from_str::<JsonValue>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|value| value["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
            .map(serde_json::from_value::<ExportMemoryRecord>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let exported_head = crate::output::jsonl_export::redact_identifier(&head, redaction);
        let head_record = memories
            .iter()
            .find(|record| record.memory_id == exported_head)
            .ok_or("exported current revision")?;
        assert!(head_record.superseded_by.is_none());
        assert_eq!(head_record.superseded_at, Some(None));
        assert_eq!(
            head_record.valid_to.as_deref(),
            Some("2099-01-01T00:00:00Z")
        );
        let restored_head = import_memory_id(head_record, redaction).map_err(|e| e.message)?;
        let exported_prior = crate::output::jsonl_export::redact_identifier(&prior, redaction);
        let prior_record = memories
            .iter()
            .find(|record| record.memory_id == exported_prior)
            .ok_or("exported historical revision")?;
        assert_eq!(
            prior_record.superseded_by.as_deref(),
            Some(exported_head.as_str())
        );
        let restored_prior = import_memory_id(prior_record, redaction).map_err(|e| e.message)?;
        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: backup,
            side_path: root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("restored"),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        let db =
            DbConnection::open_file(&result.restored_database_path).map_err(|e| e.to_string())?;
        assert_eq!(
            db.filter_current_memory_ids(&[restored_prior, restored_head.clone()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([restored_head.clone()])
        );
        assert!(
            db.get_memory_superseded_at(&restored_head)
                .map_err(|e| e.to_string())?
                .is_none()
        );
        assert_eq!(
            db.get_memory(&restored_head)
                .map_err(|e| e.to_string())?
                .ok_or("restored current revision")?
                .valid_to
                .as_deref(),
            Some("2099-01-01T00:00:00Z")
        );
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[test]
fn backup_revision_recovery_preserves_null_markers_beside_a_tombstoned_terminal() -> TestResult {
    for redaction in [RedactionLevel::None, RedactionLevel::Standard] {
        let (root, workspace, database, _workspace_id, prior, head) = source()?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        // A tombstoned later row does not retire the live, expiring revision.
        // Both markers are null, so this family has no safe successor hints.
        db.execute_raw(&format!(
            "UPDATE memories SET superseded_at = NULL WHERE id = '{prior}'"
        ))
        .map_err(|e| e.to_string())?;
        db.restore_imported_memory_tombstone(&head, "2026-06-02T00:00:00Z")
            .map_err(|e| e.to_string())?;
        assert_eq!(
            db.filter_current_memory_ids(&[prior.clone(), head.clone()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([prior.clone()])
        );
        db.close().map_err(|e| e.to_string())?;
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database),
            output_dir: None,
            label: None,
            redaction_level: redaction,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        let backup = PathBuf::from(&created.backup_path);
        let lines = fs::read_to_string(backup.join(RECORDS_FILE)).map_err(|e| e.to_string())?;
        let memories = lines
            .lines()
            .map(serde_json::from_str::<JsonValue>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|value| value["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
            .collect::<Vec<_>>();
        assert_eq!(memories.len(), 2);
        let mut restored_ids = Vec::new();
        let mut current_id = None;
        for wire in memories {
            assert_eq!(wire.get("superseded_at"), Some(&JsonValue::Null));
            let record: ExportMemoryRecord =
                serde_json::from_value(wire).map_err(|e| e.to_string())?;
            assert!(record.superseded_by.is_none());
            let restored_id = import_memory_id(&record, redaction).map_err(|e| e.message)?;
            if record.tombstoned_at.is_none() {
                assert_eq!(record.valid_to.as_deref(), Some("2099-01-01T00:00:00Z"));
                current_id = Some(restored_id.clone());
            }
            restored_ids.push(restored_id);
        }
        let current_id = current_id.ok_or("current revision")?;
        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: backup,
            side_path: root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("restored"),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        let db =
            DbConnection::open_file(&result.restored_database_path).map_err(|e| e.to_string())?;
        assert_eq!(
            db.filter_current_memory_ids(&restored_ids)
                .map_err(|e| e.to_string())?,
            BTreeSet::from([current_id.clone()])
        );
        for id in restored_ids {
            assert!(
                db.get_memory_superseded_at(&id)
                    .map_err(|e| e.to_string())?
                    .is_none()
            );
        }
        assert_eq!(
            db.get_memory(&current_id)
                .map_err(|e| e.to_string())?
                .ok_or("restored current revision")?
                .valid_to
                .as_deref(),
            Some("2099-01-01T00:00:00Z")
        );
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[test]
fn backup_revision_recovery_preserves_history_expiry_and_published_index() -> TestResult {
    for redaction in [
        RedactionLevel::None,
        RedactionLevel::Strict,
        RedactionLevel::Standard,
    ] {
        let (root, workspace, database, workspace_id, prior, head) = source()?;
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: None,
            label: None,
            redaction_level: redaction,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        assert!(created.recovery_inventory.snapshot_coverage_complete);
        let backup = PathBuf::from(&created.backup_path);
        let lines = fs::read_to_string(backup.join(RECORDS_FILE)).map_err(|e| e.to_string())?;
        let memories: Vec<ExportMemoryRecord> = lines
            .lines()
            .filter_map(|line| {
                let value: JsonValue = serde_json::from_str(line).ok()?;
                (value["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
                    .then(|| serde_json::from_value(value))
            })
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        assert_eq!(memories.len(), 2);
        let exported_prior = crate::output::jsonl_export::redact_identifier(&prior, redaction);
        let exported_head = crate::output::jsonl_export::redact_identifier(&head, redaction);
        let old = memories
            .iter()
            .find(|m| m.memory_id == exported_prior)
            .ok_or("exported prior")?;
        assert_eq!(
            old.superseded_at.as_ref().and_then(Option::as_deref),
            Some("2026-06-01T00:00:00Z")
        );
        assert_eq!(old.superseded_by.as_deref(), Some(exported_head.as_str()));
        let restored_prior = import_memory_id(old, redaction).map_err(|e| e.message)?;
        let restored_head = import_memory_id(
            memories
                .iter()
                .find(|m| m.memory_id == exported_head)
                .ok_or("exported head")?,
            redaction,
        )
        .map_err(|e| e.message)?;
        let side_path = root
            .path()
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("restored");
        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: backup,
            side_path: side_path.clone(),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        assert_eq!(result.imported_memory_count, 2);
        let db =
            DbConnection::open_file(&result.restored_database_path).map_err(|e| e.to_string())?;
        assert_eq!(
            db.get_memory_superseded_at(&restored_prior)
                .map_err(|e| e.to_string())?
                .as_deref(),
            Some("2026-06-01T00:00:00Z")
        );
        assert_eq!(
            db.get_memory(&restored_prior)
                .map_err(|e| e.to_string())?
                .ok_or("restored prior")?
                .valid_to
                .as_deref(),
            Some("2099-01-01T00:00:00Z")
        );
        assert_eq!(
            db.filter_current_memory_ids(&[restored_prior.clone(), restored_head.clone()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([restored_head.clone()])
        );
        assert_eq!(
            db.list_memories(&workspace_id, None, true)
                .map_err(|e| e.to_string())?
                .len(),
            2
        );
        db.close().map_err(|e| e.to_string())?;
        let status =
            crate::core::index::get_index_status(&crate::core::index::IndexStatusOptions {
                workspace_path: side_path.clone(),
                database_path: Some(PathBuf::from(&result.restored_database_path)),
                index_dir: None,
            })
            .map_err(|e| e.to_string())?;
        assert_eq!(status.health, crate::core::index::IndexHealth::Ready);
        assert_eq!(status.db_generation, status.index_generation);
        // Historical search requires both physical versions in the index.
        // One current DB head is not a one-document index. Prove the public
        // temporal behavior instead of discarding history to satisfy a count.
        assert_eq!(status.index_document_count, Some(2));
        for (reference, expected) in [
            ("2026-07-01T00:00:00Z", restored_head.as_str()),
            ("2026-05-15T00:00:00Z", restored_prior.as_str()),
            ("2026-06-01T00:00:00Z", restored_head.as_str()),
        ] {
            let search =
                crate::core::search::run_search_unaudited(&crate::core::search::SearchOptions {
                    workspace_path: side_path.clone(),
                    database_path: Some(PathBuf::from(&result.restored_database_path)),
                    index_dir: None,
                    query: "release builds manifest".to_owned(),
                    limit: 10,
                    speed: crate::search::SpeedMode::Default,
                    explain: false,
                    as_of: Some(
                        chrono::DateTime::parse_from_rfc3339(reference)
                            .map_err(|e| e.to_string())?
                            .with_timezone(&Utc),
                    ),
                    include_tombstoned: false,
                    include_expired: false,
                    include_future: false,
                    include_stale: false,
                    relevance_floor: Some(0.0),
                    dedup_mode: crate::core::search::SearchDedupMode::DocId,
                    source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
                    strict_source_mode: true,
                    memory_scope: crate::models::MemoryScope::Workspace,
                    strict_scope: false,
                })
                .map_err(|e| e.to_string())?;
            assert_eq!(
                search
                    .results
                    .iter()
                    .map(|hit| hit.doc_id.as_str())
                    .collect::<Vec<_>>(),
                vec![expected],
                "{redaction:?} at {reference}: {:?}",
                search.degraded,
            );
        }
    }
    Ok(())
}

#[test]
fn backup_revision_recovery_blocks_same_count_corruption_before_publication() -> TestResult {
    let (root, workspace, database, _, _, head) = source()?;
    let created = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        output_dir: None,
        label: None,
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.to_string())?;
    let side_path = root
        .path()
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join("refused");
    let options = BackupRestoreOptions {
        workspace_path: workspace.clone(),
        backup_path: PathBuf::from(&created.backup_path),
        side_path: side_path.clone(),
        restore_graph_cache: false,
        dry_run: false,
    };
    let failed = restore_backup_to_side_path_with_verification_hook(&options, |staged| {
        let db = DbConnection::open_file(staged).map_err(work_history_error)?;
        let original = db
            .get_memory(&head)
            .map_err(work_history_error)?
            .ok_or_else(|| work_history_error("fixture head absent"))?;
        assert!(
            db.update_memory_trust_class(&head, "agent_assertion")
                .map_err(work_history_error)?
        );
        db.restore_imported_memory_updated_at(&head, &original.updated_at)
            .map_err(work_history_error)?;
        assert_eq!(
            db.count_table_rows("memories")
                .map_err(work_history_error)?,
            2
        );
        db.close().map_err(work_history_error)
    })
    .expect_err("same-count corruption must prevent publication");
    assert!(
        failed.message().contains("Restored memory graph"),
        "{}",
        failed.message()
    );
    assert!(!side_path.join(WORKSPACE_MARKER).exists());
    assert!(
        side_path
            .read_dir()
            .map_err(|e| e.to_string())?
            .next()
            .is_some(),
        "unpublished diagnostic staging retained"
    );
    let source = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    assert_eq!(
        source
            .get_memory(&head)
            .map_err(|e| e.to_string())?
            .ok_or("source head")?
            .trust_class,
        "agent_validated"
    );
    source.close().map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(created.backup_path),
    })
    .map_err(|e| e.to_string())?;
    assert_eq!(verified.status, "verified");
    Ok(())
}
