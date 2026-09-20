//! A real backup must preserve usable typed memory, not just the prose body.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::super::*;
use crate::db::CreateMemoryInput;
use crate::models::memory::{
    canonicalize_typed_memory_fields_json, typed_memory_index_metadata_from_json,
};
use crate::models::{MemoryId, MemoryKind, WorkspaceId};
use uuid::Uuid;

const TIME: &str = "2026-09-01T00:00:00+00:00";
const REVISIT: &str = "2030-01-01T00:00:00Z";

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "rule",
            json!({"condition":"release","action":"verify","exceptions":["documentation"]}),
        ),
        (
            "failure",
            json!({"cause":"compile","regression_surface":"parser","family":"build","reverted_at_sha":"abcdef"}),
        ),
        (
            "decision",
            json!({"chosen":"CypressStore","options":["OakStore","CypressStore"],"rationale":"transaction isolation","revisit_by":REVISIT}),
        ),
        (
            "command",
            json!({"command":"cargo check","when_to_use":"before release","exit_meaning":"zero means success"}),
        ),
        (
            "risk",
            json!({"trigger":"disk full","blast_radius":"workspace","safer_alternative":"check free space"}),
        ),
        (
            "anti-pattern",
            json!({"trigger":"shared mutation","blast_radius":"state","safer_alternative":"isolate writes"}),
        ),
        (
            "convention",
            json!({"scope":"Rust","pattern":"explicit errors"}),
        ),
    ]
}

fn id(n: usize) -> String {
    MemoryId::from_uuid(Uuid::from_u128(100 + n as u128)).to_string()
}

fn source() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let (root, workspace, database) = tests::fixture().unwrap();
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let db = DbConnection::open_file(&database).unwrap();
    for (n, (kind, fields)) in cases().into_iter().enumerate() {
        db.insert_memory_with_timestamps(
            &id(n),
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "episodic".to_owned(),
                kind: kind.to_owned(),
                // Deliberately does not contain chosen/options/revisit data: the
                // live consumers must use the sidecar rather than parse a fallback.
                content: format!("Typed carrier for {kind}."),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.7,
                provenance_uri: Some("ee-test://typed-recovery".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: vec![],
                valid_from: Some("2026-09-01T00:00:00Z".to_owned()),
                valid_to: None,
            },
            TIME,
            TIME,
            &id(n),
        )
        .unwrap();
        assert!(
            db.set_memory_typed_fields_json(&id(n), Some(&fields.to_string()))
                .unwrap()
        );
        db.restore_imported_memory_updated_at(&id(n), TIME).unwrap();
    }
    // Include typed historical state: data retention is not limited to heads.
    db.restore_imported_memory_tombstone(&id(0), TIME).unwrap();
    db.restore_imported_memory_supersession(&id(1), "2026-09-02T00:00:00Z")
        .unwrap();
    db.close().unwrap();
    (root, workspace, database, workspace_id)
}

fn create(workspace: &Path, database: &Path, level: RedactionLevel) -> BackupCreateReport {
    create_backup(&BackupCreateOptions {
        workspace_path: workspace.to_owned(),
        database_path: Some(database.to_owned()),
        output_dir: None,
        label: None,
        redaction_level: level,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .unwrap()
}

fn options(
    root: &Path,
    workspace: &Path,
    created: &BackupCreateReport,
    round: usize,
) -> BackupRestoreOptions {
    BackupRestoreOptions {
        workspace_path: workspace.to_owned(),
        backup_path: PathBuf::from(&created.backup_path),
        side_path: root
            .canonicalize()
            .unwrap()
            .join(format!("restored-{round}")),
        restore_graph_cache: false,
        dry_run: false,
    }
}

fn snapshot(db: &DbConnection, workspace: &str) -> Vec<(String, Option<String>)> {
    db.list_memories(workspace, None, true)
        .unwrap()
        .into_iter()
        .map(|memory| {
            let fields = db.get_memory_typed_fields_json(&memory.id).unwrap();
            (memory.id, fields)
        })
        .collect()
}

#[test]
fn all_typed_kinds_survive_two_backups_and_restore_live_decision_consumers() {
    for level in [
        RedactionLevel::None,
        RedactionLevel::Strict,
        RedactionLevel::Standard,
    ] {
        let (root, mut workspace, mut database, workspace_id) = source();
        for round in 0..2 {
            let created = create(&workspace, &database, level);
            let text =
                fs::read_to_string(Path::new(&created.backup_path).join(RECORDS_FILE)).unwrap();
            let records = text
                .lines()
                .filter_map(|line| {
                    let value: JsonValue = serde_json::from_str(line).unwrap();
                    (value["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
                        .then(|| serde_json::from_value::<ExportMemoryRecord>(value).unwrap())
                })
                .collect::<Vec<_>>();
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record.typed_fields.is_some())
                    .count(),
                7
            );
            let opts = options(root.path(), &workspace, &created, round);
            let restored = restore_backup_to_side_path(&opts).unwrap();
            let db = DbConnection::open_file(&restored.restored_database_path).unwrap();
            let actual = snapshot(&db, &workspace_id);
            for record in &records {
                let restored_id = import_memory_id(record, level).unwrap();
                let kind: MemoryKind = record.kind.parse().unwrap();
                let expected = record.typed_fields.as_ref().map(|value| {
                    canonicalize_typed_memory_fields_json(&kind, &value.to_string()).unwrap()
                });
                assert_eq!(
                    db.get_memory_typed_fields_json(&restored_id).unwrap(),
                    expected
                );
                if let Some(raw) = expected {
                    // The exact registry metadata consumed by field-qualified
                    // indexing remains available for restored rows.
                    let metadata = typed_memory_index_metadata_from_json(&kind, &raw).unwrap();
                    if matches!(
                        kind,
                        MemoryKind::Decision | MemoryKind::Command | MemoryKind::Convention
                    ) {
                        assert!(!metadata.is_empty());
                    }
                }
                assert!(actual.iter().any(|(id, _)| *id == restored_id));
            }
            db.close().unwrap();
            let path = PathBuf::from(&restored.restored_database_path);
            let listed =
                crate::core::decide::decide_list(&crate::core::decide::DecideListOptions {
                    workspace_path: &opts.side_path,
                    database_path: Some(&path),
                    about: None,
                    include_superseded: false,
                    limit: 10,
                    now: None,
                })
                .unwrap();
            assert_eq!(listed.decisions.len(), 1);
            assert_eq!(listed.decisions[0].chosen, "CypressStore");
            assert_eq!(
                listed.decisions[0].options,
                vec!["OakStore", "CypressStore"]
            );
            assert_eq!(listed.decisions[0].revisit_by.as_deref(), Some(REVISIT));
            let resume =
                crate::core::resume::build_resume_report(&crate::core::resume::ResumeOptions {
                    workspace_path: &opts.side_path,
                    database_path: &path,
                    sessions: 2,
                })
                .unwrap();
            assert_eq!(resume.open_loops.revisit_decisions_total, 1);
            assert_eq!(
                resume.open_loops.revisit_decisions[0].chosen,
                "CypressStore"
            );
            assert_eq!(
                resume.open_loops.revisit_decisions[0].revisit_by.as_deref(),
                Some(REVISIT)
            );
            // Use the real rebuilt lexical index, then the public typed
            // filter, rather than merely proving that metadata can be parsed.
            use crate::core::search::{
                SearchDedupMode, SearchOptions, SearchSourceMode, TypedMemoryFieldFilter,
                apply_memory_kind_and_typed_field_filters_to_report, run_search,
            };
            let search_options = SearchOptions {
                workspace_path: opts.side_path.clone(),
                database_path: Some(path.clone()),
                index_dir: None,
                query: "Typed carrier".to_owned(),
                limit: 20,
                speed: crate::search::SpeedMode::Default,
                explain: true,
                as_of: None,
                include_tombstoned: false,
                include_expired: false,
                include_future: false,
                include_stale: false,
                relevance_floor: None,
                dedup_mode: SearchDedupMode::DocId,
                source_mode: SearchSourceMode::LexicalOnly,
                strict_source_mode: true,
                memory_scope: crate::models::MemoryScope::Swarm,
                strict_scope: false,
            };
            let mut search = run_search(&search_options).unwrap();
            assert_eq!(search.source_mode_applied, SearchSourceMode::LexicalOnly);
            assert!(!search.source_mode_fallback);
            apply_memory_kind_and_typed_field_filters_to_report(
                &search_options,
                &mut search,
                Some("decision"),
                &[TypedMemoryFieldFilter::parse("chosen=CypressStore").unwrap()],
            )
            .unwrap();
            assert_eq!(search.results.len(), 1);
            let decision_record = records.iter().find(|row| row.kind == "decision").unwrap();
            assert_eq!(
                search.results[0].doc_id,
                import_memory_id(decision_record, level).unwrap()
            );
            workspace = opts.side_path;
            database = path;
        }
    }
}

fn refuse_typed_corruption(sql: &str) {
    for late in [false, true] {
        let (root, workspace, database, workspace_id) = source();
        let created = create(&workspace, &database, RedactionLevel::None);
        let opts = options(root.path(), &workspace, &created, 0);
        let db = DbConnection::open_file(&database).unwrap();
        let before = snapshot(&db, &workspace_id);
        db.close().unwrap();
        let changed = std::cell::Cell::new(false);
        let mutate = |path: &Path| -> Result<(), DomainError> {
            let db = DbConnection::open_file(path).unwrap();
            let count = db.count_table_rows("memories").unwrap();
            let fields = snapshot(&db, &workspace_id);
            if late {
                assert!(path.parent().unwrap().join("index/meta.json").is_file());
            }
            db.execute_raw(sql).unwrap();
            assert_ne!(fields, snapshot(&db, &workspace_id));
            assert_eq!(count, db.count_table_rows("memories").unwrap());
            db.close().unwrap();
            changed.set(true);
            Ok(())
        };
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &opts,
            |path| if late { Ok(()) } else { mutate(path) },
            |path| if late { mutate(path) } else { Ok(()) },
        )
        .unwrap_err();
        assert!(changed.get(), "{}", error.message());
        assert!(
            error.message().contains("typed memory fields"),
            "{}",
            error.message()
        );
        assert!(!error.message().contains("TYPED_PRIVATE_CANARY"));
        assert!(!opts.side_path.join(WORKSPACE_MARKER).exists());
        let db = DbConnection::open_file(&database).unwrap();
        assert_eq!(snapshot(&db, &workspace_id), before);
        db.close().unwrap();
        assert_eq!(
            verify_backup(&BackupVerifyOptions {
                workspace_path: workspace,
                backup_path: opts.backup_path
            })
            .unwrap()
            .status,
            "verified"
        );
    }
}

#[test]
fn both_recovery_fences_reject_missing_typed_fields() {
    refuse_typed_corruption("UPDATE memories SET typed_fields_json = NULL WHERE kind = 'decision'");
}

#[test]
fn both_recovery_fences_reject_changed_typed_values() {
    refuse_typed_corruption(
        "UPDATE memories SET typed_fields_json = '{\"chosen\":\"TYPED_PRIVATE_CANARY\"}' WHERE kind = 'decision'",
    );
}

#[test]
fn both_recovery_fences_reject_invented_sidecars_on_legacy_rows() {
    refuse_typed_corruption(
        "UPDATE memories SET typed_fields_json = '{\"action\":\"TYPED_PRIVATE_CANARY\"}' WHERE kind = 'rule' AND typed_fields_json IS NULL",
    );
}

#[test]
fn malformed_durable_sidecar_cannot_produce_a_successful_backup() {
    let (_root, workspace, database, _) = source();
    let db = DbConnection::open_file(&database).unwrap();
    db.execute_raw(
        "UPDATE memories SET typed_fields_json = '{\"chosen\":42}' WHERE kind = 'decision'",
    )
    .unwrap();
    db.close().unwrap();
    let error = create_backup(&BackupCreateOptions {
        workspace_path: workspace,
        database_path: Some(database),
        output_dir: None,
        label: None,
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .unwrap_err();
    assert!(error.message().contains("typed memory fields"));
}
