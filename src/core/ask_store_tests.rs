use super::*;
use crate::db::{CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkSource};
use crate::models::WorkspaceId;

fn fixture(count: usize) -> (tempfile::TempDir, DbConnection, Vec<String>) {
    let root = tempfile::tempdir().expect("temporary real store");
    let connection = DbConnection::open_file(&root.path().join("ask.db")).expect("open store");
    connection.migrate().expect("migrate real schema");
    let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([31; 16])).to_string();
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: Some("ask storage regression".to_owned()),
            },
        )
        .expect("workspace");
    let ids: Vec<_> = (0..count).map(|index| format!("mem_{index:026}")).collect();
    for (index, id) in ids.iter().enumerate() {
        connection
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    content: format!("Deployment setting token_{index}."),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some(format!("manual://ask-storage/{index}")),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("memory");
    }
    (root, connection, ids)
}

fn link(connection: &DbConnection, number: usize, source: &str, target: &str, relation: MemoryLinkRelation) {
    connection
        .insert_memory_link(
            &format!("link_{number:026}"),
            &CreateMemoryLinkInput {
                src_memory_id: source.to_owned(),
                dst_memory_id: target.to_owned(),
                relation,
                weight: 1.0,
                confidence: 0.9,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: Some("ask-storage-test".to_owned()),
                metadata_json: None,
            },
        )
        .expect("stored link");
}

#[test]
fn late_and_cross_batch_edges_are_loaded_once_in_stable_order() {
    let (_root, connection, ids) = fixture(LINK_QUERY_BATCH_SIZE * 2 + 7);
    link(&connection, 1, &ids[0], &ids[LINK_QUERY_BATCH_SIZE], MemoryLinkRelation::Contradicts);
    link(&connection, 2, &ids[LINK_QUERY_BATCH_SIZE * 2], &ids[LINK_QUERY_BATCH_SIZE * 2 + 1], MemoryLinkRelation::Contradicts);
    link(&connection, 3, &ids[1], &ids[2], MemoryLinkRelation::Related);
    let mut scope: Vec<_> = ids.iter().map(String::as_str).collect();
    let before = connection.list_audit_entries(None, None).expect("audits");
    let actual = load_scoped_contradictions(&connection, &scope).expect("complete link scan");
    assert_eq!(actual.len(), 2);
    assert_eq!(actual[0].id, format!("link_{:026}", 1));
    assert_eq!(actual[1].id, format!("link_{:026}", 2));
    assert_eq!(actual[1].src_memory_id, ids[LINK_QUERY_BATCH_SIZE * 2]);
    assert_eq!(actual[1].confidence, 0.9);
    assert_eq!(actual[1].source, "agent");
    scope.reverse();
    scope.push(ids[0].as_str());
    let reversed = load_scoped_contradictions(&connection, &scope).expect("permuted scope");
    assert_eq!(actual.iter().map(|link| &link.id).collect::<Vec<_>>(), reversed.iter().map(|link| &link.id).collect::<Vec<_>>());
    assert_eq!(before.len(), connection.list_audit_entries(None, None).expect("audits after reads").len());
}

#[test]
fn an_incident_link_cannot_reintroduce_an_excluded_memory() {
    let (_root, connection, ids) = fixture(3);
    link(&connection, 1, &ids[0], &ids[1], MemoryLinkRelation::Contradicts);
    link(&connection, 2, &ids[0], &ids[2], MemoryLinkRelation::Contradicts);
    let links = load_scoped_contradictions(&connection, &[&ids[0], &ids[1]]).expect("scoped links");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].dst_memory_id, ids[1]);
}

#[test]
fn empty_scope_needs_no_link_table_but_missing_storage_for_a_real_scope_fails() {
    let root = tempfile::tempdir().expect("temporary database");
    let connection = DbConnection::open_file(&root.path().join("unmigrated.db")).expect("open");
    assert!(load_scoped_contradictions(&connection, &[]).expect("empty scope").is_empty());
    assert!(matches!(
        load_scoped_contradictions(&connection, &["mem_00000000000000000000000001"]),
        Err(DomainError::Storage { .. })
    ));
}
