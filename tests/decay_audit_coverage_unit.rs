#![allow(clippy::expect_used)]

use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection, audit_actions};
use ee::policy::MemoryDecayAction;
use ee::steward::{ScoreDecayJobOptions, run_score_decay_job};

const WORKSPACE_ID: &str = "wsp_decayaudit0000000000000000";
const DEMOTE_ID: &str = "mem_decayauditdemote0000000000";
const TOMBSTONE_ID: &str = "mem_decayaudittomb000000000000";

fn open_db() -> DbConnection {
    let connection = DbConnection::open_memory().expect("memory database opens");
    connection.migrate().expect("migrations apply");
    connection
        .insert_workspace(
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: "/tmp/ee-decay-audit".to_owned(),
                name: Some("decay-audit".to_owned()),
            },
        )
        .expect("workspace inserts");
    connection
}

fn insert_memory(
    connection: &DbConnection,
    memory_id: &str,
    level: &str,
    kind: &str,
    confidence: f32,
    utility: f32,
) {
    connection
        .insert_memory(
            memory_id,
            &CreateMemoryInput {
                workspace_id: WORKSPACE_ID.to_owned(),
                level: level.to_owned(),
                kind: kind.to_owned(),
                content: format!("decay audit fixture {memory_id}"),
                workflow_id: None,
                confidence,
                utility,
                importance: 0.8,
                provenance_uri: Some("test://decay-audit".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: vec!["decay".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("memory inserts");
    connection
        .execute_raw(&format!(
            "UPDATE memories SET created_at = '2026-01-01T00:00:00Z', updated_at = '2026-01-01T00:00:00Z' WHERE id = '{memory_id}'"
        ))
        .expect("timestamp updates");
}

#[test]
fn decay_demote_and_tombstone_write_audit_details() {
    let connection = open_db();
    insert_memory(&connection, DEMOTE_ID, "procedural", "rule", 0.8, 0.5);
    insert_memory(&connection, TOMBSTONE_ID, "semantic", "fact", 0.2, 0.2);

    let mut options = ScoreDecayJobOptions::new(WORKSPACE_ID);
    options.as_of = Some("2030-01-01T00:00:00Z".to_owned());
    options.include_decay_actions = true;
    options.actor = Some("decay-audit-test".to_owned());
    let report = run_score_decay_job(&connection, &options).expect("decay job succeeds");

    assert_eq!(report.demoted_count, 1);
    assert_eq!(report.tombstoned_count, 1);
    assert!(report.changes.iter().any(|change| {
        change.memory_id == DEMOTE_ID && change.decay_action == MemoryDecayAction::Demote
    }));
    assert!(report.changes.iter().any(|change| {
        change.memory_id == TOMBSTONE_ID && change.decay_action == MemoryDecayAction::Tombstone
    }));

    let demote_audits = connection
        .list_audit_by_target("memory", DEMOTE_ID, None)
        .expect("demote audits query succeeds");
    assert!(demote_audits.iter().any(|entry| {
        entry.action == audit_actions::MEMORY_DECAY_DEMOTE
            && entry
                .details
                .as_deref()
                .is_some_and(|details| details.contains("\"previousLevel\":\"procedural\""))
    }));

    let tombstone_audits = connection
        .list_audit_by_target("memory", TOMBSTONE_ID, None)
        .expect("tombstone audits query succeeds");
    assert!(tombstone_audits.iter().any(|entry| {
        entry.action == audit_actions::MEMORY_DECAY_TOMBSTONE
            && entry
                .details
                .as_deref()
                .is_some_and(|details| details.contains("\"reason\":\"auto_forgetting\""))
    }));
}
