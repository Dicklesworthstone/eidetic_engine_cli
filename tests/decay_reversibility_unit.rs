#![allow(clippy::expect_used)]

use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};

const WORKSPACE_ID: &str = "wsp_decayrev000000000000000001";
const MEMORY_ID: &str = "mem_decayrev000000000000000001";

fn open_db() -> DbConnection {
    let connection = DbConnection::open_memory().expect("memory database opens");
    connection.migrate().expect("migrations apply");
    connection
        .insert_workspace(
            WORKSPACE_ID,
            &CreateWorkspaceInput {
                path: "/tmp/ee-decay-reversibility".to_owned(),
                name: Some("decay-reversibility".to_owned()),
            },
        )
        .expect("workspace inserts");
    connection
}

fn insert_memory(connection: &DbConnection) {
    connection
        .insert_memory(
            MEMORY_ID,
            &CreateMemoryInput {
                workspace_id: WORKSPACE_ID.to_owned(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: "obsolete fact restored by untombstone".to_owned(),
                workflow_id: None,
                confidence: 0.2,
                utility: 0.2,
                importance: 0.5,
                provenance_uri: Some("test://decay-reversibility".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: vec!["decay".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("memory inserts");
}

#[test]
fn auto_forgetting_is_reversible_tombstoning() {
    let connection = open_db();
    insert_memory(&connection);

    let tombstone_audit = connection
        .tombstone_memory_decay_audited(
            MEMORY_ID,
            WORKSPACE_ID,
            Some("decay-test"),
            r#"{"reason":"auto_forgetting"}"#,
        )
        .expect("decay tombstone succeeds");
    assert!(tombstone_audit.is_some());
    assert!(
        connection
            .get_memory(MEMORY_ID)
            .expect("memory query succeeds")
            .expect("memory exists")
            .tombstoned_at
            .is_some()
    );

    let restored_at = "2030-01-01T00:00:00Z";
    let untombstone_audit = connection
        .untombstone_memory_audited(
            MEMORY_ID,
            WORKSPACE_ID,
            Some("curate-test"),
            restored_at,
            r#"{"reason":"restore after decay"}"#,
        )
        .expect("untombstone succeeds");
    assert!(untombstone_audit.is_some());

    let memory = connection
        .get_memory(MEMORY_ID)
        .expect("memory query succeeds")
        .expect("memory exists");
    assert!(memory.tombstoned_at.is_none());
    assert_eq!(memory.updated_at, restored_at);
}
