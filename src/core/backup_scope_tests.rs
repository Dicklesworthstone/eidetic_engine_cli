//! The real shared store must yield independently restorable workspace capsules.

use super::*;
use crate::core::backup::*;
use crate::db::{
    CreateAuditInput, CreateEvidenceSpanInput, CreateMemoryInput, CreateMemoryLinkInput,
    CreateProceduralRuleInput, CreateRecorderRunInput, CreateSessionInput, CreateWorkspaceInput,
    EvidenceProducerKind, MemoryLinkRelation, MemoryLinkSource,
};
use crate::models::{EvidenceId, MemoryId, RedactionLevel, RuleId, SessionId, WorkspaceId};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const TIME: &str = "2026-09-01T00:00:00Z";

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    workspaces: Vec<PathBuf>,
    ids: Vec<String>,
    memories: Vec<Vec<String>>,
    rules: Vec<String>,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("shared.db");
    let db = DbConnection::open_file(&database).unwrap();
    db.migrate().unwrap();
    let mut fixture = Fixture {
        root,
        database,
        workspaces: vec![],
        ids: vec![],
        memories: vec![],
        rules: vec![],
    };
    for number in 0..2 {
        let name = if number == 0 {
            "CopperKestrel"
        } else {
            "VioletHeron"
        };
        let path = fixture.root.path().join(name);
        std::fs::create_dir_all(path.join(".ee")).unwrap();
        let path = path.canonicalize().unwrap();
        let id = WorkspaceId::from_uuid(Uuid::from_u128(number + 1)).to_string();
        db.insert_workspace(
            &id,
            &CreateWorkspaceInput {
                path: path.display().to_string(),
                name: Some(name.into()),
            },
        )
        .unwrap();
        let mut memories = Vec::new();
        // Different cardinalities make a mistaken whole-database count visible.
        for slot in 0..number + 2 {
            let memory = MemoryId::from_uuid(Uuid::from_u128(10 + number * 100 + slot)).to_string();
            db.insert_memory(
                &memory,
                &CreateMemoryInput {
                    workspace_id: id.clone(),
                    level: "semantic".into(),
                    kind: "decision".into(),
                    content: format!("{name} durable storage choice number {slot}."),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.7,
                    importance: 0.8,
                    provenance_uri: Some("manual://shared-recovery".into()),
                    trust_class: "human_explicit".into(),
                    trust_subclass: None,
                    tags: vec![name.into(), format!("choice-{slot}")],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
            db.set_memory_typed_fields_json(
                &memory,
                Some(
                    &serde_json::json!({
                        "chosen": format!("{name}Store{slot}"),
                        "options": ["OtherStore", format!("{name}Store{slot}")],
                        "rationale": "Preserve transactional ownership.",
                        "revisit_by": "2030-01-01T00:00:00Z"
                    })
                    .to_string(),
                ),
            )
            .unwrap();
            memories.push(memory);
        }
        link(
            &db,
            &format!("lnk_{number:026}"),
            &memories[0],
            &memories[1],
        );
        let rule = RuleId::from_uuid(Uuid::from_u128(50 + number)).to_string();
        db.insert_procedural_rule(
            &rule,
            &CreateProceduralRuleInput {
                workspace_id: id.clone(),
                content: format!("Run cargo check in {name} before a release."),
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                trust_class: "human_explicit".into(),
                scope: "workspace".into(),
                scope_pattern: None,
                maturity: "validated".into(),
                protected: true,
                source_memory_ids: memories.clone(),
                tags: vec![name.into()],
            },
        )
        .unwrap();
        let session = SessionId::from_uuid(Uuid::from_u128(70 + number)).to_string();
        let excerpt = format!("{name} compilation completed successfully.");
        let hash = format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex());
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: id.clone(),
                cass_session_id: format!("shared-recovery-{number}"),
                source_path: None,
                agent_name: Some("fixture".into()),
                model: None,
                started_at: Some(TIME.into()),
                ended_at: Some(TIME.into()),
                message_count: 1,
                token_count: None,
                content_hash: hash.clone(),
                metadata_json: Some(r#"{"source":"cass","schema":"cass.session.v1"}"#.into()),
            },
        )
        .unwrap();
        db.insert_evidence_span(
            &EvidenceId::from_uuid(Uuid::from_u128(80 + number)).to_string(),
            &CreateEvidenceSpanInput {
                workspace_id: id.clone(),
                session_id: session,
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("shared-window-{number}"),
                span_kind: "message".into(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".into()),
                excerpt,
                content_hash: hash,
                metadata_json: Some(r#"{"source":"cass","schema":"cass.evidence_span.v1"}"#.into()),
                inherited_redaction_classes: vec![],
            },
        )
        .unwrap();
        audit(&db, &format!("audit_{number:026}"), Some(&id), name);
        recorder(&db, &format!("run_scoped_{number}"), Some(&id));
        fixture.workspaces.push(path);
        fixture.ids.push(id);
        fixture.memories.push(memories);
        fixture.rules.push(rule);
    }
    audit(
        &db,
        "audit_00000000000000000000000099",
        None,
        "SharedAuditHistory",
    );
    recorder(&db, "run_shared_history", None);
    db.close().unwrap();
    fixture
}

fn audit(db: &DbConnection, id: &str, workspace: Option<&str>, label: &str) {
    db.insert_audit(
        id,
        &CreateAuditInput {
            workspace_id: workspace.map(str::to_owned),
            actor: Some("fixture".into()),
            action: "workspace.inspect".into(),
            target_type: None,
            target_id: None,
            details: Some(serde_json::json!({"label": label}).to_string()),
        },
    )
    .unwrap();
}

fn recorder(db: &DbConnection, id: &str, workspace: Option<&str>) {
    db.insert_recorder_run(
        id,
        &CreateRecorderRunInput {
            workspace_id: workspace.map(str::to_owned),
            agent_id: "fixture".into(),
            session_id: None,
            source_type: "live".into(),
            source_id: None,
            status: "completed".into(),
            started_at: TIME.into(),
            ended_at: Some(TIME.into()),
            event_count: 0,
            redacted_count: 0,
            payload_bytes: 0,
            chain_complete: true,
        },
    )
    .unwrap();
}

fn link(db: &DbConnection, id: &str, left: &str, right: &str) {
    db.insert_memory_link(
        id,
        &CreateMemoryLinkInput {
            src_memory_id: left.into(),
            dst_memory_id: right.into(),
            relation: MemoryLinkRelation::Related,
            weight: 0.8,
            confidence: 0.9,
            directed: false,
            evidence_count: 1,
            last_reinforced_at: None,
            source: MemoryLinkSource::Human,
            created_by: None,
            metadata_json: None,
        },
    )
    .unwrap();
}

fn create(fixture: &Fixture, number: usize, dry_run: bool) -> BackupCreateReport {
    create_backup(&BackupCreateOptions {
        workspace_path: fixture.workspaces[number].clone(),
        database_path: Some(fixture.database.clone()),
        output_dir: None,
        label: None,
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run,
    })
    .unwrap()
}

fn inventory_count(report: &BackupCreateReport, table: &str) -> u64 {
    report
        .recovery_inventory
        .entries
        .iter()
        .find(|row| row.table == table)
        .unwrap()
        .row_count
}

#[test]
fn every_required_table_has_executable_source_ownership() {
    let fixture = fixture();
    let db = DbConnection::open_file(&fixture.database).unwrap();
    db.begin_read_snapshot().unwrap();
    for &(table, _, _) in REQUIRED_TABLES {
        assert!(scope(table).is_some(), "missing source ownership: {table}");
        for id in &fixture.ids {
            assert!(
                count_rows(&db, table, id).unwrap() <= db.count_table_rows(table).unwrap() as u64
            );
        }
    }
    assert_eq!(count_rows(&db, "memories", &fixture.ids[0]).unwrap(), 2);
    assert_eq!(count_rows(&db, "memories", &fixture.ids[1]).unwrap(), 3);
    assert_eq!(count_rows(&db, "memory_tags", &fixture.ids[0]).unwrap(), 4);
    assert_eq!(count_rows(&db, "memory_tags", &fixture.ids[1]).unwrap(), 6);
    assert_eq!(
        count_rows(&db, "rule_source_memories", &fixture.ids[0]).unwrap(),
        2
    );
    assert_eq!(
        count_rows(&db, "rule_source_memories", &fixture.ids[1]).unwrap(),
        3
    );
    assert_eq!(
        count_rows(&db, "recorder_runs", &fixture.ids[0]).unwrap(),
        2
    );
    assert_eq!(count_rows(&db, "audit_log", &fixture.ids[0]).unwrap(), 2);
    assert!(count_rows(&db, "curation_ttl_policies", &fixture.ids[0]).unwrap() > 0);
    assert_eq!(count_rows(&db, "memories", "' OR 1=1 --").unwrap(), 0);
    assert!(count_rows(&db, "memories; SELECT 1", &fixture.ids[0]).is_err());
    db.commit_read_snapshot().unwrap();
    db.close().unwrap();
}

#[test]
fn shared_database_workspaces_restore_independently_with_typed_and_learned_history() {
    let fixture = fixture();
    for number in 0..2 {
        let other = 1 - number;
        let report = create(&fixture, number, false);
        assert_eq!(report.status, "completed", "{:?}", report.degraded);
        assert!(report.recovery_inventory.snapshot_coverage_complete);
        assert_eq!(inventory_count(&report, "workspaces"), 1);
        assert_eq!(
            inventory_count(&report, "memories"),
            fixture.memories[number].len() as u64
        );
        assert_eq!(
            inventory_count(&report, "rule_source_memories"),
            fixture.memories[number].len() as u64
        );
        assert_eq!(inventory_count(&report, "memory_links"), 1);
        assert_eq!(inventory_count(&report, "sessions"), 1);
        assert_eq!(inventory_count(&report, "evidence_spans"), 1);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&report.manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest["recoveryInventory"]["requiredRowScope"],
            "selected_workspace_and_shared_history"
        );
        assert_eq!(
            verify_backup(&BackupVerifyOptions {
                workspace_path: fixture.workspaces[number].clone(),
                backup_path: report.backup_path.clone().into(),
            })
            .unwrap()
            .status,
            "verified"
        );
        let restored = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: fixture.workspaces[number].clone(),
            backup_path: report.backup_path.into(),
            side_path: fixture
                .root
                .path()
                .canonicalize()
                .unwrap()
                .join(format!("restored-{number}")),
            restore_graph_cache: false,
            dry_run: false,
        })
        .unwrap();
        let db = DbConnection::open_file(&restored.restored_database_path).unwrap();
        assert_eq!(db.count_table_rows("workspaces").unwrap(), 1);
        assert!(db.get_workspace(&fixture.ids[other]).unwrap().is_none());
        assert_eq!(
            db.list_memories(&fixture.ids[number], None, true)
                .unwrap()
                .len(),
            fixture.memories[number].len()
        );
        for memory in &fixture.memories[number] {
            assert!(db.get_memory(memory).unwrap().is_some());
            assert!(
                db.get_memory_typed_fields_json(memory)
                    .unwrap()
                    .unwrap()
                    .contains("Store")
            );
            assert_eq!(db.get_memory_tags(memory).unwrap().len(), 2);
        }
        for memory in &fixture.memories[other] {
            assert!(db.get_memory(memory).unwrap().is_none());
        }
        assert_eq!(
            db.list_procedural_rules(&fixture.ids[number], None, None, true)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.get_rule_source_memory_ids(&fixture.rules[number])
                .unwrap(),
            fixture.memories[number]
        );
        assert_eq!(db.count_table_rows("memory_links").unwrap(), 1);
        assert_eq!(db.count_table_rows("sessions").unwrap(), 1);
        assert_eq!(db.count_table_rows("evidence_spans").unwrap(), 1);
        assert_eq!(db.count_table_rows("recorder_runs").unwrap(), 2);
        assert!(db.get_recorder_run("run_shared_history").unwrap().is_some());
        let audits = db.list_audit_entries(None, None).unwrap();
        assert!(audits.iter().any(|row| {
            row.workspace_id.is_none()
                && row
                    .details
                    .as_deref()
                    .is_some_and(|raw| raw.contains("SharedAuditHistory"))
        }));
        assert!(
            !audits
                .iter()
                .any(|row| row.workspace_id.as_deref() == Some(&fixture.ids[other]))
        );
        assert!(
            Path::new(&restored.restored_database_path)
                .parent()
                .unwrap()
                .join("index/meta.json")
                .is_file()
        );
        db.close().unwrap();
    }
    let db = DbConnection::open_file(&fixture.database).unwrap();
    assert_eq!(db.count_table_rows("workspaces").unwrap(), 2);
    assert_eq!(db.count_table_rows("memories").unwrap(), 5);
    db.close().unwrap();
}

#[test]
fn scoped_dry_run_does_not_initialize_keys_or_change_shared_database() {
    let fixture = fixture();
    let before = std::fs::read(&fixture.database).unwrap();
    for number in 0..2 {
        let report = create(&fixture, number, true);
        assert!(report.recovery_inventory.snapshot_coverage_complete);
        assert_eq!(inventory_count(&report, "workspaces"), 1);
        assert!(!Path::new(&report.backup_path).exists());
        assert_eq!(
            std::fs::read_dir(fixture.workspaces[number].join(".ee"))
                .unwrap()
                .count(),
            0
        );
    }
    assert_eq!(std::fs::read(&fixture.database).unwrap(), before);
}

#[test]
fn cross_workspace_links_remain_uncovered_instead_of_leaking_or_disappearing() {
    let fixture = fixture();
    let db = DbConnection::open_file(&fixture.database).unwrap();
    link(
        &db,
        "lnk_00000000000000000000000099",
        &fixture.memories[0][0],
        &fixture.memories[1][0],
    );
    db.close().unwrap();
    for number in 0..2 {
        let report = create(&fixture, number, false);
        assert_eq!(report.status, "partial");
        let row = report
            .recovery_inventory
            .entries
            .iter()
            .find(|row| row.table == "memory_links")
            .unwrap();
        assert_eq!(row.row_count, 2);
        assert!(!row.snapshot_covered);
        assert_eq!(report.link_count, 1);
        let records =
            std::fs::read_to_string(Path::new(&report.backup_path).join("records.jsonl")).unwrap();
        assert!(!records.contains(&fixture.memories[1 - number][0]));
        let side = fixture
            .root
            .path()
            .canonicalize()
            .unwrap()
            .join(format!("refused-{number}"));
        assert!(
            restore_backup_to_side_path(&BackupRestoreOptions {
                workspace_path: fixture.workspaces[number].clone(),
                backup_path: report.backup_path.into(),
                side_path: side.clone(),
                restore_graph_cache: false,
                dry_run: false,
            })
            .is_err()
        );
        assert!(!side.join(".ee").exists());
    }
}
